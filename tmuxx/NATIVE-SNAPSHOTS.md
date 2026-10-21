# Native transfer and snapshot limits

Native protocol version 2 uses MessagePack maps with named fields and binary byte strings. It replaces version 1's newline-delimited JSON and base64 snapshot chunks. Clients, SSH bridges, and owners must be updated together; restart existing owners before reconnecting. Stock tmux command/control transports are unchanged.

Each record consists of one or more independently compressed blocks. The 16-byte header is `TMX2`, one flags byte (`1` ends the record, `2` means LZ4), three zero reserved bytes, then little-endian `u32` encoded and decoded lengths. Each block contains at most 256 KiB before compression. LZ4 blocks have no embedded size prefix; the header supplies the bounded decoded size. A compressed block is sent only when it is smaller. Raw blocks have equal encoded and decoded lengths. Every nonfinal block decodes to exactly 256 KiB. Receivers reject unknown flags, invalid lengths, oversized records, truncated blocks, and invalid MessagePack. Each endpoint separately caps complete decoded records before deserialization: owners accept requests up to 16 MiB and Rustty accepts server records up to 128 MiB.

Compression is on by default for native sockets, named pipes, and SSH stdio. It applies to terminal output, commands, metadata, and snapshot chunks without waiting for a full screen or choosing a mode based on the program. Block boundaries are transport boundaries; one record may span several blocks. MessagePack removes JSON numeric-array expansion for bytes, and LZ4 reduces repetitive terminal data while bounding compression/decompression scratch space to a block. The decoded terminal and its retained history still occupy memory on each viewer.

Native snapshots describe the live owner; they do not checkpoint an operating-system process. A reconnect receives one immutable graph and terminal snapshot, then subsequent ordered events. Disconnecting or rejecting a snapshot does not remove the owner's panes, images, or PTY processes. Ordinary focus changes use selection updates rather than new snapshots.

Snapshots always stream as `snapshot_start` (instance, sequence, total MessagePack bytes), ordered `snapshot_chunk` records (byte offset and binary payload), and `snapshot_end` (sequence). Each chunk contains at most 256 KiB of serialized snapshot data and uses the same block compression as other records. Terminal checkpoints and image pixels remain binary data throughout the native transfer.

`tmuxx_protocol::native::SnapshotAssembler` checks the declared limit before creating a private temporary file, checks offsets and chunk sizes before writing, and publishes `ServerMessage::Snapshot` only after validating the end and parsing the complete MessagePack. Malformed, interrupted, and abandoned transfers delete their temporary file. Call `finish()` at EOF to detect a partial transfer. An explicit `SnapshotAborted` leaves the receiver's existing graph untouched and reports the error.

The default total limit is 8 GiB of serialized snapshot data before transport compression. `Options::snapshot_bytes` can lower the server limit; the assembler can impose a smaller receiving limit. A rejection is explicit and recoverable: reconnect with a sufficient receiving budget, or reduce the owner's retained state through ordinary commands. No valid image is silently omitted. Streaming keeps each transport record below the receiving record limit even when a complete snapshot is larger.

One snapshot may be pending per client. Further requests coalesce until it has been delivered; the immutable snapshot is accounted separately from the 64 MiB live-output queue, so one valid large snapshot does not trigger the slow-client policy. Subsequent output remains bounded and ordered after the snapshot.

The terminal wrapper's image restore budget defaults to 1 GiB per screen, matching the server's default global budget. `Snapshot::restore_with_image_limit` can enforce a caller-selected lower limit before replacing a receiving terminal. The underlying generic Rustty view decoder retains its independent 400 MiB default unless the caller uses the explicit limit API.

C tmux has no equivalent native snapshot API. Its Unix IPC and SSH control protocols keep their existing framing. C image differential tests check source/placement admission, lifetime, replies, and rendered pixels; native tests separately verify atomic transfer, small-limit rejection, retry, and preservation of the same live PID and image data.

## Measured snapshot overhead

A local release benchmark on Apple M4 Max (Rust 1.99, 2026-10-21) filled one
120×36 terminal with 1,000 varied colored log lines and retained its 498,177-byte
checkpoint. The comparison uses the previous checkpoint-v2 JSON state and its
exact streamed JSON/base64 writer at `61876226d`, not the older expanded screen
snapshots.

| Measurement | Previous JSON/base64 | Typed MessagePack/LZ4 |
| --- | ---: | ---: |
| Retained snapshot heap | 16.38 MB | 0.95 MB |
| Peak heap while constructing the snapshot | 17.33 MB | 1.17 MB |
| Complete snapshot wire bytes | 1,730,974 | 248,156 |
| Median encoding time, five runs | 7.915 ms | 0.719 ms |

Heap figures count requested live allocation bytes through a `System` allocator
wrapper, relative to the already-populated terminal. Only the peak is reset
between measurements. They exclude allocator metadata, process RSS, and the
terminal itself; encoding writes to a byte counter rather than accumulating the
wire output. The previous state is reconstructed by converting the same typed
snapshot to a JSON value, including its numeric checkpoint array. Timings include
allocation instrumentation and describe this fixture, not remote-host latency.
