# Current requirements: protocol v2 channels and stdout output

This section supersedes conflicting v1 requirements below. The user explicitly authorized secret retrieval via `output`.

- Every command accepts `--channel NAME`, default `default`. Names match `[A-Za-z0-9][A-Za-z0-9_-]{0,63}` and are public metadata.
- Keep one pending value per channel, not one globally. Channels share username allowlists; add independent `output_users` / NixOS `authorization.outputUsers`, default deny.
- `store` accepts arbitrary bytes up to 1 MiB, including controls and invalid UTF-8. Validate keyboard compatibility only when `type` requests consumption. Failed validation preserves the pending value for `output`.
- `output` consumes once and writes exact bytes to stdout without formatting or appended newline. Secret payloads use protected memory and binary IPC frames, never JSON/logs/errors. Metadata-only restrictions continue for all other commands.
- `type --sync` and `output --sync` wait indefinitely for a value (and keyboard availability for typing). One waiting consumer per channel; other consumers return busy. Authorize on registration and again before delivery. Detect disconnects before consuming.
- `store` returns on installation, without waiting for delivery. Delivery consumes before emission and never restores after failure. Output completion means writes to stdout completed, not downstream processing.
- Clear cancels waiting, staged, and active requests on that channel only. Empty inactive channels are reclaimed. Default max_channels is 8, configurable 1–64; bound memory for pending plus replacement staging on every channel.
- Allow independent concurrent stores and outputs across channels. Serialize keyboard injection globally. Keep five-minute active delivery deadlines; idle waiting has no delivery timeout.
- Use version-2 bounded JSON request metadata with channel/operation/sync, and bounded raw binary payload frames. Reject incompatible clients; update CLI and daemon together.
- Update NixOS locked-memory limits for aggregate storage, and PAM limits for output users too. Demo defaults to two channels and recommends 8 MiB memlock; never weaken protection.
- Extend tests for binary output, independent output authorization, channel isolation, waiting/clear/disconnect, reauthorization, contention, resource limits, and failure without replay. Update documentation and VM tests.

# Codex instructions: `viska`

## Goal

Build **viska**, a small, auditable Linux-only service for one-time delivery of sensitive text between separately authorized processes or Unix users **without using the desktop clipboard**.

Provide **`viskad`**, a persistent daemon running under a dedicated, non-login, non-root system account of the same name, and **`viskactl`**, a CLI with `store`, `type`, `output`, `clear`, and `status` commands. Submission and triggering may be performed by different Unix UIDs. Only the daemon accesses `/dev/uinput`.

NixOS is the required installation target. Deliver a Nix package, importable NixOS module, standalone interactive test applications, automated tests, and complete installation and operational documentation.

Remain application-agnostic: do not integrate the service with password managers, clipboard APIs, X11, Wayland, portals, desktop environments, or shortcut systems. A graphical session in a disposable test VM is a test fixture, not a production dependency.

## Desired user experience

```sh
printf '%s' 'example-secret' | viskactl store
# ... focus the intended text field ...
viskactl type
viskactl clear
viskactl status  # metadata only: never return the secret
```

- The producer may exit after acknowledged storage; the daemon persists between commands.
- A separately authorized user can trigger typing without receiving secret bytes or needing device permissions.
- Accept secrets exclusively through stdin, never through arguments, environment variables, configuration, or temporary files.
- Never return secret contents through an API, status response, log, tracing span, panic, error, or debugging facility.

## Architecture

```text
 producer UID A                         trigger UID B
      |                                      |
 stdin -> viskactl store                viskactl type/clear/status
      |                                      |
 restricted submission socket          restricted control socket
      |                                      |
      +------------------+-------------------+
                         |
                  viskad (system UID)
                  - username authorization
                  - one pending secret
                  - protected memory and expiry
                  - one-shot consumption
                         |
                     /dev/uinput
                         |
                  virtual keyboard
                         |
                  focused destination
```

## Configuration and authorization

### JSON configuration

Use one configuration format: JSON. The NixOS module generates root-owned `/etc/viska/config.json`, not writable by ordinary users. Systemd starts the packaged daemon with:

```sh
viskad --config /etc/viska/config.json
```

Example configuration (account names are illustrative):

```json
{
  "keyboard_layout": "us",
  "ttl_seconds": 45,
  "max_payload_bytes": 1048576,
  "authorization": {
    "store_users": ["producer"],
    "type_users": ["trigger"],
    "clear_users": ["trigger"],
    "status_users": ["producer", "trigger"]
  }
}
```

- Reject unknown fields, duplicate keys, invalid types, and out-of-range values.
- Missing authorization lists default to empty and grant no access.
- Configuration contains settings and account names, never secret values. Keep secrets out of Nix expressions, the Nix store, and generated configuration.
- Configuration changes require a daemon restart, clearing pending secrets. Do not implement live reload in v1.
- Support explicit endpoint-path configuration for isolated tests; production endpoints live under `/run/viska`.

### Username-based authorization

Expose usernames, not fixed numeric UIDs, in JSON and NixOS configuration:

```nix
services.viska = {
  enable = true;
  keyboardLayout = "se"; # Explicit Swedish selection; the default is "us".

  authorization = {
    storeUsers = [ "producer" ];
    typeUsers = [ "trigger" ];
    clearUsers = [ "trigger" ];
    statusUsers = [ "producer" "trigger" ];
  };
};
```

- Authenticate connections using kernel-provided `SO_PEERCRED` before processing payloads; never trust a client-supplied identity.
- For each request, resolve the relevant configured usernames through the running system's account database and compare their UIDs with the peer UID.
- Resolve accounts at runtime, not during Nix builds. Do not retain permanent startup UID mappings. Account-resolution failures deny the affected operation.
- Keep resolution bounded so account-database delays do not block expiry, injection cancellation, or control handling.
- Independently authorize submission, typing, clearing, and status. Submission permission never implies trigger permission.
- Use endpoint permissions, groups, or ACLs as additional barriers, not replacements for daemon checks or supplementary-group application authorization.
- Authorization identifies a Unix account, not human intent. Any process acting as an authorized account can exercise its permissions.
- Every authorized trigger can consume the shared pending secret, regardless of its producer. Per-secret recipient binding is outside v1.

## IPC and command semantics

### Submission and control transport

- Use two pathname Unix-domain `SOCK_SEQPACKET` sockets: one for submission and one for control, under `/run/viska`.
- This deliberately replaces the original FIFO preference: sockets provide peer credentials, isolated connections, record boundaries, and acceptance acknowledgments. A 4096-byte FIFO atomic-write limit cannot accommodate the agreed payload size and framing.
- The default maximum secret payload is **1 MiB (1,048,576 bytes)**, excluding protocol overhead. Permit lower configured limits; larger limits are outside v1. Treat lengths as untrusted.
- Use a versioned submission protocol with a declared total payload length, ordered packets carrying at most 16 KiB of payload each, and an explicit completion record. Each connection carries one submission.
- Do not depend on sending a single 1 MiB socket message. Handle UTF-8 characters spanning packets, EOF, packet truncation, and failed sends/receives explicitly.
- Reject malformed framing, unsupported versions, oversized declarations, excess bytes, incomplete transfers, and truncated packets. Never combine different connections' data or install partial data.
- Permit one staged submission at a time; reject additional transfers as busy. Apply a five-second inactivity timeout and a thirty-second total transfer deadline.
- Bound accepted control connections and response queues. Prevent stalled clients from exhausting resources or blocking expiry and cancellation.
- Clients must not be able to modify the runtime directory or replace endpoints. Establish restrictive permissions before making endpoints available; reject unexpected existing filesystem objects rather than blindly replacing them. Document permission/ACL provisioning and test effective access.

### Command contracts

- **`store`** succeeds only after complete validation and atomic installation. Invalid, unsupported, oversized, or incomplete submissions preserve an existing valid secret.
- **`type`** atomically consumes an unexpired secret before emitting events. Failure, cancellation, or client disconnection never restores it. Simultaneous triggers cannot replay it.
- **`clear`** zeroizes pending data and cancels staged submission. During injection, acknowledge completion only after stopping further typing and attempting held-key release.
- **`status`** returns only empty/pending/injecting state, remaining TTL where applicable, and backend readiness. Do not expose secret contents or length.
- Reject stores and additional type requests while injecting. A successful type response means the backend accepted the event sequence; it does not prove application receipt or correct destination.
- Document uncertain outcomes after connection loss. Never automatically retry a request that may already have taken effect.

## Secret lifetime, concurrency, and protected memory

- Use **Rust**, minimal dependencies, and narrow Linux interfaces. Serialize state transitions in one event loop; keep IPC and device operations nonblocking.
- Maintain at most one pending secret and one bounded staging buffer. Atomically replace the pending value only after full validation. Transfer consumed data into injection ownership rather than copying the complete secret.
- Default TTL: **45 seconds**, configurable from one to 300 seconds, starting at installation. Use Linux `CLOCK_BOOTTIME` so suspended time counts. Schedule proactive expiry cleanup and check expiry immediately before consumption.
- Clear on expiry, replacement, explicit clear, injection completion or failure, and orderly shutdown. Never persist secrets or use the kernel keyring in v1.
- Establish fixed-capacity protected buffers before receiving sensitive bytes. Use `mlock` and `MADV_DONTDUMP`; disable process dumpability and core dumps. Fail closed if required protections cannot be established.
- Protect CLI input, daemon receive/staging buffers, and transient event data. Translate incrementally instead of constructing a full-secret event array. Limit copies and explicitly zeroize before unlocking or freeing memory.
- Provision sufficient locked-memory limits without broad capabilities. Document prerequisites for both service and ordinary CLI execution; never silently weaken protections in the demo.
- Document limits involving kernel IPC buffers, privileged inspection, abrupt termination, and hibernation. Do not claim that protected userspace buffers prevent every possible persistent copy.

## Keyboard layouts and injection

- Implement injection through Linux `/dev/uinput`, never clipboard APIs, `xdotool`, `wtype`, or desktop protocols. Give only the daemon narrowly scoped device access, without physical input-device access.
- Provide configurable named mappings for **US (`us`, the default)** and **Swedish (`se`)**. Reject unknown layouts at startup.
- Accept valid UTF-8 containing all characters directly mapped by the selected layout, including Shift and AltGr combinations. Swedish support must include `åäöÅÄÖ` and the layout's other directly mapped symbols.
- Reject invalid UTF-8, control characters (including newline, tab, and NUL), and characters requiring dead-key or composition sequences. Do not normalize, transliterate, silently substitute, truncate, or append Enter.
- Validate complete input before installing it; never echo offending input in errors.
- Use reviewed static mappings derived from pinned XKB definitions, without a runtime XKB dependency. Keep translation independently testable; adding a layout requires mapping coverage and tests.
- The configured mapping must match the target session's active layout. Viska does not discover or change desktop layout settings. Document held physical modifiers, Caps Lock, input methods, and layout switching as correctness constraints.
- Keep a narrowly scoped virtual keyboard registered for the daemon's lifetime. Emit balanced key-down/key-up events with `EV_SYN` boundaries and track held keys.
- Attempt bounded best-effort release on failure or cancellation; disable the backend if recovery fails. Do not guarantee successful release after device failure.
- Pace injection without blocking cancellation or control handling. Bound injection attempts to five minutes; timeout cancels and consumes the secret.
- Document startup discovery delays: registering a device does not prove a graphical session is listening. Provide a manual readiness smoke test.
- Target the active input stack/focused application, not a window identity. V1 targets a tested single-seat installation without claiming destination authentication or perfect seat isolation. Document virtual-terminal, lock-screen, focus-stealing, multi-seat, and unintended-destination risks.

## Triggering

- V1 trigger: `viskactl type` through the authenticated control socket. It is an ordinary command, not a separate service started for each request.
- Users may bind it through their existing shortcut system. Document releasing shortcut modifiers before injection.
- Keep hardware-event and hotkey handling outside the core daemon. A USB HID button, foot pedal, or separate listener invoking the same command is an optional later component, not a required v1 deliverable.
- A future listener should receive access only to its specific device and run as a trigger-authorized identity. Do not request broad access to `/dev/input/event*` in v1.
- A physical button is not an authentication factor and does not authenticate the destination.

## NixOS packaging and systemd integration

### Required artifacts

Provide a flake exposing a package installing both binaries, an importable NixOS module, a development shell, automated checks, a standalone demo, and an interactive VM launcher.

The `services.viska` module must configure:

- Package selection and enablement.
- Dedicated `viskad` account and hardened `viskad.service`.
- Independent username authorization lists.
- Layout (default `us`), TTL, and payload limit.
- JSON generation, runtime directory, endpoint permissions, and necessary ACL/group provisioning.
- uinput module loading and narrowly scoped device permissions.
- Sufficient locked-memory limits.

### Service lifecycle and hardening

- Start the system service at boot when enabled and keep it alive between commands. The daemon creates its own IPC endpoints; do not use socket activation in v1.
- Use `Type=notify`. Report readiness after configuration validation, memory protection, endpoint setup, and virtual-keyboard registration. Readiness does not guarantee discovery by a graphical session.
- Use systemd `RuntimeDirectory=viska` to manage `/run/viska`, with safe ownership and permissions allowing only the necessary client traversal/access.
- Run the packaged daemon with `--config /etc/viska/config.json`; do not hardcode Nix store paths.
- Combine `/dev/uinput` filesystem permissions with systemd device restrictions. `DeviceAllow` alone does not grant filesystem access. Never rely on world-writable devices, broad input groups, or sandbox settings that hide required device access.
- Disable core dumps, prevent privilege escalation, and restrict filesystem access without breaking account resolution or injection.
- Restart after crashes with rate limiting; every restart begins empty. Arrange a restart when generated configuration changes. Do not implement live reload in v1.
- On orderly shutdown, stop injection, attempt key release, zeroize sensitive memory, and destroy the virtual keyboard. Preserve systemd's bounded shutdown behavior.
- Log operational metadata and errors only. Document administration with `systemctl status/restart/stop viskad.service` and `journalctl -u viskad.service`.

## Interactive testing before host installation

The following commands are required deliverables, not prerequisites already present in this repository. They must work without incorporating viska into the host's NixOS configuration.

### Standalone mock demo

```sh
nix run .#demo
```

- Open a temporary shell containing `viskactl` and a running mock-backed daemon.
- Create a private `0700` runtime directory and temporary JSON configuration authorizing only the invoking username. Direct the CLI to isolated test endpoints, separate from production paths.
- Preserve memory protections and report unmet prerequisites explicitly. Do not grant host device access or require host service installation.
- Support interactive commands such as:

```sh
printf '%s' 'example-secret@123' | viskactl store
viskactl status
viskactl type
viskactl status
```

- The mock validates event generation and reports completion without printing secrets or event sequences. It never injects into host applications.
- On exit, stop the daemon and remove runtime artifacts. Demonstrate framing, validation, expiry, and one-shot behavior; do not claim this proves cross-UID isolation or real device operation.
- Document how to select Swedish for an isolated demo and try nonsensitive Swedish text.

### Disposable graphical NixOS VM

```sh
nix run .#test-vm
```

- Launch a repository-owned VM configuration importing the actual module, without importing the host's `/etc/nixos` configuration.
- Include separate producer, trigger, and unauthorized users, normal service hardening and memory protection, guest `/dev/uinput`, and an ordinary graphical text field.
- Explicitly configure Swedish (`se`) in both viska and the guest graphical session to exercise the nondefault layout.
- Document storing nonsensitive text as the producer, focusing the guest field, and triggering typing as the authorized trigger user.
- Exercise real uinput in the guest without injecting into host applications. Use disposable storage by default and do not share host home directories or clipboard services.
- Print shutdown and cleanup instructions. Document display and virtualization prerequisites, and support slower software emulation for interactive use when KVM is unavailable.
- Keep a separate manual host uinput smoke test after installation: guest success does not establish correctness in the host's desktop session.

## Automated tests and acceptance criteria

Expose at least these commands on x86_64 Linux:

```sh
nix flake check --no-build
nix build .#checks.x86_64-linux.unit-tests
nix build .#checks.x86_64-linux.integration
```

Use the actual NixOS module with a mock keyboard backend for automated cross-UID VM integration tests. Document KVM and builder prerequisites separately from ordinary unit tests. Tests not requiring a real uinput device must run using the mock backend.

Required coverage:

- Authorized producer A can store and exit; distinct authorized trigger B can consume exactly once without receiving secret bytes or needing device permissions.
- Unauthorized accounts cannot submit, type, clear, or inspect status. Producer-only permission cannot trigger typing.
- Username resolution handles missing users, lookup failures, and changed UID mappings without stale authorization.
- US is the default; explicit Swedish configuration works through JSON and the NixOS module. Test all directly mapped characters, including Swedish letters and Shift/AltGr event sequences.
- Reject unsupported characters, controls, and invalid UTF-8; handle valid multibyte characters spanning transport packets.
- Accept supported input exactly 1 MiB long and reject one byte over the limit without truncation or replacement of existing valid data.
- Concurrent producers cannot mix submissions; malformed, truncated, stalled, oversized, and disconnected transfers cannot install partial values.
- Concurrent triggers cannot replay secrets. Cover expiry across simulated suspend, replacement, clearing, injection cancellation/failure, cleanup failure, and prevention of replay.
- Cover protection failures and zeroization of owned sensitive buffers.
- Validate strict JSON parsing, generated configuration, module evaluation, service readiness/restarts/shutdown, effective endpoint/device access, and demo cleanup.
- Verify the product does not touch the clipboard or depend on desktop protocols.
- Manually exercise real uinput in a disposable Swedish-layout VM with nonsensitive text, including Swedish letters and AltGr symbols.

## Implementation order

1. Establish these requirements and the protocol/state design before writing the service.
2. Implement protected buffers, clock abstraction, state transitions, and deterministic tests.
3. Implement US and Swedish translation tables and mock injection, including failures and cancellation.
4. Implement JSON configuration, username authorization, authenticated IPC, and CLI.
5. Add the standalone demo, Nix packaging, module, systemd integration, and cross-UID VM tests.
6. Implement real uinput, the graphical VM, deployment hardening, security review notes, and operational documentation.

Keep steps small and testable. Add tests with each subsystem rather than deferring them to the end.

## Documentation and non-goals

- README instructions must lead with standalone testing, then cover permanent NixOS incorporation, separate producer/trigger authorization, US default and Swedish selection, rebuilding, verification, troubleshooting, and removal.
- Document partial typing, uncertain outcomes, potentially lengthy large-payload injection, memory protection, swap/hibernation, permissions, service hardening, and focus/seat risks.
- No password-manager integration, production GUI, desktop-specific APIs, network synchronization, secret retrieval/history, arbitrary Unicode composition, kernel-keyring persistence, or fingerprint/NFC authentication in v1.

## Important security caveat

Viska is a **secret-delivery mechanism**, not a secure attention key or trusted path. A malicious focused application, keylogger, compromised input stack, or sufficiently privileged process may capture typed secrets. Bypassing the clipboard does not authenticate the destination or eliminate these threats.

## Instructions when working on this repository

- Prioritize correctness, least privilege, explicit failure modes, and testability over feature count.
- Do not weaken privilege separation for convenience. Explain and isolate any additional capabilities.
- Do not hardcode local usernames, UIDs, seats, nonstandard device paths, or Nix store paths. Example account names must remain clearly illustrative.
- Avoid unnecessary frameworks and dependencies. Prefer explicit errors to silent fallback, especially for authorization, memory protection, and translation.
- Update README and tests with every externally visible implementation behavior change.
- Before declaring a milestone complete, run available checks and report separately what passed and whether mock tests, cross-UID VM tests, real guest uinput, and real host uinput were exercised.
