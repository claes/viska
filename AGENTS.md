# Codex instructions: `secretd`

## Goal

Build **secretd**, a small, auditable Linux-only service for one-time transfer of sensitive text between processes/users **without using the desktop clipboard**. Any authorized producer submits bytes via standard input; a separately authorized consumer triggers the service to synthesize keyboard events into the active seat through `/dev/uinput`. Submission and triggering may be performed by **different Unix UIDs**. The service runs under its own dedicated system UID.

This project is **application-agnostic**: do not integrate it with `pass`, password managers, X11, Wayland, portals, or a particular desktop environment.

## Desired user experience

```sh
printf '%s' 'example-secret' | secretctl store
# ... focus a target text field ...
secretctl type
# or: secretctl clear
secretctl status  # metadata only: never return the secret
```

The producer can exit after a successful `store`. `secretctl type` is a separate invocation, possibly under another UID. A service must persist between these operations.

`secretctl type` **never prints or returns secret bytes**. It requests a one-shot injection by `secretd`.

## Architecture

```text
 producer UID A                    trigger UID B
      |                                  |
 stdin -> secretctl store         secretctl type/clear/status
      |                                  |
      v                                  v
 restricted submission IPC         restricted control IPC
      |                                  |
      +--------------+-------------------+
                     |
                     v
          secretd (dedicated system UID)
          - one pending secret in memory
          - expiration and one-shot state
          - restricted access to /dev/uinput
                     |
                     v
          Linux virtual keyboard -> active seat
```

### IPC and authorization

- Prefer a **write-only FIFO** for submission, as discussed. Provide a CLI that frames/validates submissions so secrets may contain newlines and do not require a terminating newline. A FIFO is a byte stream: explicitly handle record boundaries, partial writes, concurrent producers, EOF, and the `PIPE_BUF` atomicity limit. **Never assume one `read()` equals one message.** If a robust atomic-record protocol is awkward with the FIFO, document that clearly and propose a Unix-domain `SOCK_SEQPACKET` socket as a safer alternative *before implementing it*. Preserve the logical separation of submission and control.
- Prefer a separate **Unix-domain control socket** for `type`, `clear`, and `status`. Authenticate callers using `SO_PEERCRED`; authorize based on explicitly configured UIDs/groups. Unix socket permissions are an additional barrier, not a replacement for credential checks.
- Producer authorization and trigger authorization must be separate. A producer must not automatically gain the ability to trigger typing, and a trigger user must not have a read-secret capability.
- Publish no API that returns secret bytes, including debugging, logs, status, or errors.
- Create socket/FIFO paths under a systemd-managed `/run/secretd` directory with safe owner, permissions, and no writable-parent or symlink-replacement vulnerability. A dedicated service UID must be able to read the submission endpoint; producer UIDs should be able to write to it but not read it. Document ACL/group provisioning and verify effective access with tests.
- Treat IPC payload sizes as untrusted. Enforce a configurable maximum (initial default: 4096 bytes) and fail closed on invalid or oversized submissions. Avoid accepting unauthenticated clients.

### Secret lifetime and concurrency

- Keep **at most one pending secret**. A new valid submission atomically replaces the old one; never allow partially received bytes to become the active secret.
- Default TTL: **45 seconds**, measured with a monotonic clock. TTL is configurable with a sensible maximum.
- Clear on expiry, explicit `clear`, replacement, service shutdown, and after a `type` request has been accepted for consumption.
- Enforce one-shot consumption using an atomic state transition; simultaneous `type` requests must not replay the secret. The daemon should serialize transitions in a single event loop or use a clearly correct lock.
- If injection fails, report the failure without re-enabling replay by default. Document when/why partial typing may occur.
- Do not persist the secret to disk or use the Linux kernel keyring in v1. We chose **daemon-owned protected memory** for the simpler single-consumer design.
- Use a bounded sensitive buffer and limit copies. On Linux, attempt `mlock`, use `MADV_DONTDUMP` where applicable, disable core dumps and process dumpability as appropriate, and explicitly zeroize secret buffers and transient copies. Define behavior if locking fails (default: fail closed unless explicitly configured otherwise). These are defense-in-depth protections, not guarantees against root or a compromised daemon.
- Avoid placing secrets in argv, environment variables, journal/log output, tracing spans, panic messages, or temporary files.

### Keyboard injection

- Implement injection through **Linux `/dev/uinput`**, not clipboard APIs, `xdotool`, `wtype`, or any desktop protocol. Use a narrowly scoped virtual keyboard device. `secretd` should be given specific `/dev/uinput` access rather than broad access to physical input devices.
- Create and manage a virtual keyboard. Prefer keeping it registered for the service lifetime to avoid hotplug discovery delays and dropped initial events. Clean it up on shutdown.
- Generate balanced key-down/key-up events with `EV_SYN` boundaries and ensure error paths release held modifiers.
- **Character encoding/layout is a hard constraint:** `/dev/uinput` injects keycodes, not Unicode text. Never assume that arbitrary UTF-8 will type correctly under every keyboard layout. For the first milestone, explicitly support a documented character set and layout (e.g. printable ASCII on a configured US layout); reject unsupported characters instead of silently typing wrong bytes. Make the translation layer independently testable. Design for future configurable layout support.
- Targeting is by the active input seat/focused application, **not by window identity**. Document multi-seat, virtual-terminal, lock-screen, focus-stealing, and unintended-destination risks. Do not claim perfect seat isolation without implementation and testing.
- The `type` command must not need permission to access `/dev/uinput`; only the service does.

### Triggering

- V1 trigger: `secretctl type` through the authenticated control socket. This command is suitable for binding to any existing shortcut system.
- Keep hotkey handling outside the core service. A small USB HID button or evdev shortcut handler is an optional later feature, **not required for v1**. Do not request broad access to `/dev/input/event*` in v1.
- The act of triggering does not authenticate the *destination*. Clearly warn users that typing goes to the currently focused application and a physical button alone is not an authentication factor.

### Service and deployment

- Linux and systemd are initial targets; Nix/NixOS packaging is desirable.
- Dedicated non-login, non-root system account (`secretd`) with minimum necessary device access and systemd hardening. Explicitly document how `/dev/uinput` permissions are granted; never rely on world-writable device permissions or unrestricted groups.
- Provide systemd service configuration, a sample NixOS module (if practical), and instructions to authorize separate producer and trigger identities.
- Prefer a small, reviewable implementation in **Rust** or **C**; choose based on the repository's existing language/tooling. Avoid unnecessary frameworks and dependencies. If starting from an empty repo, Rust is preferred for IPC parsing and state management, while using Linux interfaces directly or through narrow crates.
- Don't assume root privileges for ordinary `secretctl` commands.

## Suggested implementation order

1. Inspect the repository and write a brief plan identifying the language, IPC protocol, service model, keycode/layout strategy, and permission model. Note any deviations from this spec.
2. Implement the state machine and bounded protected buffer, with deterministic unit tests for expiry, replacement, clearing, and concurrent one-shot consumption.
3. Implement the authorization and IPC protocol, including a real cross-UID integration test or a documented privileged test harness.
4. Implement a mockable keyboard backend and unit-test event sequences and unsupported-character failures.
5. Add the real `/dev/uinput` backend, with a manual smoke-test procedure for a disposable test session.
6. Add the CLI (`store`, `type`, `clear`, `status`), help text, service deployment, and hardening.
7. Add automated tests, security review notes, and operational documentation. Keep each step small and testable.

## Acceptance criteria

- An authorized user A can run `printf ... | secretctl store`, exit, and the daemon retains the pending secret until consumed or expired.
- A distinct authorized user B can run `secretctl type` and cause exactly one injection attempt, without receiving the secret as output or needing `/dev/uinput` access.
- An unauthorized UID cannot submit, trigger, clear, or inspect status unless explicitly authorized.
- Concurrent submissions cannot interleave into a valid secret, and concurrent triggers cannot replay it.
- Timeouts and replacement zeroize previous secrets; expired secrets cannot be typed.
- Newlines and binary input are either correctly framed and supported or explicitly rejected with precise errors; never silently truncate.
- The implementation does not touch the system clipboard or require X11/Wayland.
- Unsupported characters/layouts fail explicitly; supported test strings produce the correct evdev event sequences.
- The daemon never logs sensitive contents; memory-dump, swap, permission, and service-hardening choices are documented.
- Tests run without a real `/dev/uinput` device using a mock backend. A separate manual test verifies the real backend.

## Non-goals for v1

- Password manager integration, including `pass` wrappers or patched executables.
- A graphical interface or desktop-specific clipboard and shortcut APIs.
- Arbitrary Unicode text injection across every keyboard layout.
- Synchronization over a network, cloud storage, multi-user secret retrieval, or secret history.
- Guaranteeing the identity or safety of the currently focused window.
- Kernel-keyring-based persistence or authentication via fingerprint/NFC.

## Important security caveat

This is a **secret-delivery mechanism**, not a secure attention key or trusted path. A malicious focused application, a keylogger, a compromised input stack, or a process with sufficiently elevated privileges can capture typed secrets. Avoid overstating the protection achieved by bypassing the clipboard.

## Instructions when working on this repository

- Prioritize correctness, least privilege, clear failure modes, and testability over feature count.
- Do not weaken privilege separation for convenience. If a requested feature requires additional capabilities, explain and isolate them.
- Do not hardcode local usernames, UIDs, seats, device paths beyond standard Linux interfaces, or Nix store paths.
- Prefer explicit errors to silent fallback, especially around security properties and keyboard translation.
- Update README and tests with every externally visible behavior change.
- Before declaring a milestone complete, run the available tests and report what was and was not tested (including whether real `uinput` was exercised).
