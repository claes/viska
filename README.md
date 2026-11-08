# Viska

Viska delivers sensitive text once through a Linux virtual keyboard without using
the desktop clipboard. `viskad` keeps one pending value per channel in protected memory;
`viskactl send` submits stdin and a separately authorized account runs
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
printf '%s' 'example-secret@123' | viskactl send
viskactl status
viskactl type
viskactl type   # fails: consumed already
```

Successful send/type/clear commands are silent. The mock exercises event generation
but never types into host applications. Status returns only metadata. Exiting the
shell stops the daemon and removes temporary runtime files.

For Swedish:

```sh
VISKA_DEMO_LAYOUT=se nix run .#demo
# In the demo shell:
printf '%s' 'räksmörgås ÅÄÖ @ €' | viskactl send
viskactl type
```

Both daemon and CLI fail closed when sensitive memory cannot be locked. Check
`ulimit -l`: allow at least 8192 KiB for the demo and producer CLI. You may raise the
soft limit up to your existing hard limit (`ulimit -Hl`); otherwise your login's
memory-lock limit must be provisioned by an administrator. No unsafe fallback is
provided. A daemon needs two maximum-sized allocations per channel plus scratch space.

### Disposable graphical VM: real guest typing

```sh
nix run .#test-vm
```

This builds a separate NixOS configuration with the actual module, separate
producer/trigger/reader/outsider accounts, a Swedish graphical session, and real guest
uinput. It does not import your host configuration. A graphical display is needed;
KVM is recommended, with slower QEMU software emulation as fallback.

The desktop logs in as `trigger`. Open Mousepad as the destination and two terminals.
In one terminal:

```sh
su - producer
# VM-only password: producer
printf '%s' 'räksmörgås ÅÄÖ @ €' | viskactl send
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
              sendUsers = [ "producer" ];
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
    "send_users": ["producer"],
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

## Migration to 0.3.0

Update the client and daemon together: this release uses protocol v3 and clears
pending values on service restart. Rename `viskactl store` to `viskactl send`, JSON
`authorization.store_users` to `authorization.send_users`, and NixOS
`authorization.storeUsers` to `authorization.sendUsers`. Old names are rejected.
The IPC filenames remain unchanged. View `viskactl --help` or `viskactl send --help`
without contacting the daemon.

## Restrict a value to a recipient

```sh
printf '%s' 'hello' | viskactl send --channel work --recipient alice
```

The example username must already appear in `typeUsers` or `outputUsers`, and must
resolve on the running system. The restriction narrows existing permissions: alice
still needs `outputUsers` to retrieve bytes or `typeUsers` to request typing. Every
other account is denied both operations on this value, even if normally authorized.
Omit `--recipient` for the previous shared-channel behavior. Names are public metadata.

Invalid/unlisted/missing recipients reject the submission without replacing the
previous value. The daemon resolves the recipient again before consumption, binding
that check to the specific value's generation so replacements cannot reuse stale
authorization. A waiting consumer for another user is rejected and releases its
reservation; the value remains pending for its recipient. Clear and status keep
their ordinary permissions. A replacement carries its own optional recipient;
restrictions are per value, not permanent channel settings.

Unix accounts sharing a UID are indistinguishable. Restricting who invokes typing
does not authenticate the currently focused destination.

## Channels, output, and waiting for a copy

All commands accept `--channel NAME`; omitted means `default`. A channel contains
one pending value, independently of other channels. Channels share allowlists;
channel names are public metadata, not access-control boundaries. Names are
case-sensitive, at most 64 ASCII letters/digits/underscores/hyphens, beginning with
a letter or digit. Never put sensitive content in channel names.

```sh
printf '%s' 'first' | viskactl send
printf '%s' 'second' | viskactl send --channel work
viskactl output --channel work  # exact bytes: second (no newline)
viskactl type                  # consumes default through the keyboard
```

`send` accepts arbitrary bytes, including newlines, NUL, and invalid UTF-8. It
never strips or changes input. `output` consumes once and writes the exact bytes.
`type` validates UTF-8 and the configured layout before consuming; unsupported
values remain pending and can be retrieved using `output`. It never synthesizes
Enter for newline. All other operations and errors remain metadata-only.

Output requires separate authorization, which defaults to deny:

```nix
services.viska.authorization.outputUsers = [ "reader" ];
services.viska.maxChannels = 8; # default; configurable from 1 to 64
```

The JSON equivalents are `authorization.output_users` and `max_channels`.
Type permission never implies output permission. Output users receive socket access
and PAM memlock provisioning just as producers do. Configuration changes restart
the service and clear all channels; update client and daemon together (protocol v3).

For reverse-order delivery, start the consumer first:

```sh
# Terminal A: waits indefinitely, then prints a future value.
viskactl output --channel work --sync
# Terminal B:
printf '%s' 'hello' | viskactl send --channel work
```

`type --sync` works the same way, but injects keyboard events. Existing values are
consumed immediately. Only one consumer can wait on each channel; another returns
busy. `clear --channel work`, Ctrl-C, disconnection, and daemon shutdown cancel
waiting. Authorization is checked again before delivering a future value.
`--sync` is only supported on type/output. Store still acknowledges installation,
without waiting for consumption. Status reports waiting and outputting states too.

For a shortcut, `viskactl type --delay-ms 500` waits 500 milliseconds before
connecting to the daemon. The default is zero; the argument must be an unsigned
integer in milliseconds fitting in `u64`. This gives you time to release the
shortcut modifiers; it does not detect their release. For example, with
plasma-manager:

```nix
hotkeys.commands.viska-paste = {
  name = "Viska Paste";
  key = "Meta+Shift+V";
  command = "${osConfig.services.viska.package}/bin/viskactl type --delay-ms 500";
};
```

The option is only supported on `type`. With `--sync`, the delay happens before
registering the waiting consumer. Ctrl-C or SIGTERM during the delay cancels
without submitting a request. The delay does not reserve or extend the lifetime
of a pending value: it can expire, be replaced, cleared, or consumed by another
request. A clear during the delay cannot cancel the future request, since it has
not yet reached the daemon. Once submitted, normal authorization and one-shot
delivery rules apply.

- TTL is 1–300 seconds, default 45, starting at installation and including suspend.
- Default payload limit is 1 MiB per value. Replacement never installs partial data.
- The channel limit counts pending values, staging, waiting, and active delivery.
  Empty inactive entries are reclaimed; status/clear do not allocate unknown names.
- Different channels may submit and output concurrently. One staged send is allowed
  per channel; stores to a channel actively delivering a value return busy.
- One global keyboard can inject at a time. Synchronous typing waits for availability;
  ordinary typing returns busy. Waiting does not extend a pending value's TTL.
- Once delivery starts it has a five-minute deadline, and failures never restore the
  value. Output completion means stdout writes completed, not downstream processing.
  Broken pipes, cancellation, and device errors can leave partial output.
- Values and event buffers remain locked and zeroized. The NixOS service provisions
  memory for two maximum-sized buffers per channel plus scratch space. Local mock
  demos default to two channels; raise `VISKA_DEMO_MAX_CHANNELS` and your memlock
  limit together if more are needed. At least 8192 KiB is recommended for the demo.
- Matching a username authorizes its processes, not a human gesture. The focused
  destination is not authenticated. Caps Lock, held modifiers, remapping, input
  methods, layout changes, focus switches, lock screens, VTs, and multiple seats
  can change or redirect typing. A hardware listener remains outside v1.

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
