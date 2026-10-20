# Native snapshot transfer and limits

Native snapshots describe the live owner; they do not checkpoint an operating-system process. A reconnect receives one immutable graph and terminal snapshot, then subsequent ordered events. Disconnecting or rejecting a snapshot does not remove the owner's panes, images, or PTY processes.

The native version-1 hello advertises `snapshot_stream: true`. Missing advertisement means a legacy peer. Negotiated peers receive `snapshot_start` (instance, sequence, total JSON bytes), ordered `snapshot_chunk` records (byte offset and base64 payload), and `snapshot_end` (sequence). Each chunk contains at most 256 KiB of decoded data. Images and animation frames also encode pixels as base64 strings; their deserializer accepts earlier numeric byte arrays.

`tmuxx_protocol::native::SnapshotAssembler` checks the declared limit before creating a private temporary file, checks offsets and chunk sizes before writing, and publishes the existing `ServerMessage::Snapshot` only after validating the end and parsing the complete JSON. Malformed, interrupted, and abandoned transfers delete their temporary file. Call `finish()` at EOF to detect a partial transfer. An explicit `SnapshotAborted` leaves the receiver's existing graph untouched and reports the error.

The default total is 8 GiB of serialized snapshot JSON, enough for the default 1 GiB global image budget represented in both screen views, with room for terminal state. `Options::snapshot_bytes` can lower the server limit; the assembler can impose a smaller receiving limit. A rejection is explicit and recoverable: reconnect with a sufficient receiving budget, or reduce the owner's retained state through ordinary commands. No valid image is silently omitted. Legacy peers must negotiate streaming to receive snapshots above 64 MiB.

One snapshot may be pending per client. Further requests coalesce until it has been delivered; the immutable snapshot is accounted separately from the 64 MiB live-output queue, so one valid large snapshot does not trigger the slow-client policy. Subsequent output remains bounded and ordered after the snapshot.

The terminal wrapper's image restore budget defaults to 1 GiB per screen, matching the server's default global budget. `Snapshot::restore_with_image_limit` can enforce a caller-selected lower limit before replacing a receiving terminal. The underlying generic Rustty view decoder retains its independent 400 MiB default unless the caller uses the explicit limit API.

C tmux has no native JSON snapshot API. The C Unix IPC and SSH control protocols keep their existing framing. C image differential tests check source/placement admission, lifetime, replies, and rendered pixels; native tests separately verify atomic transfer, small-limit rejection, retry, and preservation of the same live PID and image data.
