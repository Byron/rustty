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

Windows servers inherit the account's Windows Terminal default shell and its
arguments at startup, including Git Bash's login flags. Explicit `default-shell`
or command options override that preference for new panes. Load server settings
with `-f PATH`, or set `tmuxx-config-file` in Rustty's settings for GUI-launched
servers; SSH clients use the remote server's settings. See the
[Rustty configuration guide](../crates/rustty-app/README.md#tmuxx-windows) for examples.

An unmodified supported tmux client can send commands and use control mode with
tmuxx. Ordinary interactive tmux TTY attachment is unsupported: Rustty supplies
the pane UI. Conversely, Rustty can attach to stock tmux with
`--tmuxx=system-ssh:HOST`; that control-mode adapter remains independent of native
tmuxx features. See the [Rustty guide](../crates/rustty-app/README.md).

Native clients negotiate version and optional features in `Hello`. The Unix
command protocol and native JSON protocol use separate endpoints; native JSON
must never be sent to a stock tmux socket. This already permits richer Rustty
interop without requiring extensions in stock tmux. Terminal snapshots have
their own version and validation. Compatibility with arbitrary future or older
snapshot formats is not implied by sharing a native protocol version.

Rustty validates the native greeting before sending queued commands. Window-size
connections also verify the primary connection's server incarnation, so a daemon
replacement cannot resize unrelated sessions that reuse the same numeric IDs.
Older daemons without an incarnation in their greeting are checked through a
read-only snapshot on that connection before any size commands are sent.

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
