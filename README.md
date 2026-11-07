# Viska

Viska delivers sensitive text once through a Linux virtual keyboard without using
the desktop clipboard. `viskad` keeps one pending value in protected memory;
`viskactl store` submits stdin and a separately authorized account runs
`viskactl type`. US is the default layout; Swedish is explicitly supported.

This is a secret-delivery mechanism, **not a trusted input path**. The focused
application, keyloggers, input stack, or privileged processes can capture output.
The destination is never authenticated. Use nonsensitive text for every test.

## Try before installing

The flake requires Linux and Nix with `nix-command` and `flakes` enabled. Commands
build from this checkout; they do not change `/etc/nixos` or enable host services.
While new flake files are still untracked by Git, use `path:.#demo` or
`path:.#test-vm` in place of `.#demo` or `.#test-vm`; Git-backed flakes otherwise
omit untracked files. The same `path:.#` syntax applies to build checks.

### Interactive mock demo

```sh
nix run .#demo
```

The command opens a temporary shell with a mock daemon, isolated sockets, and
username authorization for your account. In that shell:

```sh
printf '%s' 'example-secret@123' | viskactl store
viskactl status
viskactl type
viskactl type   # fails: consumed already
```

Successful store/type/clear commands are silent. The mock exercises event generation
but never types into host applications. Status returns only metadata. Exiting the
shell stops the daemon and removes temporary runtime files.

For Swedish:

```sh
VISKA_DEMO_LAYOUT=se nix run .#demo
# In the demo shell:
printf '%s' 'räksmörgås ÅÄÖ @ €' | viskactl store
viskactl type
```

Both daemon and CLI fail closed when sensitive memory cannot be locked. Check
`ulimit -l`: allow at least 4096 KiB for the demo and producer CLI. You may raise the
soft limit up to your existing hard limit (`ulimit -Hl`); otherwise your login's
memory-lock limit must be provisioned by an administrator. No unsafe fallback is
provided. A daemon needs a little over 2 MiB; the NixOS service receives 8 MiB.

### Disposable graphical VM: real guest typing

```sh
nix run .#test-vm
```

This builds a separate NixOS configuration with the actual module, separate
producer/trigger/outsider accounts, a Swedish graphical session, and real guest
uinput. It does not import your host configuration. A graphical display is needed;
KVM is recommended, with slower QEMU software emulation as fallback.

The desktop logs in as `trigger`. Open Mousepad as the destination and two terminals.
In one terminal:

```sh
su - producer
# VM-only password: producer
printf '%s' 'räksmörgås ÅÄÖ @ €' | viskactl store
```

In the trigger terminal:

```sh
sleep 3; viskactl type
```

Focus Mousepad during the delay and release modifiers. Expect exactly the submitted
text and no Enter. A second type request must fail. Test the separate characters
`åäöÅÄÖ`, `@`, `€`, and `|`. A recently registered keyboard may need discovery time.
A successful daemon startup alone cannot establish desktop readiness.

Account passwords (including root) equal their usernames, only in this disposable VM. Additional
instructions are in `/etc/viska/VM-README.txt`. Shut down through the desktop menu.
The launcher removes its disk image on exit. Host home and clipboard are not shared.
Guest success does not prove correct behavior in your host desktop session.

## Install on NixOS

Add this repository as a flake input (replace the example location with your
checkout or repository URL):

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    viska.url = "path:/absolute/path/to/viska";
  };
  outputs = { nixpkgs, viska, ... }: {
    nixosConfigurations.myHost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        viska.nixosModules.default
        {
          services.viska = {
            enable = true;
            keyboardLayout = "se"; # Omit for US (us).
            ttlSeconds = 45;
            maxPayloadBytes = 1048576;
            authorization = {
              storeUsers = [ "producer" ];
              typeUsers = [ "trigger" ];
              clearUsers = [ "trigger" ];
              statusUsers = [ "producer" "trigger" ];
            };
          };
        }
      ];
    };
  };
}
```

Use your actual account names and host configuration. The module creates the
service account; client accounts should already be provisioned in your system.
It generates settings only: **never put a secret in a Nix expression or file in
the Nix store**. Usernames are resolved on the running system, not at build time.

```sh
sudo nixos-rebuild switch --flake .#myHost
# Log out and back in for client-group membership and PAM memlock changes.
systemctl status viskad.service
journalctl -u viskad.service
```

The module loads uinput, grants the service's dedicated group device access, adds
clients to a socket-access group, and sets producer login memory-lock limits.
Membership permits connection only: the daemon separately checks each operation.
It does not grant access to physical keyboards or world-writable devices.
Existing user systemd managers may need a fresh login/session for updated limits;
check the limits in the actual producer process.

`viskad.service` runs persistently with `Type=notify`, a systemd-managed
`/run/viska`, core dumps disabled, bounded locked memory, no capabilities, read-only
system filesystem, and restricted devices. It registers its keyboard before
notifying systemd. Socket activation is deliberately not used.

The root-controlled `/etc/viska/config.json` is generated using `pkgs.formats.json`:

```json
{
  "keyboard_layout": "us",
  "ttl_seconds": 45,
  "max_payload_bytes": 1048576,
  "backend": "uinput",
  "authorization": {
    "store_users": ["producer"],
    "type_users": ["trigger"],
    "clear_users": ["trigger"],
    "status_users": ["producer", "trigger"]
  }
}
```

Unknown fields, duplicate keys, bad types, unsupported layouts, and invalid limits
fail startup. Lists default to empty (deny). Username lookup is repeated for each
request; a missing account or lookup error denies the operation. Configuration
changes restart the daemon and clear pending data; live reload is not supported.

Optional JSON `submission_socket` and `control_socket` fields select absolute
paths for isolated operation. Their parent must be owned by root or the daemon
and not writable by clients. The CLI uses `/run/viska` unless `--socket-dir PATH`
or `VISKA_SOCKET_DIR` selects a test directory. `backend: "mock"` is for tests only.

To remove: set `services.viska.enable = false` (or remove the module), rebuild,
and verify `viskad.service` is stopped. No secret history or persistent secret
files exist. NixOS may retain the unused system account/group; review those
separately if removing them is desired.

## Behavior and constraints

- `store` reads stdin to EOF without removing a newline. It succeeds only after
  complete validation and atomic installation. Invalid data preserves an older
  valid pending value. The byte limit defaults to 1 MiB and can be lowered.
- Input must be UTF-8 and directly typable on the selected basic layout. Newline,
  tab, NUL, other controls, invalid encoding, and dead-key/composition sequences
  fail explicitly. No normalization, substitution, clipboard use, or Enter.
- `type` consumes before injection. Failure, disconnect, cancellation, or timeout
  never restores the value. A partial prefix may already have been typed.
- `clear` cancels staging and pending data; during injection it stops further
  typing and attempts key release. Failed release disables the backend.
- Status contains only state, remaining TTL, and backend readiness, never content
  or length. Commands return nonzero on failure and do not retry automatically.
- TTL is 1–300 seconds (default 45), measured from installation with suspend-aware
  `CLOCK_BOOTTIME`. Expiry is checked proactively and before consumption.
- One staged submission is permitted; concurrent stores may return busy. Incoming
  data expires after five seconds idle or thirty seconds total. Stores and extra
  triggers are rejected while injecting.
- Typing is paced, with a five-minute deadline. A 1 MiB value can be stored, but a
  sufficiently long injection will reach that deadline and stop after a prefix.
- Matching a username authorizes its processes, not a human gesture. Any authorized
  trigger can consume the shared pending value, regardless of who submitted it.
- Focus may change midway. Lock screens, other VTs, or unintended applications may
  receive events. Multi-seat isolation is not guaranteed. Match the session layout;
  Caps Lock, held modifiers, input methods, or remapping can change output.

Bind `viskactl type` using an existing shortcut system; release shortcut modifiers
before injection. A dedicated USB button/listener can invoke the same command,
but hardware handlers are outside v1 and a button does not authenticate a destination.

## Tests and development

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build
python3 tests/integration.py
```

```sh
nix flake check --no-build
nix build .#checks.x86_64-linux.unit-tests
nix build .#checks.x86_64-linux.layouts
nix run .#test
nix build .#checks.x86_64-linux.integration
```

`unit-tests` builds the package and runs Rust tests. `nix run .#test` runs the
unprivileged real-socket/mock suite on the host with private temporary endpoints,
without installing a service. This suite is also run inside the NixOS integration
VM. It is not run inside Nix's build sandbox: remapped root ownership there is
deliberately rejected by the daemon's endpoint checks. The separate NixOS check
uses QEMU/KVM and separate UIDs, including a user deliberately given socket access
without daemon authorization. Its builder needs the `kvm` system feature and
access to `/dev/kvm`; configure a suitable local/remote builder if unavailable.
The interactive VM's software fallback does not make the automated KVM check pass.

Layout fixtures are generated from xkeyboard-config **2.41**, model `pc105`, basic
`us`/`se`. See `layouts/SOURCES.json` and `scripts/generate-layouts.py`; generation
uses libxkbcommon as a development tool only. Main alphanumeric ISO keys are in
scope; keypad/NumLock and composition are not. Tests include all fixture entries
and independently known Swedish letter and modifier positions.
The `layouts` check regenerates all entries from the hash-pinned upstream archive
using libxkbcommon and compares them with the checked-in tables and source hashes.

### Manual host smoke test

After installation, use a disposable session with the configured layout, modifiers
released and Caps Lock off. Store nonsensitive text as the producer, then run
`sleep 3; viskactl type` as the trigger and focus a disposable editor. Verify case,
punctuation, Swedish characters when configured, absence of appended Enter, and
one-shot behavior. Test expiry, clear, service restart, and unauthorized users.
Do not use a lock screen or a real credential as a test destination.

See [protocol](docs/PROTOCOL.md) and [security review notes](docs/SECURITY.md).
