# Reporting agent status from a Rustty pane

The receiving Rustty PTY binds each report to its pane. Two reporting panes in
the same directory remain separate. One serialized reporter owns each pane;
switching its displayed thread updates that pane's existing tile. The generic
emitter below is useful for a reporting program or a disposable demonstration.
It does not discover stock Codex sessions.

Build locally without replacing installed applications:

```sh
cargo build -p rustty-app --bin rustty
python3 test/rustty/agent-status-demo.py --rustty target/debug/rustty
```

Run the demo inside a pane of the Rustty build with the dashboard enabled. It
shows every state, Unicode, input resolution, cancellation, duplicate completed
turns, A→B→A thread switching, a new completion, and cleanup. Each step lasts
2.5 seconds (`--delay` changes this). Ctrl-C sends end through the same serialized
loop. It changes no configuration and does not create or resume a Codex thread.
For explicit captured fixtures, run `--self-test`; this checks each whole frame
without sending OSC to the terminal.

## Generic CLI

```text
rustty agent-status begin|update STATE [--label TEXT] [--thread-id ID] [--turn-id ID] [--raw]
rustty agent-status end [--raw]
```

Use these commands serially from one enclosing foreground program, for example:

```sh
#!/bin/sh
set -eu
RUSTTY_BIN=/absolute/path/to/target/debug/rustty
trap '"$RUSTTY_BIN" agent-status end' EXIT
"$RUSTTY_BIN" agent-status begin idle --label 'Review Δ'
"$RUSTTY_BIN" agent-status update working --label 'Review Δ' --thread-id A
# The enclosing program performs its own work here.
"$RUSTTY_BIN" agent-status update done --label 'Review Δ' --thread-id A --turn-id T1
# Keep the program alive while that completed turn is available to inspect.
```

Output goes only to the command's actual stdout, which must be a terminal. It
never opens a guessed `/dev/tty` or routes captured tool output to another pane.
`--raw` explicitly permits nonterminal stdout for fixtures or a caller's own
serialized passthrough; capture alone does not report to Rustty. Do not run
competing asynchronous hooks or teardown writers. A separate interactive shell
command may end at a shell integration boundary immediately after emitting begin;
use an enclosing reporter lifecycle rather than unrelated shell invocations.

## Private OSC v1

See [the sequence inventory](../test/rustty/SEQUENCES.md#private-agent-status)
for validation limits and VT behavior. The complete wire frame is:

```text
ESC ] 777;rustty-agent;1;BASE64(COMPACT_UTF8_JSON) ST
```

BEL or ordinary ST (`ESC \`) may terminate it. Encode the whole JSON document as
canonical standard padded base64; never splice a raw label into OSC syntax.
Examples of decoded payloads:

```json
{"op":"begin","state":"idle","label":"Review Δ","thread_id":"A"}
{"op":"update","state":"needs_input","label":"Review Δ","thread_id":"A"}
{"op":"update","state":"done","label":"Review Δ","thread_id":"A","turn_id":"T1"}
{"op":"end"}
```

Begin registers or replaces the pane's reporter. Update is a complete snapshot,
ignored before begin; omitted optional fields use pane-derived defaults. End
unregisters. Done is still registered. The states are `idle`, `working`,
`needs_input`, `done`, `error`, `paused`, and `unknown`. Use explicit pending
requests for needs_input, including nonblocking requests; clear it when the
request resolves or is cancelled. Silence, a timeout, or progress expiry does
not mean done. `turn_id` is required for done, stable across duplicate snapshots
and thread switches, and distinct for each completed turn. Generic single-thread
reporters may omit `thread_id`; a producer switching threads should provide it.

The application clears live registration when the pane/PTY exits or a trustworthy
enclosing foreground-command end is observed. A reporter should send end before
normal exit or suspend and begin with its current snapshot after resuming. RIS,
redraw, and compaction are not exit events. A crash without an observable cleanup
boundary can leave uncertain status until the next explicit report or pane exit;
there is no heartbeat-based completion inference. Programs sharing the PTY can
spoof its metadata. Messages cannot approve input, execute commands, select a
target pane, or request focus.

One reporter passed through a multiplexer can fit the protocol. Multiple tmux
panes reporting through one Rustty PTY violate v1's single-reporter contract.
The CLI adds no automatic tmux wrapping. SSH, tmux, and Windows ConPTY require
their own end-to-end verification; local protocol tests are not that evidence.
