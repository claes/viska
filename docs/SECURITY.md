# Security review notes

The daemon owns all pending values. There is no clipboard use or implicit secret read permission; output is a separately authorized read API. There is no
keyring, history, or intentional disk storage. Each bounded channel owns at most one protected pending/delivering value and one staging value. State transitions run in one event loop; consuming
moves ownership before the first key event. Invalid submissions preserve pending state. Arbitrary bytes are allowed; type-time validation preserves untypable values for output. Failed injection never re-enables replay.

## Memory boundary

CLI stdin is read with read(2) directly into anonymous mmap memory, avoiding Rust
Stdin's internal buffering. Buffers are fixed-capacity, mlocked, MADV_DONTDUMP,
non-Clone and non-Debug. Drop zeroizes full mappings before munlock/munmap. Process
core limits and dumpability are disabled before reading sensitive input. Event
queues and held-key bookkeeping also use protected mappings. Individual character,
keycode, and modifier locals are wiped where practical; compiler registers, stack
copies, CPU state, and third-party internals are not a formal noninterference proof.

mlock does not cover kernel pipe/socket/input buffers or the producer's own memory.
Hibernation can persist RAM. SIGKILL/power loss bypass orderly cleanup; the kernel
reclaims mappings, without a userspace wipe guarantee. Root, a compromised daemon,
or a compromised input stack defeats this boundary. Panic messages are replaced
with a fixed message; release builds abort on panic rather than expose payloads.

## Authorization boundary

SO_PEERCRED identifies the connection's credentials at connect time. An account
can pass its connected socket to another process; authorization cannot distinguish
that deliberate delegation. Runtime NSS lookups compare explicitly configured
names to peer UIDs for each operation. Lookup failures deny access; no startup UID
cache is trusted. NSS is a system-trusted dependency, isolated from the state loop
with one bounded worker. A stuck module may require restart to restore availability.

Endpoint parents must be service/root-owned and not client-writable. NixOS
provisions a dedicated socket access group and a separate service-only uinput
group; client groups never grant application roles. Removing a username from
configuration restarts the service. Changed client group membership may require
new login sessions, but lingering socket access does not bypass daemon checks.

The production service has no Linux capabilities. uinput DAC and systemd's device
policy are both required. A compromised daemon can misuse its uinput descriptor;
limiting advertised keys is not a complete sandbox against a compromised process
that can create more virtual devices. No access to physical keyboard devices is
requested. Do not add broad input groups for convenience.

## Delivery boundary

Configured layout is an administrator assertion, not observed desktop state.
Mapping fixtures use Shift/AltGr without composition on main ISO alphanumeric keys.
Lock modifiers, active layout changes, remapping, input methods, focus switches,
VT changes, session locks and multi-seat setups can redirect or alter output.
Service readiness confirms local setup, not compositor discovery or app receipt.

Device errors may occur after a prefix was emitted. Cleanup is bounded best-effort;
if key release fails, the backend is disabled until restart. Five-minute injection
timeouts can truncate very large inputs. Physical buttons and shortcuts are only
triggers, not destination authentication or secure attention mechanisms.

## Verification boundaries

Rust tests and unprivileged real-socket/mock tests do not prove cross-UID OS access
or real input-stack behavior. The NixOS VM test exercises account separation and
systemd deployment. The graphical VM exercises guest uinput with Swedish explicitly
selected. A manual host test remains necessary for the actual session. Always
report which checks ran; never infer real-device success from a mock test.

## Channels and output

Channels share operation allowlists and are not security boundaries. output_users
defaults to deny independently of typing. Output bytes are intentionally disclosed
to the authorized consumer and its stdout destination, including pipe/file/terminal
buffers; protection cannot follow them into downstream applications. Binary values
never pass through JSON or logs. The CLI validates the entire bounded output into
locked memory before stdout, zeroizes it, and acknowledges successful writes.
Delivery consumes before emission; broken pipes and partial failures never restore it.

One synchronous consumer reserves a channel. Waiting is indefinite but bounded by
client/channel caps. Disconnects are checked before consumption and usernames are
resolved again before delivery. Clear cancels that channel's waiters/staging/active
delivery only. Pending TTL is not extended by waiting. Active delivery is bounded
to five minutes. NixOS sizes locked-memory limits for aggregate channel storage.

## Recipient restrictions

Recipient usernames narrow the consumer allowlists for one value, including both
stdout output and typing. They do not grant permissions, constrain clear/status,
or authenticate the destination of keyboard events. Missing/unlisted accounts fail
before installation. Consumption resolves the username afresh and checks peer UID.
Accounts sharing a UID remain equivalent. Globally unique value generations bind
NSS results to the value checked, preventing replacement/clear/recreation races
from reusing stale authorization. A denied waiter does not consume the value.
