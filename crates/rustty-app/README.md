# Rustty for macOS and Windows

Rustty is a private Rust workspace implementing a terminal, native font services,
a portable frame renderer and a native desktop application. All crates are
`publish = false`. GPU rendering uses Metal on macOS and DirectX 12 with
DirectComposition on Windows. Windows also supports CPU rendering with GDI
presentation. CoreText and DirectWrite supply the respective native font backends.

## Windows 11 x64

Install Rust 1.95 or later and the Visual Studio C++ build tools/Windows SDK.
From PowerShell at the repository root:

```powershell
./crates/rustty-app/build.ps1 -Release
./target/release/Rustty/rustty.exe
```

Omit `-Release` for a debug build. The first build downloads Microsoft's pinned
ConPTY runtime and verifies its SHA-256. `-Offline` uses cached Cargo dependencies
and the runtime package under `target/conpty`.
The script builds for `x86_64-pc-windows-msvc` with a static C runtime; the portable
folder needs no Visual C++ redistributable. It contains the executable, ConPTY,
themes, shell integration, and license notices. Move the complete folder,
including `resources`, together. `-SkipBuild`
restages an existing script build. Cargo builds also work directly:
`cargo run -p rustty-app --bin rustty`. Build the portable folder first for resource
discovery during development. Zig, WSL, and an external shader compiler are not needed.

The bundled ConPTY preserves the order of application-controlled synchronized
updates (`CSI ?2026 h/l`), including cursor visibility. Some in-box Windows
versions forward the end marker before their queued screen/cursor update, causing
prompt animation flicker. Rustty loads its bundled runtime before creating any
sessions; it reports missing runtime files instead of silently using that older
transport. Both software and GPU rendering use the same synchronization guard.

To add this location to the Start Menu and enable notification activation, run
`rustty.exe --register`, or pass `-Register` to the build script. Registration is
per-user and requires no administrator access. Run `rustty.exe --unregister` before
moving or deleting a registered copy, then register the new location if needed.
Normal launches do not register or install anything.

On Windows, closing the last regular window exits Rustty and closes its hidden
quick terminal and shell sessions. Existing close confirmation also covers running
jobs in the quick terminal. Set `quit-after-last-window-closed = false` to keep
Rustty running for the global quick-terminal shortcut after closing its windows.

Layout changes are autosaved without waiting for activity to stop. Closing the
last window saves its layout before exiting, so the next launch restores its
tabs, splits, focused pane, window geometry and working directories. Set
`window-save-state = never` to disable layout saving and restoration.

Settings live in `%APPDATA%\Rustty\rustty.txt`; workspace state lives in
`%LOCALAPPDATA%\Rustty\workspace.json`. Explicit config files, XDG paths, and existing
Ghostty-format settings remain supported. `rustty.exe --config-info` reports both
the selected configuration and shell.

Windows defaults to `renderer = auto`: use a compatible hardware GPU when one is
available, otherwise render on the CPU. Adapter discovery excludes Microsoft's
software render driver before graphics-device initialization. If initial GPU
device or surface setup fails, `auto` also falls back to software. This fallback
applies at startup; it does not recover an already-running GPU device that is lost.
Use `renderer = software` (or `rustty.exe --renderer=software`) to force CPU
rendering, or `renderer = gpu` to force WGPU, including Windows' emulated adapter.
The renderer setting takes effect after restarting Rustty; macOS uses GPU rendering.
Both Windows paths retain native menus, per-pixel background opacity, and the same
terminal/UI drawing. Software animation follows the monitor refresh rate, with a
60 Hz default when it is unavailable. Idle windows do not continuously repaint.

When no Rustty command is configured, Rustty reads **only the default shell command**
from Windows Terminal's settings and profile fragments. It preserves its arguments,
including Git Bash's login flags. It does not import Terminal's working directory,
environment, appearance, fonts, or shortcuts. An explicit Rustty command or `-e`
takes precedence. An unresolved generated profile produces a diagnostic and uses
`ComSpec` (`cmd.exe`) as fallback. Git Bash supports cwd/title and command lifecycle
integration without editing user startup files; other commands can be launched
explicitly. The Windows bundle uses `xterm-256color` unless a compiled terminfo
directory is supplied.

Windows defaults use Ctrl+Shift+C/V for copy/paste, Ctrl+Shift+T for a tab,
Ctrl+Shift+D for a right split, Ctrl+Alt+D for a down split, Ctrl+Tab to switch tabs,
Alt+Enter for fullscreen, and Ctrl-click to open links. Ctrl+C and other ordinary
shell control keys remain available to the child. Configured bindings retain their
literal modifier meanings.

Native menus, taskbar badges, toast notifications, clipboard formats, IME, and
AccessKit/UI Automation use Windows services. The quick terminal is a tool window;
`quick-terminal-space-behavior = move` moves it to the foreground desktop when
revealed. Selection clipboard storage is shared within one process. The saved-layout
picker accepts Rustty JSON; Ghostty's macOS archived-state format requires macOS.

Windows validation:

```powershell
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p rustty-app --test windows_platform -- --ignored
cargo test -p rustty-app --test windows_conpty -- --ignored
$env:RUSTTY_SMOKE_DIR = Join-Path $env:TEMP 'rustty-native-smoke'
./target/debug/Rustty/rustty.exe
Remove-Item Env:RUSTTY_SMOKE_DIR
```

The native smoke uses isolated Git Bash sessions (no profile or history writes),
requiring Git for Windows at its standard installation path. Set `RUSTTY_SMOKE_SHELL`
to another native Git Bash executable if needed. It writes its own workspace and
readback image inside `RUSTTY_SMOKE_DIR` and does not restore the normal workspace.
Its report includes the selected renderer and animation frame rate. Set
`RUSTTY_SMOKE_NATIVE_CAPTURE=1` to also save `native-window.png`, an actual desktop
capture including the Windows frame and menu; the test window must be foreground
and unobscured. `window.png` captures client rendering with either backend.
The native CPU opacity/menu test is opt-in:
`cargo test -p rustty-app --lib software_surface -- --ignored --test-threads=1`.
The ConPTY check sends fragmented animated redraws through a real PTY and verifies
every byte boundary retains synchronization until text and cursor restoration
are complete. Passing `--system` additionally selects the OS runtime for diagnosis.

## macOS

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

Global shortcuts such as `keybind = global:ctrl+super+backquote=toggle_quick_terminal`
use native macOS hotkey registration without Accessibility or Input Monitoring
permission. Registered shortcuts take priority over local bindings in both the
foreground and background, including when no terminal window is open. Logical
shortcuts follow the current keyboard layout; `physical:` shortcuts keep their key
positions. Unsupported or conflicting combinations produce a warning in Rustty's
messages and are skipped globally; other shortcuts keep working, and failed
shortcuts may still work in a focused Rustty window. Reloading configuration retries
registration. Global shortcuts require explicit keys: `global:...catch_all` is
rejected, while local `catch_all` bindings remain supported.

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
links, detected URLs and file paths without spaces. Command-click opens the
highlighted target with its default application. Relative paths use the pane's
working directory, and `~/` paths use the home directory.

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
[CSI/OSC implementation inventory](../../test/rustty/SEQUENCES.md). Notifications are
available in the bundled app; permissions remain under macOS control.

Validation:

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo +1.95.0 run --release --offline -p rustty-vt --example parity-runner
RUSTTY_SMOKE_DIR=/tmp/rustty-native-smoke target/debug/Rustty.app/Contents/MacOS/rustty
```

The windowless hotkey regression runs explicitly on a macOS desktop with
`cargo test -p rustty-app --test native_global_hotkeys --offline -- --ignored`.
It checks registration conflicts, reloads, stale events, layout notifications and
cleanup using synthetic Carbon events without posting keyboard input.

The native text-input regression runs with
`cargo test -p rustty-app --test native_text_input --offline -- --ignored`.
It checks character-picker commits, composition cleanup and subsequent typing
through a hidden native window without posting keyboard input to other apps.

The opt-in native smoke check starts disposable `/bin/sh` sessions, checks input,
four split panes, tabs, quadrant focus and zoom, URI directory reports, restoration,
progress animation across pane focus changes, hover scrolling, file-drop targeting,
reverse video, DEC column mode, text blinking, synchronized output, hidden-tab title
updates, independent Find overlays without terminal resizing, and idle rendering.
The report records animation frame rate, the monitor's reported refresh rate, and
window redraws and pane preparations during hidden-tab title updates. It writes
`result.json`, `workspace.json` and a rendered `window.png` into the specified
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
