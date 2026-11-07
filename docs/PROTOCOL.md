# Local IPC protocol v1

Two pathname AF_UNIX/SOCK_SEQPACKET sockets: `submit.sock` and `control.sock`.
Connections carry exactly one request. Authenticate using SO_PEERCRED; the
submission endpoint checks store authorization before reading any payload.
The control endpoint reads only an operation byte before checking its allowlist.
No secret retrieval operation exists. Replies are single JSON metadata packets,
`{"ok":true}` or `{"ok":false,"error":"fixed public message"}`. Status additionally
returns `state`, `ttl_remaining_ms`, and `backend_ready`.
If rejection closes a connection with unread packets, Linux may report a reset
instead of delivering the metadata error packet. This still rejects the submission
and does not replace the existing pending value.

Submission records:

1. A 16-byte header: bytes `56 49 53 4b 41 00 01 00` followed by an unsigned
   little-endian 64-bit declared byte length (0–configured maximum, at most 1 MiB).
2. Nonempty raw data packets of at most 16384 bytes, totaling exactly that length.
   UTF-8 boundaries need not coincide with packets. Zero-byte values skip this step.
3. One packet containing the seven ASCII bytes `COMMIT1`.
4. Client calls `shutdown(SHUT_WR)` to establish end-of-record. Any further packet
   is an error; no installation occurs before this EOF.
5. Daemon validates complete UTF-8 and layout coverage, atomically replaces the
   pending value, starts TTL, and sends an acknowledgment.

A client disconnect after completion can still leave a successfully installed
value whose acknowledgment was lost. Client success requires the acknowledgment;
failure after sending is an uncertain outcome, not permission to retry.
Unexpected EOF, oversize/truncated packet, excess bytes, or malformed records
cancel staging while preserving a valid pending value. Receive buffers are locked
and wiped even after recv errors/truncation. No read-equals-message assumption is
made for stdin: it is read repeatedly until EOF into a bounded locked allocation.

SO_PASSCRED record metadata distinguishes an empty packet from actual EOF;
empty packets are rejected, including after COMMIT1. Application authorization
still uses connection SO_PEERCRED. Unexpected ancillary data is rejected and any
received SCM_RIGHTS descriptors are closed, including before truncation errors.

Control packet: one byte `t` (type), `c` (clear), or `?` (status). Keep the connection
open for the reply. Type replies only after completion/failure; client disconnect
cancels ongoing injection without restoring the secret. Additional data during
injection is a protocol error and cancels it.

At most 32 accepted clients, listen backlog 16, one staging connection, and one
NSS worker with eight queued jobs/results. Authorization has a two-second deadline;
other idle requests have five seconds, and submissions have thirty seconds total.
Expiry and cancellation run independently of stalled NSS. A permanently stuck NSS
module can deny further work until restart but cannot create unbounded threads.
