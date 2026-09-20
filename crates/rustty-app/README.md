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
`title-report = true` enables CSI 21 t replies with the terminal title; it is
disabled by default and follows configuration reloads.
New terminals default to `grapheme-width-method = unicode`, matching Ghostty's
emoji widths. The `legacy` setting remains available; changing this setting
applies only to new terminals.
Tabs, splits, zoom, quadrant navigation, clipboard and
search use the configured Ghostty keybindings. Window layouts and pane directories
are saved separately under `com.rustty.app`.
Find opens in the upper-right corner of its pane without resizing terminal content.
Each pane keeps its own query; clicking a terminal leaves its Find overlay open.
Enter returns focus to the terminal while keeping Find open. Shift+Enter,
the arrow buttons and configured search shortcuts navigate matches.
Escape closes the focused pane's Find.
All matches stay highlighted in yellow, with the selected match in peach.
`search-unfocused-opacity = 0.8` controls the whole overlay's opacity when its
controls or window lose focus; values from 0 through 1 are supported and reload
immediately. This is independent of `unfocused-split-opacity` and its dimming color.
Holding Command underlines the openable link under the pointer, including OSC 8
links and detected URLs. Command-click opens the highlighted target.

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

For CLI compatibility, sessions advertise `TERM_PROGRAM=ghostty` and the
bundled `TERM=xterm-ghostty`; XTVersion replies also identify as `ghostty`,
with Rustty's package version. An existing shell can enable Cargo progress
immediately with `TERM_PROGRAM=ghostty cargo check`.

The terminal port is still undergoing differential compatibility work. A passing
smoke test does not establish full libghostty-vt parity. The exhaustive coverage
gate in `test/rustty/coverage.json` records unfinished protocol and snapshot work;
see [the compatibility checks](../../test/rustty/README.md) and the
[CSI/OSC implementation inventory](../../test/rustty/SEQUENCES.md). Native global shortcuts
need macOS Accessibility permission. Notifications are available in the bundled
app; permissions remain under macOS control.

Validation:

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo +1.95.0 run --release --offline -p rustty-vt --example parity-runner
RUSTTY_SMOKE_DIR=/tmp/rustty-native-smoke target/debug/Rustty.app/Contents/MacOS/rustty
```

The opt-in native smoke check starts disposable `/bin/sh` sessions, checks input,
four split panes, tabs, quadrant focus and zoom, URI directory reports, restoration,
progress animation across pane focus changes, hover scrolling, file-drop targeting,
reverse video, DEC column mode, text blinking, synchronized output, hidden-tab title
updates, independent Find overlays without terminal resizing, and idle rendering.
The report records animation frame rate, the monitor's reported refresh rate, and
window redraws and pane preparations during hidden-tab title updates. It writes
`result.json`, `workspace.json` and a WGPU readback `window.png` into the specified
directory and exits. It uses the selected display configuration but does not
restore or overwrite the regular app workspace.
Find checks also capture `find-focused.png` and `find-unfocused.png` to verify
the overlays and their transparency.

On a locked or headless Mac, set `RUSTTY_SMOKE_OFFSCREEN=1` for an offscreen
Metal capture of the same host primitives. The report labels this mode; it does
not verify that macOS presents the window on a physical display.
Offscreen smoke checks keep the test host rendering if macOS occludes its window.
The native smoke report includes event counts during its idle phase, distinguishing
unchanged cursor events, actual pointer movement and egui repaint requests.
Set `RUSTTY_SMOKE_HOVER=1` to require a stationary pointer over a shell pane
during the idle interval. Move the pointer into a test pane before the 45-second
timeout; leaving the pane or moving the pointer restarts the interval.
