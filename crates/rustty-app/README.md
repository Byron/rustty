# Rustty for macOS

Rustty is a private Rust workspace implementing a terminal, CoreText font services,
a portable frame renderer and a WGPU desktop application. All seven crates are
`publish = false`. WGPU uses Metal on macOS; Windows and Linux desktop integration
and a direct Metal renderer are deferred.

Build a native app from the repository root:

```sh
nu crates/rustty-app/build.nu --release
open target/release/Rustty.app
```

For development, use `cargo run -p rustty-app --bin rustty`. Build the debug bundle
first (`nu crates/rustty-app/build.nu`) to make themes, terminfo and shell integration
available to development sessions. Ordinary Cargo builds do not require Zig.

Rustty loads its own configuration when present, otherwise Ghostty Local settings,
then stable Ghostty settings. `rustty --config-info` reports the selected files.
Own settings can be stored in `~/.config/rustty/rustty.txt` or
`~/Library/Application Support/com.rustty.app/rustty.txt`. Legacy `config` and
`config.rustty` files remain supported; an empty own file
intentionally disables Ghostty fallback. No Ghostty file is modified.
Configuration diagnostics appear in the app. The native menu provides reload and
open-configuration actions. Settings opens the selected configuration file,
including Ghostty's file while using fallback, so opening it does not create an
empty Rustty override. Settings always uses the default text editor, including
for existing files with an unregistered extension. Light/dark theme pairs follow macOS appearance, and
reload keeps the original command-line overrides. Terminal appearance and visibility
queries reflect the OS scheme and whether the pane is currently shown.
Tabs, splits, zoom, quadrant navigation, clipboard and
search use the configured Ghostty keybindings. Window layouts and pane directories
are saved separately under `com.rustty.app`.

File → Open Saved Layout lists the last saved layouts from Ghostty Local,
Ghostty and Rustty, and can browse for another Rustty workspace JSON or Ghostty
saved-state folder. Imported tabs and panes open in new windows with fresh shells
in their saved directories. Existing sessions and the source files stay intact;
the imported layout participates in Rustty's normal saving and undo history.

Tab colors apply to tab controls, split focus outlines and completion flashes;
unassigned tabs use the macOS system accent. Inactive tabs and directory labels
underline running commands, while `▶` counts panes explicitly reporting work
through a leading activity spinner or progress report. Waiting-for-input titles
do not count as active work. Each pane can trigger a short completion flash even
while another pane remains busy. Quadrants share one directory label when every
pane has the same basename; differing or missing directories keep individual labels.

OSC 9;4 progress reports show a thin bar at the top of their pane, with percentages,
red errors, orange pauses and animated indeterminate progress. Reports disappear
when removed or after 15 seconds without an update. Ghostty's `progress-style = false`
setting disables them. Determinate bars do not schedule animation frames.

The terminal port is still undergoing differential compatibility work. A passing
smoke test does not establish full libghostty-vt parity. The exhaustive coverage
gate in `test/rustty/coverage.json` records unfinished protocol and snapshot work;
see [the compatibility checks](../../test/rustty/README.md). Native global shortcuts
need macOS Accessibility permission. Notifications are available in the bundled
app; permissions remain under macOS control.

Validation:

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 test/rustty/parity.py
RUSTTY_SMOKE_DIR=/tmp/rustty-native-smoke target/debug/Rustty.app/Contents/MacOS/rustty
```

The opt-in native smoke check starts disposable `/bin/sh` sessions, checks input,
four split panes, tabs, quadrant focus and zoom, URI directory reports, restoration
and idle rendering. It writes `result.json`, `workspace.json` and a WGPU readback
`window.png` into the specified directory and exits. It uses the selected display
configuration but does not restore or overwrite the regular app workspace.

On a locked or headless Mac, set `RUSTTY_SMOKE_OFFSCREEN=1` for an offscreen
Metal capture of the same host primitives. The report labels this mode; it does
not verify that macOS presents the window on a physical display.
The native smoke report includes event counts during its idle phase, distinguishing
unchanged cursor events, actual pointer movement and egui repaint requests.
Set `RUSTTY_SMOKE_HOVER=1` to require a stationary pointer over a shell pane
during the idle interval. Move the pointer into a test pane before the 45-second
timeout; leaving the pane or moving the pointer restarts the interval.
