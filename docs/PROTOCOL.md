# Local IPC protocol v2

Use matching daemon and CLI versions. Submission and control remain separate
AF_UNIX/SOCK_SEQPACKET pathname sockets with SO_PEERCRED authentication.

The first packet is JSON metadata, at most 512 bytes, with required `version:2`
and `operation` (store/type/output/clear/status), optional `channel` (default
"default"), optional `sync:false`, and `length` required only for store. Unknown
fields, duplicate keys, invalid channel names and unsupported versions are errors.
Only type/output allow sync. Store is accepted only on the submission endpoint.
Authenticate the requested role before reading secret payloads. Never put secrets
in the JSON metadata. Output has its own output_users username allowlist.

Store sends nonempty raw packets up to 16384 bytes until its declared byte length
is reached, then a COMMIT2 packet and shutdown(SHUT_WR). EOF is required before
atomic installation. Empty records, excess data, truncation, ancillary descriptors,
incomplete transfers, and limits fail closed without replacing existing data.
Zero-byte values send no data packets. Input is arbitrary bytes, not necessarily
UTF-8 or typable. The daemon acknowledges installation with {"ok":true}.

Type validates the complete pending value before atomic consumption, then replies
only after injection. Output atomically consumes and sends {"ok":true,"length":N},
raw packets totaling N bytes, COMMIT2, then shutdown(SHUT_WR). The CLI validates
the entire transfer into locked memory before writing exact bytes to stdout.
It sends DONE2 after successful stdout writes. The channel remains occupied until
completion/cancellation/disconnect or the five-minute delivery deadline. No failure
restores consumed data. Successful stdout writes do not prove downstream processing.

Other replies contain JSON metadata only; errors use fixed public strings. A
rejected connection may reset when unread packets remain. Status includes state
(empty/pending/waiting/injecting/outputting), remaining TTL and backend readiness,
but no secret bytes or length. Status and clear apply to the selected channel.

Synchronous consumers wait indefinitely and reserve a channel; only one consumer
may wait. Resolve authorization on entry and again before delivery. Detect abandoned
waiters before taking values. Clear cancels waiting and active operations and
staging on the selected channel. Normal store success never waits for consumption.

Retain a 32-client cap, one bounded NSS worker, two-second authorization deadlines,
five-second idle request limits, and thirty-second submission deadlines. Idle
consumer waiting is exempt. Each occupied channel permits one pending or delivering
value plus one staging allocation; per-channel and global channel caps bound memory.
SO_PASSCRED distinguishes empty packets from EOF; authorization uses SO_PEERCRED.
Unexpected ancillary data is rejected and received descriptors are closed. Always
wipe receive buffers even on truncation/error. Never retry ambiguous delivery.
