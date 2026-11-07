use std::{io::Write, path::PathBuf};
use viska::{memory::Secret, socket::Socket, Result, CHUNK, MAX_PAYLOAD};
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!("Usage: viskactl [--socket-dir PATH] store|type|clear|status\n\
            store reads UTF-8 exclusively from stdin (up to 1 MiB); no trailing newline is removed.\n\
            type consumes once and requests keyboard injection into the focused destination.\n\
            status returns metadata only. Requests are never automatically retried.\n\
            Set VISKA_SOCKET_DIR for an isolated demo. Only viskad needs /dev/uinput.");
        return Ok(());
    }
    let mut dir = std::env::var_os("VISKA_SOCKET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/run/viska".into());
    let command = match args.as_slice() {
        [command] => command.as_str(),
        [flag, path, command] if flag == "--socket-dir" => {
            dir = path.into();
            command.as_str()
        }
        _ => return Err("usage: viskactl [--socket-dir PATH] store|type|clear|status"),
    };
    if !matches!(command, "store" | "type" | "clear" | "status") {
        return Err("unknown command");
    }
    viska::protect_process()?;
    let socket = if command == "store" {
        // One extra protected byte detects oversized stdin without silent truncation.
        let mut input = Secret::new(MAX_PAYLOAD + 1)?;
        loop {
            if input.bytes().len() == MAX_PAYLOAD + 1 {
                return Err("payload limit exceeded");
            }
            // Avoid Stdin's unprotected internal buffering: read directly into
            // the locked allocation and never make a heap copy of stdin bytes.
            let spare = input.spare();
            let n =
                unsafe { libc::read(libc::STDIN_FILENO, spare.as_mut_ptr().cast(), spare.len()) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err("cannot read stdin");
            }
            if n == 0 {
                break;
            }
            input.set_len(input.bytes().len() + n as usize)?;
        }
        std::str::from_utf8(input.bytes()).map_err(|_| "input is not valid UTF-8")?;
        let socket = Socket::connect(&dir.join("submit.sock"))?;
        let mut header = [0u8; 16];
        header[..8].copy_from_slice(viska::daemon::MAGIC);
        header[8..].copy_from_slice(&(input.bytes().len() as u64).to_le_bytes());
        socket.send_wait(&header)?;
        for chunk in input.bytes().chunks(CHUNK) {
            socket.send_wait(chunk)?;
        }
        socket.send_wait(viska::daemon::COMMIT)?;
        if unsafe { libc::shutdown(socket.fd(), libc::SHUT_WR) } != 0 {
            return Err("cannot finish submission; outcome uncertain");
        }
        // Secret input is zeroized before waiting for the acknowledgment.
        drop(input);
        socket
    } else {
        let socket = Socket::connect(&dir.join("control.sock"))?;
        socket.send_wait(match command {
            "type" => b"t",
            "clear" => b"c",
            _ => b"?",
        })?;
        socket
    };
    // A response is metadata only. Bound even a malicious endpoint's reply.
    let mut response = [0u8; 2048];
    let deadline = viska::boottime()
        + std::time::Duration::from_secs(if command == "type" { 305 } else { 35 });
    let n = loop {
        if let Some(n) = socket.recv(&mut response)? {
            break n;
        }
        if viska::boottime() >= deadline {
            return Err("response timeout; request outcome may be uncertain");
        }
        socket.wait(libc::POLLIN, 1000).or_else(|_| {
            if viska::boottime() < deadline {
                Ok(())
            } else {
                Err("response timeout; request outcome may be uncertain")
            }
        })?;
    };
    let reply: serde_json::Value =
        serde_json::from_slice(&response[..n]).map_err(|_| "invalid daemon response")?;
    if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        // Do not echo arbitrary endpoint-provided errors; only public error codes.
        let error = reply.get("error").and_then(|v| v.as_str()).unwrap_or("");
        return Err(match error {
            "not authorized" => "not authorized",
            "busy" => "busy",
            "no pending secret" => "no pending secret",
            "unsupported character for configured layout" => {
                "unsupported character for configured layout"
            }
            "payload limit exceeded" => "payload limit exceeded",
            "input is not valid UTF-8" => "input is not valid UTF-8",
            "keyboard unavailable" => "keyboard unavailable",
            _ => "request failed; partial typing or an uncertain outcome is possible",
        });
    }
    if command == "status" {
        // Whitelist metadata instead of forwarding an arbitrary reply body.
        let state = reply
            .get("state")
            .and_then(|v| v.as_str())
            .filter(|s| matches!(*s, "empty" | "pending" | "injecting"))
            .ok_or("invalid status")?;
        let output = serde_json::json!({"state":state, "ttl_remaining_ms":reply.get("ttl_remaining_ms").and_then(|v| v.as_u64()),
            "backend_ready":reply.get("backend_ready").and_then(|v| v.as_bool()).ok_or("invalid status")?});
        writeln!(std::io::stdout(), "{output}").map_err(|_| "cannot write status")?;
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("viskactl: {e}");
        std::process::exit(1);
    }
}
