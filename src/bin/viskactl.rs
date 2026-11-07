use std::{
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use viska::{
    daemon::{Request, COMMIT},
    memory::Secret,
    socket::Socket,
    Result, CHUNK, MAX_PAYLOAD,
};
static CANCEL: AtomicBool = AtomicBool::new(false);
extern "C" fn cancel(_: i32) {
    CANCEL.store(true, Ordering::Relaxed);
}
fn check() -> Result<()> {
    if CANCEL.load(Ordering::Relaxed) {
        Err("request cancelled")
    } else {
        Ok(())
    }
}
fn receive(socket: &Socket, buffer: &mut Secret, deadline: Option<Duration>) -> Result<()> {
    buffer.clear();
    loop {
        check()?;
        if let Some(n) = socket.recv(buffer.spare())? {
            buffer.set_len(n)?;
            return Ok(());
        }
        if deadline.is_some_and(|d| viska::boottime() >= d) {
            return Err("response timeout; outcome uncertain");
        }
        let _ = socket.wait(libc::POLLIN, 100);
    }
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: viskactl [--socket-dir PATH] [--channel NAME] store|type|output|clear|status [--sync]\nChannel defaults to default. --sync waits for a value (type/output only).\nstore reads arbitrary stdin bytes, up to 1 MiB. output consumes and writes exact bytes.\ntype consumes once through uinput; its layout must support the value. No automatic retries.");
        return Ok(());
    }
    let mut dir = std::env::var_os("VISKA_SOCKET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/run/viska".into());
    let mut r = Request {
        version: 2,
        operation: String::new(),
        channel: "default".into(),
        sync: false,
        length: None,
    };
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--socket-dir" => dir = iter.next().ok_or("missing socket directory")?.into(),
            "--channel" => r.channel = iter.next().ok_or("missing channel")?.clone(),
            "--sync" => r.sync = true,
            _ if r.operation.is_empty() => r.operation = a.clone(),
            _ => return Err("unexpected argument"),
        }
    }
    if r.operation == "store" {
        r.length = Some(0);
    }
    r.validate()?;
    viska::protect_process()?;
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = cancel as *const () as _;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let mut input = if r.operation == "store" {
        Some(Secret::new(MAX_PAYLOAD + 1)?)
    } else {
        None
    };
    if let Some(input) = input.as_mut() {
        loop {
            check()?;
            if input.bytes().len() > MAX_PAYLOAD {
                return Err("payload limit exceeded");
            }
            let spare = input.spare();
            let n = unsafe { libc::read(0, spare.as_mut_ptr().cast(), spare.len()) };
            if n < 0 {
                return Err("stdin read failed or cancelled");
            }
            if n == 0 {
                break;
            }
            input.set_len(input.bytes().len() + n as usize)?;
        }
        r.length = Some(input.bytes().len());
    }
    let socket = Socket::connect(&dir.join(if r.operation == "store" {
        "submit.sock"
    } else {
        "control.sock"
    }))?;
    socket.send_wait(&serde_json::to_vec(&r).map_err(|_| "cannot encode request")?)?;
    if let Some(input) = input.take() {
        for chunk in input.bytes().chunks(CHUNK) {
            check()?;
            socket.send_wait(chunk)?;
        }
        socket.send_wait(COMMIT)?;
        if unsafe { libc::shutdown(socket.fd(), libc::SHUT_WR) } != 0 {
            return Err("submission completion failed; outcome uncertain");
        }
    }
    let mut packet = Secret::new(CHUNK + 32)?;
    let deadline = if r.sync {
        None
    } else {
        Some(viska::boottime() + Duration::from_secs(if r.operation == "type" { 305 } else { 35 }))
    };
    receive(&socket, &mut packet, deadline)?;
    if packet.bytes().len() > 2048 {
        return Err("invalid response");
    }
    let reply: serde_json::Value = serde_json::from_slice(packet.bytes())
        .map_err(|_| "invalid response; protocol v2 required")?;
    if reply["ok"] != true {
        return Err(match reply["error"].as_str().unwrap_or("") {
            "not authorized" => "not authorized",
            "busy" => "busy",
            "no pending secret" => "no pending secret",
            "channel limit reached" => "channel limit reached",
            "input is not valid UTF-8" => "input is not valid UTF-8",
            "unsupported character for configured layout" => {
                "unsupported character for configured layout"
            }
            "payload limit exceeded" => "payload limit exceeded",
            "channel cleared; delivery cancelled" => "channel cleared; delivery cancelled",
            _ => "request failed; delivery may be partial or outcome uncertain",
        });
    }
    if r.operation == "output" {
        let length = reply["length"]
            .as_u64()
            .filter(|n| *n <= MAX_PAYLOAD as u64)
            .ok_or("invalid output length")? as usize;
        let mut value = Secret::new(MAX_PAYLOAD)?;
        let end = viska::boottime() + Duration::from_secs(300);
        while value.bytes().len() < length {
            receive(&socket, &mut packet, Some(end))?;
            if packet.bytes().len() > CHUNK || packet.bytes().len() > length - value.bytes().len() {
                return Err("invalid output packet");
            }
            value.append(packet.bytes())?;
        }
        receive(&socket, &mut packet, Some(end))?;
        if packet.bytes() != COMMIT {
            return Err("invalid output completion");
        }
        packet.clear();
        loop {
            check()?;
            match socket.recv(packet.spare()) {
                Err("peer disconnected") => break,
                Ok(None) => {
                    if viska::boottime() >= end {
                        return Err("output timeout");
                    }
                    let _ = socket.wait(libc::POLLIN, 100);
                }
                _ => return Err("excess output data"),
            }
        }
        packet.clear();
        let mut cursor = 0;
        while cursor < value.bytes().len() {
            check()?;
            if viska::boottime() >= end {
                return Err("output timeout; partial delivery possible");
            }
            let mut fds = [
                libc::pollfd {
                    fd: 1,
                    events: libc::POLLOUT,
                    revents: 0,
                },
                libc::pollfd {
                    fd: socket.fd(),
                    events: 0,
                    revents: 0,
                },
            ];
            unsafe {
                libc::poll(fds.as_mut_ptr(), 2, 100);
            }
            if fds[1].revents & libc::POLLHUP != 0 {
                return Err("output cancelled; partial delivery possible");
            }
            if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err("stdout unavailable; partial delivery possible");
            }
            if fds[0].revents & libc::POLLOUT == 0 {
                continue;
            }
            let bytes = &value.bytes()[cursor..(cursor + 4096).min(length)];
            let n = unsafe { libc::write(1, bytes.as_ptr().cast(), bytes.len()) };
            if n <= 0 {
                return Err("stdout write failed; partial delivery possible");
            }
            cursor += n as usize;
        }
        drop(value);
        socket.send_wait(b"DONE2")?;
    } else if r.operation == "status" {
        let state = reply["state"]
            .as_str()
            .filter(|s| {
                matches!(
                    *s,
                    "empty" | "pending" | "waiting" | "injecting" | "outputting"
                )
            })
            .ok_or("invalid status")?;
        let metadata = serde_json::json!({"state":state,"ttl_remaining_ms":reply["ttl_remaining_ms"].as_u64(),"backend_ready":reply["backend_ready"].as_bool().ok_or("invalid status")?});
        writeln!(std::io::stdout(), "{metadata}").map_err(|_| "status write failed")?;
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("viskactl: {e}");
        std::process::exit(1);
    }
}
