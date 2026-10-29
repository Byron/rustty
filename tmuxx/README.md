# tmuxx in Rustty

tmuxx is Rustty's multiplexer. Its server, command client, native protocol and
terminal adapter are members of the root Cargo workspace. Rustty and tmuxx use
the same `crates/rustty-parser` and `crates/rustty-vt`; changes to those crates now
reach both the owner and its views in one commit.

Build from the repository root:

```sh
cargo build --locked -p tmuxx-cli
cargo build --locked -p rustty-app
target/debug/tmuxx -L rustty-demo -f /dev/null new-session -d -s work
target/debug/tmuxx -L rustty-demo list-panes -a
```

Rustty also embeds the CLI as `rustty tmux`. Its default is a native daemon;
standalone `tmuxx`, or explicit `-L`/`-S`, exposes the compatible tmux command
socket and its companion `.native` endpoint. Existing socket names, configuration
and commands are retained by the import. No running server is migrated or restarted.
Rustty's macOS and Windows bundles include `tmuxx` or `tmuxx.exe` beside the GUI
executable. Use the console `tmuxx.exe` for Windows commands and SSH access; it
shares the bundle's ConPTY resources with Rustty.

Generate shell completions with `tmuxx completions SHELL`, where `SHELL` is
`bash`, `zsh`, `fish`, `powershell`, or `elvish`. Commands, aliases, and flags
come from the CLI's command registry. Fish completions require fish 4.0 or
newer; install the script with:

```fish
mkdir -p ~/.config/fish/completions
tmuxx completions fish > ~/.config/fish/completions/tmuxx.fish
```

Fish also queries the running server for session, window, pane, client, and
buffer targets. For example, complete `tmuxx kill-pane -t ` or
`tmuxx -L work attach-session -t `. Queries honor the leading `-L`, `-S`,
`--native-endpoint`, and `--imsg-schema` options and the usual connection
environment. They use `-N`, so pressing Tab never starts a server. Session and
window targets include readable names and stable IDs; pane IDs carry descriptions
showing their session, window, and current command.

Windows servers inherit the account's Windows Terminal default shell and its
arguments at startup, including Git Bash's login flags. Explicit `default-shell`
or command options override that preference for new panes.

On startup, `tmuxx` and `rustty tmux` load `$XDG_CONFIG_HOME/tmuxx/tmux.conf`, or
`~/.config/tmuxx/tmux.conf` when `XDG_CONFIG_HOME` is unset, empty or relative.
Windows uses `%APPDATA%\tmuxx\tmux.conf`. A missing default file is ignored.
Explicit `-f PATH` arguments replace this default lookup; use `-f /dev/null` on
Unix to start without user configuration. The file loads once, before the first
session. Reload a running server with `tmuxx source-file PATH` to apply changes
to future panes; existing shells keep running.

Set `tmuxx-config-file` in Rustty's settings to select a file for GUI-launched
servers, including embedded servers. SSH clients use the remote server's settings. See
[Rustty configuration guide](../crates/rustty-app/README.md#tmuxx-windows) for examples.

Splits and new floating panes inherit the source pane's reported working directory
(OSC 7, including file URIs and kitty shell reports, or OSC 9;9). An explicit
`split-window -c PATH` or `new-pane -c PATH` takes precedence. Without a usable
local report, tmuxx uses the source process's detected directory or its starting
directory. Reports naming another host are ignored; directories that no longer
exist use the normal home-directory fallback when the child starts. New windows
and sessions retain their existing directory defaults.

An unmodified supported tmux client can send commands and use control mode with
tmuxx. Ordinary interactive tmux TTY attachment is unsupported: Rustty supplies
the pane UI. Conversely, Rustty can attach to stock tmux with
`--tmuxx=system-ssh:HOST`; that control-mode adapter remains independent of native
tmuxx features. See the [Rustty guide](../crates/rustty-app/README.md).

Native clients negotiate version and optional features in `Hello`. Native
protocol v2 uses MessagePack binary records with independent LZ4 blocks, including
live terminal output and streamed snapshots. It has a separate endpoint from
stock tmux command/control sockets; never send native messages to those sockets.
Update clients, bridges, and servers together and restart existing owners before
reconnecting. Version 1's JSON transport is no longer accepted.

Snapshots retain one binary terminal checkpoint per pane, including history,
plus explicit graph and agent state. They are streamed with bounded blocks and
installed atomically; ordinary focus changes use small selection updates. See
[transfer framing and limits](NATIVE-SNAPSHOTS.md).

Rustty validates the native greeting before sending queued commands. Window-size
connections also verify the primary connection's server incarnation, so a daemon
replacement cannot resize unrelated sessions that reuse the same numeric IDs.

## Clipboard forwarding

Application OSC 52 clipboard requests, including tmux-wrapped requests emitted
by Codex's fallback, use `set-clipboard on`. Rustty enables this for servers it
creates; existing and remote servers retain their settings. For those servers,
add `set -s set-clipboard on` to the tmuxx configuration or apply it to the running
server. The request goes to one eligible client, preferring a focused pane view,
and follows that client's clipboard access policy.

This compatibility path unwraps OSC 52 specifically, without enabling general
DCS passthrough. Fragmented requests resume through snapshots; completed requests
are not replayed when clients attach or reconnect.

## Source and maintenance

Imported from [Byron/tmux](https://github.com/Byron/tmux) commit
`700884116bc7239d032e6c91fb4e37e7c540c335` (2026-10-20), under its original
[license](COPYING). The Rust implementation derives its command/control behavior
from upstream [tmux/tmux](https://github.com/tmux/tmux) commit
`22f34d3fa984e03719b0a1b2ac4677aaf61cd53d` (2026-10-19, next-3.9).
That commit is an ancestor of the imported revision; the C production sources
are unchanged between them. `conformance/manifest.toml` also records this base.

The import copies `rust/crates`, supporting tests, conformance tools and Windows
packaging here. The formerly vendored parser/VT source and tests are folded back
into the existing Rustty crates. Their existing corpus, color data and snapshot
fixtures are reused. The patched portable-pty and POSIX matcher are retained
under `vendor/` because neither existed here; each retains its own license and
upstream notes. The only copied C source is `test-support/fixtures/tty-features.c`,
an immutable protocol bit-order test fixture. No C server is built or linked.

Rustty interop is the development priority. The recorded tmux base is a reference
for selectively porting fixes, not a requirement to track its history or recreate
its terminal UI. An external C checkout is needed only for optional upstream
metadata/oracle checks; normal builds and unit tests need no sibling repository.
Historical conformance results describe the imported revision, not this tree.

## Verify

```sh
cargo test --locked --offline -p 'tmuxx-*' -p rustty-parser -p rustty-vt
cargo test --locked --offline -p rustty --features sessions --lib tmuxx::
cargo test --locked --offline -p rustty-app --test tmux_cli
```

Ignored reference/PTY/SSH tests declare their requirements. Use disposable
endpoints when running them. See [conformance](conformance/README.md) for optional
upstream comparisons and [Windows](windows/README.md) for ConPTY packaging.
