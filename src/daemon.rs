use crate::{
    auth::Resolver,
    boottime,
    config::{Backend, Config},
    keyboard::Keyboard,
    layout::Layout,
    memory::Secret,
    socket::{Listener, Socket},
    state::State,
    Result, CHUNK,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use zeroize::Zeroize;
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
const MAX_CLIENTS: usize = 32;
const AUTH_TIMEOUT: Duration = Duration::from_secs(2);
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAGIC: &[u8; 8] = b"VISKA\0\x01\0";
pub const COMMIT: &[u8] = b"COMMIT1";

#[derive(PartialEq)]
enum Phase {
    Control,
    Auth(u8),
    Header,
    Data(usize),
    Commit,
    End,
    Typing,
    Reply,
}
struct Client {
    id: u64,
    socket: Socket,
    store: bool,
    uid: libc::uid_t,
    phase: Phase,
    created: Duration,
    activity: Duration,
    staging: Option<Secret>,
    reply: Vec<u8>,
}
impl Client {
    fn reply(&mut self, message: &'static str) {
        self.staging = None;
        self.reply = message.as_bytes().to_vec();
        self.phase = Phase::Reply;
        self.activity = boottime();
    }
    fn error(&mut self, message: &'static str) {
        self.staging = None;
        self.reply = serde_json::to_vec(&serde_json::json!({"ok":false,"error":message})).unwrap();
        self.phase = Phase::Reply;
        self.activity = boottime();
    }
}
const OK: &str = "{\"ok\":true}";
struct Injection {
    owner: u64,
    secret: Secret,
    cursor: usize,
    started: Duration,
    next_event: Duration,
}
fn cancel(
    injection: &mut Option<Injection>,
    keyboard: &mut Keyboard,
    clients: &mut [Client],
    error: &'static str,
) {
    if let Some(inj) = injection.take() {
        let _ = keyboard.release();
        if let Some(owner) = clients.iter_mut().find(|c| c.id == inj.owner) {
            owner.error(error);
        }
        // Dropping injection zeroizes the consumed secret, including on failure.
    }
}
fn notify_ready() -> Result<()> {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    use std::os::{
        linux::net::SocketAddrExt,
        unix::{
            ffi::OsStrExt,
            net::{SocketAddr, UnixDatagram},
        },
    };
    let bytes = path.as_bytes();
    let addr = if bytes.first() == Some(&b'@') {
        SocketAddr::from_abstract_name(&bytes[1..])
    } else {
        SocketAddr::from_pathname(path)
    }
    .map_err(|_| "invalid notification socket")?;
    UnixDatagram::unbound()
        .and_then(|s| s.send_to_addr(b"READY=1", &addr))
        .map_err(|_| "cannot notify systemd")?;
    Ok(())
}

pub fn run(config: Config) -> Result<()> {
    STOP.store(false, Ordering::Relaxed);
    unsafe {
        libc::umask(0o117); // 0660 socket creation; protected parent is required.
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let layout = Layout::new(&config.keyboard_layout)?;
    let mut keyboard = Keyboard::new(config.backend, &layout)?;
    let mut scratch = Secret::new(CHUNK + 32)?;
    // Prove that the maximum pending + staging allocation fits before readiness.
    let reserve_a = Secret::new(config.max_payload_bytes)?;
    let reserve_b = Secret::new(config.max_payload_bytes)?;
    drop((reserve_a, reserve_b));
    let submission = Listener::bind(&config.submission_socket)?;
    let control = Listener::bind(&config.control_socket)?;
    let resolver = Resolver::new();
    let mut state = State::new();
    let mut clients: Vec<Client> = Vec::new();
    let mut next_id = 1;
    let mut injection: Option<Injection> = None;
    notify_ready()?;
    eprintln!(
        "viskad: ready ({})",
        if config.backend == Backend::Mock {
            "mock"
        } else {
            "uinput"
        }
    );
    while !STOP.load(Ordering::Relaxed) {
        let now = boottime();
        state.expire(now);
        // Bounded work per pass prevents connection floods starving timers.
        for (listener, store) in [(&submission, true), (&control, false)] {
            for _ in 0..8 {
                let Some(socket) = listener.accept()? else {
                    break;
                };
                if clients.len() >= MAX_CLIENTS {
                    let _ = socket.send(b"{\"ok\":false,\"error\":\"busy\"}");
                    continue;
                }
                let Ok(uid) = socket.peer_uid() else {
                    continue;
                };
                let busy = store
                    && (injection.is_some()
                        || clients.iter().any(|c| c.store && c.phase != Phase::Reply));
                let mut client = Client {
                    id: next_id,
                    socket,
                    store,
                    uid,
                    phase: if store {
                        Phase::Auth(b's')
                    } else {
                        Phase::Control
                    },
                    created: now,
                    activity: now,
                    staging: None,
                    reply: Vec::new(),
                };
                next_id += 1;
                if busy {
                    client.error("busy");
                } else if store
                    && !resolver.submit(client.id, uid, config.authorization.store_users.clone())
                {
                    client.error("authorization unavailable");
                }
                clients.push(client);
            }
        }
        // Only metadata goes through the NSS worker; no sensitive buffers cross threads.
        while let Ok((id, allowed)) = resolver.rx.try_recv() {
            let Some(index) = clients
                .iter()
                .position(|c| c.id == id && matches!(c.phase, Phase::Auth(_)))
            else {
                continue;
            };
            if now.saturating_sub(clients[index].activity) >= AUTH_TIMEOUT {
                clients[index].error("authorization timeout");
                continue;
            }
            if !allowed {
                clients[index].error("not authorized");
                continue;
            }
            let Phase::Auth(command) = clients[index].phase else {
                continue;
            };
            match command {
                b's' => {
                    if injection.is_some() {
                        clients[index].error("busy");
                    } else {
                        clients[index].phase = Phase::Header;
                        clients[index].activity = now;
                    }
                }
                b't' => {
                    if injection.is_some() {
                        clients[index].error("busy");
                    } else if !keyboard.ready() {
                        clients[index].error("keyboard unavailable");
                    } else if let Some(secret) = state.take(boottime()) {
                        let owner = clients[index].id;
                        clients[index].phase = Phase::Typing;
                        injection = Some(Injection {
                            owner,
                            secret,
                            cursor: 0,
                            started: now,
                            next_event: now,
                        });
                        // A store already in flight cannot install during injection.
                        for c in clients
                            .iter_mut()
                            .filter(|c| c.store && c.phase != Phase::Reply)
                        {
                            c.error("busy");
                        }
                    } else {
                        clients[index].error("no pending secret");
                    }
                }
                b'c' => {
                    state.clear();
                    cancel(
                        &mut injection,
                        &mut keyboard,
                        &mut clients,
                        "injection cancelled; partial typing possible",
                    );
                    for c in clients
                        .iter_mut()
                        .filter(|c| c.store && c.phase != Phase::Reply)
                    {
                        c.error("submission cancelled");
                    }
                    if keyboard.ready() {
                        clients[index].reply(OK);
                    } else {
                        clients[index].error("key release failed; keyboard disabled");
                    }
                }
                b'?' => {
                    let remaining = state.remaining(now);
                    let mode = if injection.is_some() {
                        "injecting"
                    } else if remaining.is_some() {
                        "pending"
                    } else {
                        "empty"
                    };
                    clients[index].reply = serde_json::to_vec(&serde_json::json!({"ok":true,"state":mode,
                        "ttl_remaining_ms":remaining.map(|r| r.as_millis() as u64), "backend_ready":keyboard.ready()})).unwrap();
                    clients[index].phase = Phase::Reply;
                    clients[index].activity = now;
                }
                _ => clients[index].error("invalid control command"),
            }
        }
        let mut disconnected_owner = false;
        clients.retain_mut(|c| {
            if c.phase == Phase::Reply {
                return match c.socket.send(&c.reply) {
                    Ok(true) | Err(_) => false,
                    Ok(false) => now.saturating_sub(c.activity) < IDLE_TIMEOUT,
                };
            }
            if let Phase::Auth(_) = c.phase {
                if now.saturating_sub(c.activity) >= AUTH_TIMEOUT {
                    c.error("authorization timeout");
                }
                return true;
            }
            if c.phase != Phase::Typing
                && (now.saturating_sub(c.activity) >= IDLE_TIMEOUT
                    || (c.store && now.saturating_sub(c.created) >= TRANSFER_TIMEOUT))
            {
                c.error("request timeout");
                return true;
            }
            scratch.clear();
            let packet = c.socket.recv(scratch.spare());
            let n = match packet {
                Ok(None) => return true,
                Err("peer disconnected") if c.phase == Phase::End => {
                    if let Some(secret) = c.staging.take() {
                        if injection.is_some() {
                            c.error("busy");
                        } else if let Err(error) = layout.validate(secret.bytes()) {
                            c.error(error);
                        } else {
                            state.install(
                                secret,
                                boottime(),
                                Duration::from_secs(config.ttl_seconds),
                            );
                            c.reply(OK);
                        }
                    }
                    return true;
                }
                Err(error) => {
                    if c.phase == Phase::Typing {
                        disconnected_owner = true;
                        return false;
                    }
                    c.error(error);
                    return true;
                }
                Ok(Some(n)) => n,
            };
            let _ = scratch.set_len(n);
            c.activity = now;
            let data = scratch.bytes();
            match c.phase {
                Phase::Control => {
                    if n != 1 {
                        c.error("invalid control command");
                    } else {
                        let names = match data[0] {
                            b't' => Some(config.authorization.type_users.clone()),
                            b'c' => Some(config.authorization.clear_users.clone()),
                            b'?' => Some(config.authorization.status_users.clone()),
                            _ => None,
                        };
                        if let Some(names) = names {
                            c.phase = Phase::Auth(data[0]);
                            if !resolver.submit(c.id, c.uid, names) {
                                c.error("authorization unavailable");
                            }
                        } else {
                            c.error("invalid control command");
                        }
                    }
                }
                Phase::Header => {
                    if n != 16 || &data[..8] != MAGIC {
                        c.error("invalid submission header");
                    } else {
                        let total = u64::from_le_bytes(data[8..16].try_into().unwrap());
                        if total > config.max_payload_bytes as u64 {
                            c.error("payload limit exceeded");
                        } else {
                            match Secret::new(config.max_payload_bytes) {
                                Ok(s) => {
                                    c.staging = Some(s);
                                    c.phase = if total == 0 {
                                        Phase::Commit
                                    } else {
                                        Phase::Data(total as usize)
                                    };
                                }
                                Err(e) => c.error(e),
                            }
                        }
                    }
                }
                Phase::Data(total) => {
                    let staging = c.staging.as_mut().unwrap();
                    if n > CHUNK || n > total - staging.bytes().len() {
                        c.error("invalid data packet length");
                    } else if let Err(e) = staging.append(data) {
                        c.error(e);
                    } else if staging.bytes().len() == total {
                        c.phase = Phase::Commit;
                    }
                }
                Phase::Commit => {
                    if data == COMMIT {
                        c.phase = Phase::End;
                    } else {
                        c.error("invalid completion record");
                    }
                }
                Phase::End => c.error("excess submission data"),
                Phase::Typing => {
                    disconnected_owner = true;
                    c.error("unexpected data during injection");
                }
                _ => (),
            }
            scratch.clear();
            true
        });
        // recv may have filled scratch before a truncation/error: always wipe it.
        scratch.clear();
        if disconnected_owner {
            cancel(
                &mut injection,
                &mut keyboard,
                &mut clients,
                "trigger disconnected; partial typing possible",
            );
        }
        if let Some(inj) = injection.as_mut() {
            let result = if now.saturating_sub(inj.started) >= Duration::from_secs(300) {
                Err("injection timeout; partial typing possible")
            } else if now < inj.next_event {
                Ok(())
            } else {
                // IPC readiness/flooding must not accelerate keyboard output.
                inj.next_event = now + Duration::from_millis(1);
                (|| {
                    if keyboard.idle() && inj.cursor < inj.secret.bytes().len() {
                        // Full UTF-8 validation occurred before installation;
                        // bytes are immutable and cursor advances by char widths.
                        // Avoid repeatedly scanning the entire remaining secret.
                        let text = unsafe {
                            std::str::from_utf8_unchecked(&inj.secret.bytes()[inj.cursor..])
                        };
                        let mut ch = text.chars().next().ok_or("invalid injection buffer")?;
                        let stroke = layout.lookup(ch);
                        inj.cursor += ch.len_utf8();
                        ch.zeroize();
                        keyboard.prepare(stroke?)?;
                    }
                    keyboard.tick()?;
                    Ok(())
                })()
            };
            if let Err(error) = result {
                cancel(&mut injection, &mut keyboard, &mut clients, error);
            } else if injection
                .as_ref()
                .is_some_and(|i| i.cursor == i.secret.bytes().len() && keyboard.idle())
            {
                let done = injection.take().unwrap();
                if let Some(c) = clients.iter_mut().find(|c| c.id == done.owner) {
                    c.reply(OK);
                }
            }
        }
        // Poll readiness for IPC, but cap sleeps for proactive expiry/NSS results.
        let mut fds = Vec::with_capacity(clients.len() + 2);
        for fd in [submission.fd(), control.fd()] {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        for c in &clients {
            // Auth packets must remain unread until the credentials are authorized.
            if matches!(c.phase, Phase::Auth(_)) {
                continue;
            }
            fds.push(libc::pollfd {
                fd: c.socket.fd(),
                events: if c.phase == Phase::Reply {
                    libc::POLLOUT
                } else {
                    libc::POLLIN
                },
                revents: 0,
            });
        }
        unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as _,
                if injection.is_some() { 1 } else { 10 },
            );
        }
    }
    cancel(
        &mut injection,
        &mut keyboard,
        &mut clients,
        "service stopping; partial typing possible",
    );
    state.clear();
    eprintln!("viskad: stopped");
    Ok(())
}
