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
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use zeroize::Zeroize;
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
pub const COMMIT: &[u8] = b"COMMIT2";
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub operation: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub sync: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<usize>,
}
fn default_channel() -> String {
    "default".into()
}
impl Request {
    pub fn validate(&self) -> Result<()> {
        if self.version != 2 {
            return Err("incompatible protocol version; use matching binaries");
        }
        let n = self.channel.as_bytes();
        if n.is_empty()
            || n.len() > 64
            || !n[0].is_ascii_alphanumeric()
            || !n
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        {
            return Err("invalid channel name");
        }
        if !matches!(
            self.operation.as_str(),
            "store" | "type" | "output" | "clear" | "status"
        ) || (self.sync && !matches!(self.operation.as_str(), "type" | "output"))
            || ((self.operation == "store") != self.length.is_some())
        {
            return Err("invalid request");
        }
        Ok(())
    }
    fn users<'a>(&self, c: &'a Config) -> &'a [String] {
        match self.operation.as_str() {
            "store" => &c.authorization.store_users,
            "type" => &c.authorization.type_users,
            "output" => &c.authorization.output_users,
            "clear" => &c.authorization.clear_users,
            _ => &c.authorization.status_users,
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Header,
    Auth,
    Ready,
    Wait,
    Reauth,
    DeliverReady,
    Data,
    Commit,
    End,
    Typing,
    OutHeader,
    OutData,
    OutCommit,
    OutAck,
    Reply,
    Dead,
}
struct Client {
    id: u64,
    socket: Socket,
    store: bool,
    uid: u32,
    phase: Phase,
    request: Option<Request>,
    secret: Option<Secret>,
    cursor: usize,
    created: Duration,
    activity: Duration,
    started: Duration,
    next_event: Duration,
    reply: Vec<u8>,
}
impl Client {
    fn json(&mut self, v: serde_json::Value) {
        self.secret = None;
        self.reply = serde_json::to_vec(&v).unwrap();
        self.phase = Phase::Reply;
        self.activity = boottime();
    }
    fn ok(&mut self) {
        self.json(serde_json::json!({"ok":true}));
    }
    fn error(&mut self, e: &'static str) {
        self.json(serde_json::json!({"ok":false,"error":e}));
    }
    fn channel(&self) -> &str {
        self.request.as_ref().map_or("", |r| r.channel.as_str())
    }
    fn consumer(&self) -> bool {
        matches!(
            self.phase,
            Phase::Wait
                | Phase::Reauth
                | Phase::DeliverReady
                | Phase::Typing
                | Phase::OutHeader
                | Phase::OutData
                | Phase::OutCommit
                | Phase::OutAck
        )
    }
    fn active(&self) -> bool {
        matches!(
            self.phase,
            Phase::Typing | Phase::OutHeader | Phase::OutData | Phase::OutCommit | Phase::OutAck
        )
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
        libc::umask(0o117);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let layout = Layout::new(&config.keyboard_layout)?;
    let mut keyboard = Keyboard::new(config.backend, &layout)?;
    let mut scratch = Secret::new(CHUNK + 32)?;
    // Preflight the configured worst case, then allocate secret slots on demand.
    let mut reserves = Vec::new();
    for _ in 0..2 * config.max_channels {
        reserves.push(Secret::new(config.max_payload_bytes)?);
    }
    drop(reserves);
    let submit = Listener::bind(&config.submission_socket)?;
    let control = Listener::bind(&config.control_socket)?;
    let resolver = Resolver::new();
    let mut channels: BTreeMap<String, State> = BTreeMap::new();
    let mut clients: Vec<Client> = Vec::new();
    let mut id = 0;
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
        for s in channels.values_mut() {
            s.expire(now);
        }
        channels.retain(|name, s| {
            s.pending().is_some()
                || clients
                    .iter()
                    .any(|c| c.channel() == name && !matches!(c.phase, Phase::Dead | Phase::Reply))
        });
        for (listener, store) in [(&submit, true), (&control, false)] {
            for _ in 0..8 {
                let Some(socket) = listener.accept()? else {
                    break;
                };
                if clients.len() >= 32 {
                    let _ = socket.send(b"{\"ok\":false,\"error\":\"busy\"}");
                    continue;
                }
                let Ok(uid) = socket.peer_uid() else { continue };
                id += 1;
                clients.push(Client {
                    id,
                    socket,
                    store,
                    uid,
                    phase: Phase::Header,
                    request: None,
                    secret: None,
                    cursor: 0,
                    created: now,
                    activity: now,
                    started: now,
                    next_event: now,
                    reply: Vec::new(),
                });
            }
        }
        while let Ok((id, allowed)) = resolver.rx.try_recv() {
            if let Some(c) = clients
                .iter_mut()
                .find(|c| c.id == id && matches!(c.phase, Phase::Auth | Phase::Reauth))
            {
                if now.saturating_sub(c.activity) >= Duration::from_secs(2) {
                    c.error("authorization timeout");
                } else if !allowed {
                    c.error("not authorized");
                } else {
                    c.phase = if c.phase == Phase::Auth {
                        Phase::Ready
                    } else {
                        Phase::DeliverReady
                    };
                }
            }
        }
        let count = clients.len();
        for _ in 0..count {
            let mut c = clients.remove(0);
            let result: Result<()> = (|| {
                if matches!(c.phase, Phase::Auth | Phase::Reauth) {
                    if now.saturating_sub(c.activity) >= Duration::from_secs(2) {
                        return Err("authorization timeout");
                    }
                    return Ok(());
                }
                if c.phase == Phase::Reply {
                    if !matches!(c.socket.send(&c.reply), Ok(false))
                        || now.saturating_sub(c.activity) > Duration::from_secs(5)
                    {
                        c.phase = Phase::Dead;
                    }
                    return Ok(());
                }
                if c.phase == Phase::Ready {
                    let r = c.request.as_ref().unwrap().clone();
                    if matches!(r.operation.as_str(), "status" | "clear") {
                        if r.operation == "clear" {
                            let mut released = true;
                            if let Some(s) = channels.get_mut(&r.channel) {
                                s.clear();
                            }
                            for other in clients.iter_mut().filter(|o| {
                                o.channel() == r.channel
                                    && !matches!(o.phase, Phase::Reply | Phase::Dead)
                            }) {
                                if other.phase == Phase::Typing {
                                    released &= keyboard.release().is_ok();
                                }
                                other.error("channel cleared; delivery cancelled");
                            }
                            if released {
                                c.ok();
                            } else {
                                c.error("keyboard unavailable");
                            }
                        } else {
                            let remaining = channels
                                .get_mut(&r.channel)
                                .and_then(|s| s.remaining(boottime()));
                            let active = clients
                                .iter()
                                .find(|o| o.channel() == r.channel && o.consumer());
                            let state = match active {
                                Some(o) if o.phase == Phase::Typing => "injecting",
                                Some(o) if o.active() => "outputting",
                                _ if remaining.is_some() => "pending",
                                Some(_) => "waiting",
                                _ => "empty",
                            };
                            c.json(serde_json::json!({"ok":true,"state":state,"ttl_remaining_ms":remaining.map(|d|d.as_millis() as u64),"backend_ready":keyboard.ready()}));
                        }
                        return Ok(());
                    }
                    if !channels.contains_key(&r.channel) {
                        if channels.len() >= config.max_channels {
                            return Err("channel limit reached");
                        }
                        channels.insert(r.channel.clone(), State::new());
                    }
                    if r.operation == "store" {
                        if clients.iter().any(|o| {
                            o.channel() == r.channel
                                && (o.active()
                                    || matches!(o.phase, Phase::Data | Phase::Commit | Phase::End))
                        }) {
                            return Err("busy");
                        }
                        if r.length.unwrap() > config.max_payload_bytes {
                            return Err("payload limit exceeded");
                        }
                        c.secret = Some(Secret::new(config.max_payload_bytes)?);
                        c.phase = if r.length == Some(0) {
                            Phase::Commit
                        } else {
                            Phase::Data
                        };
                    } else {
                        if clients
                            .iter()
                            .any(|o| o.channel() == r.channel && o.consumer())
                        {
                            return Err("busy");
                        }
                        if r.operation == "type" && !keyboard.ready() {
                            return Err("keyboard unavailable");
                        }
                        if !r.sync && channels[&r.channel].pending().is_none() {
                            return Err("no pending secret");
                        }
                        if !r.sync
                            && r.operation == "type"
                            && clients.iter().any(|o| o.phase == Phase::Typing)
                        {
                            return Err("busy");
                        }
                        c.phase = Phase::Wait;
                    }
                }
                if matches!(c.phase, Phase::Wait | Phase::DeliverReady) {
                    scratch.clear();
                    match c.socket.recv(scratch.spare()) {
                        Ok(None) => (),
                        Err("peer disconnected") => {
                            c.phase = Phase::Dead;
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                        Ok(Some(_)) => return Err("unexpected packet"),
                    }
                    scratch.clear();
                }
                if c.phase == Phase::Wait {
                    let r = c.request.as_ref().unwrap();
                    if r.operation == "type" && !keyboard.ready() {
                        return Err("keyboard unavailable");
                    }
                    if channels.get(&r.channel).and_then(State::pending).is_some()
                        && (r.operation != "type"
                            || !clients.iter().any(|o| o.phase == Phase::Typing))
                    {
                        c.activity = now;
                        c.phase = Phase::Reauth;
                        if !resolver.submit(c.id, c.uid, r.users(&config).to_vec()) {
                            return Err("authorization unavailable");
                        }
                        return Ok(());
                    }
                }
                if c.phase == Phase::DeliverReady {
                    let r = c.request.as_ref().unwrap();
                    if r.operation == "type" && !keyboard.ready() {
                        return Err("keyboard unavailable");
                    }
                    let state = channels.get_mut(&r.channel).unwrap();
                    state.expire(boottime());
                    if state.pending().is_none()
                        || (r.operation == "type"
                            && clients.iter().any(|o| o.phase == Phase::Typing))
                    {
                        if r.sync {
                            c.phase = Phase::Wait;
                        } else {
                            return Err("no pending secret");
                        }
                    } else {
                        if r.operation == "type" {
                            layout.validate(state.pending().unwrap().bytes())?;
                        }
                        c.secret = state.take(boottime());
                        if c.secret.is_none() {
                            if !r.sync {
                                return Err("no pending secret");
                            }
                            c.phase = Phase::Wait;
                            return Ok(());
                        }
                        c.started = now;
                        c.next_event = now;
                        c.cursor = 0;
                        c.phase = if r.operation == "type" {
                            Phase::Typing
                        } else {
                            Phase::OutHeader
                        };
                    }
                }
                if c.active() && now.saturating_sub(c.started) >= Duration::from_secs(300) {
                    return Err("delivery timeout; partial delivery possible");
                }
                if matches!(
                    c.phase,
                    Phase::OutHeader | Phase::OutData | Phase::OutCommit
                ) {
                    let sent=match c.phase {
                        Phase::OutHeader=>c.socket.send(&serde_json::to_vec(&serde_json::json!({"ok":true,"length":c.secret.as_ref().unwrap().bytes().len()})).unwrap())?,
                        Phase::OutData=>{let bytes=c.secret.as_ref().unwrap().bytes(); let end=(c.cursor+CHUNK).min(bytes.len()); let sent=c.socket.send(&bytes[c.cursor..end])?; if sent {c.cursor=end;} sent},
                        _=>c.socket.send(COMMIT)?,
                    };
                    if sent {
                        c.phase = match c.phase {
                            Phase::OutHeader if c.secret.as_ref().unwrap().bytes().is_empty() => {
                                Phase::OutCommit
                            }
                            Phase::OutHeader => Phase::OutData,
                            Phase::OutData
                                if c.cursor == c.secret.as_ref().unwrap().bytes().len() =>
                            {
                                Phase::OutCommit
                            }
                            Phase::OutData => Phase::OutData,
                            _ => {
                                c.secret = None;
                                if unsafe { libc::shutdown(c.socket.fd(), libc::SHUT_WR) } != 0 {
                                    return Err("output shutdown failed");
                                }
                                Phase::OutAck
                            }
                        };
                    }
                    return Ok(());
                }
                if !matches!(c.phase, Phase::Typing | Phase::Wait | Phase::OutAck)
                    && (now.saturating_sub(c.activity) > Duration::from_secs(5)
                        || (c.store && now.saturating_sub(c.created) > Duration::from_secs(30)))
                {
                    return Err("request timeout");
                }
                scratch.clear();
                match c.socket.recv(scratch.spare()) {
                    Ok(None) => (),
                    Err("peer disconnected") if c.phase == Phase::End => {
                        let r = c.request.as_ref().unwrap();
                        if clients
                            .iter()
                            .any(|o| o.channel() == r.channel && o.active())
                        {
                            return Err("busy");
                        }
                        channels.get_mut(&r.channel).unwrap().install(
                            c.secret.take().unwrap(),
                            boottime(),
                            Duration::from_secs(config.ttl_seconds),
                        );
                        c.ok();
                    }
                    Err(e) => return Err(e),
                    Ok(Some(n)) => {
                        scratch.set_len(n)?;
                        let data = scratch.bytes();
                        c.activity = now;
                        match c.phase {
                            Phase::Header => {
                                if n > 512 {
                                    return Err("request header too large");
                                }
                                let r: Request = serde_json::from_slice(data)
                                    .map_err(|_| "invalid request; protocol v2 required")?;
                                r.validate()?;
                                if c.store != (r.operation == "store") {
                                    return Err("wrong endpoint");
                                }
                                if !resolver.submit(c.id, c.uid, r.users(&config).to_vec()) {
                                    return Err("authorization unavailable");
                                }
                                c.request = Some(r);
                                c.phase = Phase::Auth;
                            }
                            Phase::Data => {
                                let total = c.request.as_ref().unwrap().length.unwrap();
                                let secret = c.secret.as_mut().unwrap();
                                if n > CHUNK || n > total - secret.bytes().len() {
                                    return Err("invalid data packet length");
                                }
                                secret.append(data)?;
                                if secret.bytes().len() == total {
                                    c.phase = Phase::Commit;
                                }
                            }
                            Phase::Commit if data == COMMIT => c.phase = Phase::End,
                            Phase::OutAck if data == b"DONE2" => c.phase = Phase::Dead,
                            _ => return Err("unexpected packet"),
                        }
                    }
                }
                scratch.clear();
                if c.phase == Phase::Typing && now >= c.next_event {
                    c.next_event = now + Duration::from_millis(1);
                    let bytes = c.secret.as_ref().unwrap().bytes();
                    if keyboard.idle() && c.cursor < bytes.len() {
                        // Validated before consumption; cursor advances by UTF-8 widths.
                        let mut ch = unsafe { std::str::from_utf8_unchecked(&bytes[c.cursor..]) }
                            .chars()
                            .next()
                            .unwrap();
                        let stroke = layout.lookup(ch);
                        c.cursor += ch.len_utf8();
                        ch.zeroize();
                        keyboard.prepare(stroke?)?;
                    }
                    keyboard.tick()?;
                    if c.cursor == c.secret.as_ref().unwrap().bytes().len() && keyboard.idle() {
                        c.ok();
                    }
                }
                Ok(())
            })();
            scratch.clear();
            if let Err(e) = result {
                if c.phase == Phase::Typing {
                    let _ = keyboard.release();
                }
                if e == "peer disconnected" {
                    c.phase = Phase::Dead;
                } else {
                    c.error(e);
                }
            }
            if c.phase != Phase::Dead {
                clients.push(c);
            }
        }
        let mut fds = vec![
            libc::pollfd {
                fd: submit.fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: control.fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        for c in &clients {
            if matches!(c.phase, Phase::Auth | Phase::Reauth) {
                continue;
            }
            fds.push(libc::pollfd {
                fd: c.socket.fd(),
                events: if matches!(
                    c.phase,
                    Phase::Reply | Phase::OutHeader | Phase::OutData | Phase::OutCommit
                ) {
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
                if clients.iter().any(|c| c.phase == Phase::Typing) {
                    1
                } else {
                    10
                },
            );
        }
    }
    let _ = keyboard.release();
    drop(clients);
    drop(channels);
    eprintln!("viskad: stopped");
    Ok(())
}

#[cfg(test)]
mod request_tests {
    use super::*;
    #[test]
    fn strict_version_channel_and_sync() {
        let mut request = Request {
            version: 2,
            operation: "output".into(),
            channel: "default".into(),
            sync: true,
            length: None,
        };
        request.validate().unwrap();
        for name in ["", "../secret", "a/b", "a b", "å", "_hidden"] {
            request.channel = name.into();
            assert!(request.validate().is_err());
        }
        request.channel = "work_2-test".into();
        request.validate().unwrap();
        request.version = 1;
        assert!(request.validate().is_err());
        request.version = 2;
        request.operation = "store".into();
        request.length = Some(1);
        assert!(request.validate().is_err());
        request.sync = false;
        request.validate().unwrap();
        assert!(serde_json::from_slice::<Request>(
            br#"{"version":2,"version":2,"operation":"output"}"#
        )
        .is_err());
    }
}
