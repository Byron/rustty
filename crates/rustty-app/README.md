# Rustty for macOS and Windows

Rustty is a private Rust workspace implementing a terminal, native font services,
a portable frame renderer and a native desktop application. All crates are
`publish = false`. GPU rendering uses Metal on macOS and DirectX 12 with
DirectComposition on Windows. Windows also supports CPU rendering with GDI
presentation. CoreText and DirectWrite supply the respective native font backends.

## Continuous integration

[Rustty CI](../../.github/workflows/rustty.yml) runs on pushes, pull requests, and
manual dispatch, using hosted macOS 15 ARM64 and Windows Server 2025 x64 runners.
It pins Rust 1.95.0 and checks formatting, Clippy, and the ordinary workspace tests.
The two Clippy passes cover both optimized kernels and the optional scalar kernels:

```sh
cargo +1.95.0 fmt --all --check
cargo +1.95.0 clippy --locked --workspace --all-targets -- -D warnings
cargo +1.95.0 clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo +1.95.0 test --locked --workspace --no-fail-fast
```

macOS tests use `TMPDIR=/tmp` to keep Unix socket paths short. Windows CI builds
the debug bundle for its pinned ConPTY runtime, selects the runner's native Python,
and runs the packaged sidecar/retained-session tests described below. To reproduce
Windows tests locally, follow the [Windows bundle prerequisites](#windows-11-x64),
put native Python on `PATH`, and prepare these variables before running the tests:

```powershell
$env:RUSTUP_TOOLCHAIN = '1.95.0'
./crates/rustty-app/build.ps1
$env:TMUXX_CONPTY_RESOURCES = (Resolve-Path target/debug/Rustty/resources).Path
$env:TMUXX_REMOTE_PYTHON = python -c "import sys; print(sys.executable)"
$env:RUSTTY_TEST_EXE = (Resolve-Path target/debug/Rustty/rustty.exe).Path
```

Ordinary tests need no C tmux checkout. Other ignored desktop and C/SSH reference
tests remain opt-in; the workflow uses no secrets.

## Stream Deck agent dashboard

Opt in with `stream-deck = true` to show reporting panes on a 5×3 Stream Deck.
See [setup, controls, preview and limitations](../../docs/rustty/stream-deck.md)
and the [private status protocol and demo](../../docs/rustty-agent-status.md).
Run `rustty agent-sim` in a pane to test without an agent; use `--state working`
for a fixed state or `--interval 0.5` for a faster cycle. Ctrl-C unregisters it.
Tiles use large icons with short worktree task slugs; run
`rustty stream-deck-legend` for the symbol meanings and controls.
Automatic Codex status requires the companion TUI build with
`tui.terminal_status = "rustty"`; stock notifications alone do not register panes.

## Windows 11 x64

Install Rust 1.95 or later and the Visual Studio C++ build tools/Windows SDK.
From PowerShell at the repository root:

```powershell
just rustty-install-windows
```

This builds a release and installs the complete portable folder as `Rustty` on
your configured Windows Desktop. Open `Rustty/rustty.exe`
there to launch it. The recipe uses the Windows PowerShell included with Windows;
without `just`, run `./crates/rustty-app/build.ps1 -Release -Install` directly.
Bare `just` only lists recipes.

Installation stages the complete new bundle beside its destination, renames an
existing bundle to `Rustty.previous.<timestamp>-<id>`, then publishes the new one.
If publication fails, it restores the previous bundle. Keep previous bundles
until their running instances and sessions exit, then remove them manually.
The installer does not close or launch applications, register shortcuts, or
replace any bundle under `target`. Windows permits renaming loaded executables,
but Rustty also holds its verified ConPTY runtime files open without delete
sharing. Running Rustty instances or retained tmuxx sessions using the installed
bundle therefore prevent renaming that folder. Installation fails with the
existing bundle intact; close those instances and sessions before retrying.
A process using the folder as its working directory can also prevent renaming;
leave that directory before retrying. `-InstallDirectory 'D:\Dev apps\Rustty'`
selects another destination when used with `-Install`. Installation paths must not
pass through junctions or symbolic links; use a regular directory if your Desktop
is redirected that way.

For a bundle under `target/release/Rustty`, run
`./crates/rustty-app/build.ps1 -Release` without `-Install`. Omit `-Release` for a
debug build under `target/debug/Rustty`. The first build downloads Microsoft's pinned
ConPTY runtime and verifies its SHA-256. `-Offline` uses cached Cargo dependencies
and the runtime package under `target/conpty`.
The script builds for `x86_64-pc-windows-msvc` with a static C runtime; the portable
folder needs no Visual C++ redistributable. It contains `rustty.exe`, `tmuxx.exe`, ConPTY,
themes, shell integration, and license notices. Move the complete folder,
including `resources`, together. `-SkipBuild`
restages an existing script build. Cargo builds also work directly:
`cargo build -p rustty-app --bin rustty -p tmuxx-cli --bin tmuxx`, followed by
`cargo run -p rustty-app --bin rustty`. Persistent sessions need `tmuxx.exe` beside
`rustty.exe`. Build the portable folder first for resource
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

The installer regression checks use fake builds and isolated temporary folders,
including a running executable, ConPTY-style file locks, and directory locks.
They do not touch your Desktop or launch Rustty:
`python test/rustty/test_windows_install.py`.

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
Software rendering supports the shared renderer's 320 MiB image atlas and separate
64 MiB glyph atlas per window. Texture storage is allocated as needed.

Windows defaults to **Cascadia Mono at 12 pt**, falling back to Consolas and then
the embedded JetBrains Mono if those system families are unavailable. Explicit
`font-family` and style-specific family lists take priority. Emoji and symbol
fallbacks remain available with any terminal font.

Windows `font-size` uses points: physical pixels = points × 96/72 × display scale.
At 100% scaling the 12 pt default is 16 pixels; at 125%, 150%, and 200% it is 20,
24, and 32 pixels. Earlier Windows builds treated points as pixels. To preserve
the physical size of an explicit value from those builds, multiply it by 0.75
(for example, change `font-size = 13` to `font-size = 9.75`). Existing settings
are not rewritten. macOS sizing and defaults are unchanged.

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
To save the layout and displayed contents for another Rustty instance, hold **Alt**
while choosing **Quit** and enable saving session state. The next process offers
saved sessions automatically. See
[copying visual state during an upgrade](#copying-visual-state-during-an-upgrade).

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
are complete. Set `RUSTTY_CONPTY_RESOURCES` to test another bundle's resources
directory (for example, `target/release/Rustty/resources`). The shared PTY backend
requires the pinned bundle; the former `--system` option is no longer supported.

The session import regression runs five hidden debug instances in sequence, with
isolated state and disposable `cmd.exe /d /q` shells. Build the debug bundle first,
then run:

```powershell
$handoffDir = Join-Path $env:TEMP ('rustty-handoff-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $handoffDir | Out-Null
$handoffExe = (Resolve-Path target/debug/Rustty/rustty.exe).Path
$env:RUSTTY_HANDOFF_SMOKE_DIR = $handoffDir
try {
    foreach ($role in 'source', 'cancel', 'requester', 'single', 'failed') {
        $env:RUSTTY_HANDOFF_SMOKE_ROLE = $role
        $process = Start-Process -FilePath $handoffExe -WindowStyle Hidden -PassThru `
            -ArgumentList '--config-default-files=false', '--renderer=software' `
            -RedirectStandardOutput "$handoffDir/$role.log" -RedirectStandardError "$handoffDir/$role.err"
        $process | Wait-Process -Timeout 45
    }
    Get-Content "$handoffDir/*-result.json"
} finally {
    Remove-Item Env:RUSTTY_HANDOFF_SMOKE_DIR, Env:RUSTTY_HANDOFF_SMOKE_ROLE -ErrorAction SilentlyContinue
}
```

All five `*-result.json` files must report `"passed": true`; `selector.png` captures
the chooser. The check covers saving on quit, multiple-dump selection,
single-dump automatic import and failure fallback, cancellation, import cleanup,
styled history and alternate-screen contents, and output from a new ConPTY shell
below the restoration marker. Use `--renderer=gpu` to check GPU rendering.

## macOS

Build a native app from the repository root:

```sh
nu crates/rustty-app/build.nu --release
open target/release/Rustty.app
```

The bundle includes the `tmuxx` command in `Rustty.app/Contents/MacOS`, beside
`rustty`, with its license notice under `Contents/Resources/licenses`.

### Keeping macOS permissions across local builds

The bundle is signed ad hoc by default, so rebuilding changes its code-signing
identity and can invalidate macOS privacy permissions. For personal builds, create
one free, persistent signing certificate in **Keychain Access**:

1. Choose **Keychain Access > Certificate Assistant > Create a Certificate**.
2. Name it **Rustty Local Signing**, select **Self Signed Root** and **Code Signing**,
   and enable **Let me override defaults**.
3. Set the validity to **3650 days**, accept the remaining defaults, and save it in
   the **login** keychain.
4. Open the certificate, expand **Trust**, set **Code Signing** to **Always Trust**,
   close the window, and authenticate.

These steps follow [Apple's self-signing guide](https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/Procedures/Procedures.html).
Confirm that `security find-identity -v -p codesigning` lists the certificate, then
use it for every build:

```sh
nu crates/rustty-app/build.nu --release --sign 'Rustty Local Signing'
codesign --verify --deep --strict target/release/Rustty.app
codesign -dr - target/release/Rustty.app
```

To build and install the signed release bundle on macOS, run
`just rustty-install-macos`. Bare `just` only lists recipes. The installer stages
and verifies the complete bundle under `/Applications`, renames an existing
`Rustty.app` to `Rustty.previous.<timestamp>-<pid>.app`, then installs the new copy.
If the final move fails, it restores the previous app. It leaves running instances
open; keep their previous bundles until they exit. The recipe does not launch the
new app or grant permissions.

The designated requirement should include `app.rustty.Rustty` and a certificate
requirement, rather than only an executable `cdhash`. Keep the same certificate and
private key; recreating the certificate creates a different identity. If Keychain
Access asks whether `codesign` may use the private key, allow it for this signing
operation.

Install the complete signed bundle using the [upgrade procedure](#copying-visual-state-during-an-upgrade).
After switching from ad hoc signing, grant **Full Disk Access** and **Developer
Tools** again under **System Settings > Privacy & Security** for the installed
`/Applications/Rustty.app`. If an old entry remains enabled but access fails,
remove and re-add it, then restart the signed instance. Subsequent builds with the
same identity should retain these grants; verify this across a later rebuild on
your macOS version. Self-signing does not provide Developer ID trust or notarization
for public distribution.

### Development and configuration

For development, use `cargo run -p rustty-app --bin rustty`. Build the debug bundle
first (`nu crates/rustty-app/build.nu`) to make themes, terminfo and shell integration
available to development sessions. Ordinary Cargo builds do not require Zig.

Rustty loads its own configuration when present, otherwise Ghostty Local settings,
then stable Ghostty settings. `rustty --config-info` reports the selected files.
Own settings can be stored in `~/.config/rustty/rustty.txt` or
`~/Library/Application Support/com.rustty.app/rustty.txt`. Legacy `config` and
`config.rustty` files remain supported; an empty own file
intentionally disables Ghostty fallback. No Ghostty file is modified.
Rustty-specific settings, including `tmuxx-*`, can live directly in the active
Ghostty configuration file. An include such as
`config-file = /path/to/shared-settings` is optional; no redirect or separate
Rustty file is required. Put the settings in the file
reported by `--config-info`: creating an own Rustty file switches to that family
instead of combining it with Ghostty fallback settings.
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
The command palette's **Reset Terminal** action (`reset` in keybindings) directly
resets the focused pane's modes, screen, and scrollback, including unfinished
escape sequences. It works when terminal keyboard input is disabled. The child
process keeps running; its own input settings or subsequent output can still
require recovery within that application. Press Enter if the shell needs to
redraw its prompt.

Choose **Themes…** in the command palette to search user themes followed by
bundled themes. Arrow keys or moving the pointer over a theme preview its
appearance across open windows. Escape restores the previous appearance;
Enter or a click keeps the theme for this session and shows the configuration
line to add or edit. Rustty never writes configuration files from this picker.
Explicit appearance settings still override the theme. Previews preserve
shortcuts, commands, history limits and permissions, even in custom theme files.
Reloading configuration (including an OS appearance change) restores the saved
theme and closes any active preview. The `themes` action can also be bound to
a key. Theme discovery follows the active configuration family: Rustty user
themes live under `$XDG_CONFIG_HOME/rustty/themes` (normally
`~/.config/rustty/themes`), or `%APPDATA%\Rustty\themes` on Windows; Ghostty
fallback uses its corresponding `ghostty/themes` directory.

Tabs, splits, zoom, quadrant navigation, clipboard and
search use the configured Ghostty keybindings. Window layouts and pane directories
are saved separately under `com.rustty.app`.

Unfocused windows show nearly neutral tab colors and UI accents, including the
frame tint and selected panel outline. Terminal and panel status colors stay intact;
tab colors return when the window gains focus.
`unfocused-tab-saturation = 5%` controls how much color unfocused windows retain:
`0%` is grayscale and `100%` preserves the original colors. Values without `%`
are also percentages, and an empty value restores the 5% default. Changes take
effect on configuration reload.

**Quad roots** add recursive quad levels to local tiled tabs. Zoom into an inferred
quad, then use **Toggle Quad Root** in the command palette to designate the focused
sub-quad and zoom into it. The same inference rule applies at each level: the first
split on both axes identifies the subtree. A leaf can be designated too; subsequent
splits stay inside its root. While zoomed into a root, the action tries to create
another level. From its parent view, it removes the focused child's designation
from any contained panel. Removing a parent also clears descendant designations,
leaving panels and splits intact.
There is no fixed nesting limit beyond the existing layout limits.

Quad Zoom toggles between a designated child and its parent. If the focused branch
has a designated child, Quad Zoom enters it instead of leaving the parent; selecting
an ancestor in the breadcrumb returns directly to that level. The outliner,
breadcrumbs, and pinned paths show the extra levels. Quad navigation uses the
visible hierarchy, while holding the configured Quad Peek chord still shows the
original inferred quads; releasing it restores the nested zoom when staying in that
root. Designations survive undo, saved state, and layout imports, and follow surviving
subtrees when panels close. They are unavailable in server-owned tmuxx tabs.

The action has no default shortcut. For example:

```ini
keybind = ctrl+shift+g=toggle_quad_root
```

Splitting a pane shrinks it to make room for the new pane in 100 ms by default.
Closing a pane or exiting its shell expands surviving panes with the same duration,
including when returning to a single pane.
The closing pane's last rendered content stays visible until the expansion covers it.
Pane and quadrant zoom transitions slide into place in 250 ms; Quad Peek takes
100 ms. Text fades to the pane background at the midpoint, then reappears at
its final size, without resizing terminals at intermediate animation steps.
Automatic unzoom after a pane closes or exits uses the same animation, as do
splitting or resizing panes when those actions leave zoom mode.
Surrounding panes remain visible throughout the slide, blending halfway toward
the window frame color around the tabs as the enlarged pane or quadrant slides
over them. The background mix reaches its configured strength by the midpoint
and stays there through the rest of the slide; zooming out reverses this fade.
Consumed pane and quadrant zoom shortcuts toggle once per key press, so holding
the shortcut does not repeatedly reverse the animation.
Animations follow macOS **Reduce Motion** by default. On other platforms they
default to enabled with GPU rendering and disabled with software rendering.
Set `window-animations = false` to disable these transitions, or `true` to enable
them explicitly. An empty value restores the platform default. This setting
takes effect when configuration is reloaded.

`split-animation-duration` controls adding and removing splits. The zoom duration keys
apply in both directions. `zoom-animation-duration`
controls pane zoom from the normal layout; `zoom-from-quad-animation-duration`
controls pane zoom from a zoomed quadrant. Values use time units and may exceed
the defaults; `0` disables that transition and an empty value restores its
default. Changes take effect when configuration is reloaded.

```ini
split-animation-duration = 100ms
quad-zoom-animation-duration = 250ms
quad-peek-animation-duration = 100ms
zoom-animation-duration = 250ms
zoom-from-quad-animation-duration = 250ms
zoom-animation-background-mix = 50%
```

`zoom-animation-background-mix` accepts percentages from `0%` (original pane
colors) to `100%` (the full window frame color). The `%` suffix is optional;
`50` also means 50%. An empty value restores the 50% default. Changes take
effect when configuration is reloaded.

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

Tab colors apply to tab controls, split focus outlines and completion flashes,
and bleed downward beneath the selected tab into a subtle tint in the existing
thin frame and split gaps, without coloring terminal margins;
unassigned tabs use the macOS system accent with the same bleed and frame tint.
`tab-frame-color-mix = 9%` controls the tab color mixed into the window frame,
which is the bottom color of the tab gradient. It accepts `0%` through `100%`;
`18%` restores the previous intensity, and an empty value restores the 9% default.
Inactive tabs and directory labels underline running commands, while `▶` counts panes explicitly reporting work
through a leading activity spinner or progress report. Waiting-for-input titles
do not count as active work. Each pane can trigger a short completion flash even
while another pane remains busy. Directory labels normally use the basename.
When different paths share that basename, labels include the shortest parent suffix
that distinguishes them, such as `work/project` and `fork/project`. Names are shared
across terminal badges, the outliner, breadcrumbs, pins, and panel search, with
unopened tabs included. Paths are compared lexically, without filesystem lookups.
Quadrants share one label when all their panels have the same resolved directory
name; differing or missing directories keep individual labels.
Focusing a shared-name quad keeps sibling names hidden without replacing them
with standalone status badges. Pinned panels retain their pin indicator.
Single-panel quads keep the same label size when focused. Labels participate in
the content fade during zoom transitions.

The tab color menu offers the current theme's palette, background, and foreground,
plus softer shades blended toward the theme background and higher-contrast shades
blended toward its foreground. The general color picker remains available.

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
RUSTTY_SMOKE_DIR=/tmp/rustty-native-smoke RUSTTY_SMOKE_OFFSCREEN=1 target/debug/Rustty.app/Contents/MacOS/rustty
```

The macOS session import regression runs five hidden debug instances in sequence
with isolated configuration, disposable `/bin/sh` shells, and state. Each
`*-result.json` must report `"passed": true`; `selector.png` captures the chooser.

```sh
cargo build -p rustty-app --bin rustty --offline
handoff_dir=$(mktemp -d /tmp/rustty-handoff.XXXXXX)
printf 'command = /bin/sh\nshell-integration = none\nstream-deck = false\nworking-directory = %s\n' "$handoff_dir" > "$handoff_dir/config"
for role in source cancel requester single failed; do
  RUSTTY_HANDOFF_SMOKE_DIR="$handoff_dir" RUSTTY_HANDOFF_SMOKE_ROLE="$role" \
    target/debug/rustty --config-default-files=false --config-file="$handoff_dir/config" \
    > "$handoff_dir/$role.log" 2>&1 || break
done
cat "$handoff_dir"/*-result.json
```

The windowless hotkey regression runs explicitly on a macOS desktop with
`cargo test -p rustty-app --test native_global_hotkeys --offline -- --ignored`.
It checks registration conflicts, reloads, stale events, layout notifications and
cleanup using synthetic Carbon events without posting keyboard input.

On macOS, drag emoji or plain text into a terminal pane to paste it there.
File drops insert shell-quoted paths. Text drops use the same bracketed-paste
handling and paste protection as clipboard text.

The native text-input regression runs with
`cargo test -p rustty-app --test native_text_input --offline -- --ignored`.
It checks external text commits (including split UTF-16 surrogates), composition
cleanup and subsequent typing
through a hidden native window without posting keyboard input to other apps.
The input-routing tests also check that terminal composition survives native
window focus changes, including when the character picker takes focus.

The native drag-and-drop regression runs with
`cargo test -p rustty-app --test native_drag_drop --offline -- --ignored`.
It checks text drops, file precedence, and drop coordinates through a hidden
native window and a private pasteboard.

The opt-in native smoke check starts disposable `/bin/sh` sessions, checks input,
four split panes, tabs, quadrant focus and zoom, URI directory reports, restoration,
progress animation across pane focus changes, hover scrolling, file/text-drop targeting,
reverse video, DEC column mode, text blinking, synchronized output, hidden-tab title
updates, independent Find overlays without terminal resizing, and idle rendering.
On macOS, unobscured indefinite progress bars animate in Core Animation without
repainting the window. Captures, zoom transitions, and overlapping UI use the
ordinary renderer. The smoke check covers native layer timing and cleanup; its
foreground run also checks that progress stops requesting frequent window frames.
The report records window frame rate, the monitor's reported refresh rate, and
window redraws and pane preparations during hidden-tab title updates. It writes
`result.json`, `workspace.json` and a rendered `window.png` into the specified
directory and exits. It uses the selected display configuration but does not
restore or overwrite the regular app workspace.
Find checks also capture `find-focused.png` and `find-unfocused.png` to verify
the overlays and their transparency.
Zoom checks capture `zoom.png`, `quad-zoom.png` and `nested-zoom.png`, with
distinct labels in the panel header, tab label and window title. Toggling
quadrant zoom from nested pane zoom returns to the quadrant before unzooming.
Animation checks cover both directions of pane, quadrant, nested zoom and Quad
Peek, asserting endpoint-only terminal grids and cached terminal content between
frames. `animation-*.png` captures show each transition at one quarter, midpoint
and three quarters, including distinct panel labels during nested zooms.
Theme checks capture `theme-picker.png` and verify preview, cancellation,
acceptance, reload and unchanged configuration files using isolated themes.

Use `RUSTTY_SMOKE_OFFSCREEN=1` while working on the same desktop, or on a locked
or headless Mac. It renders the same host primitives into Metal textures while
keeping native windows hidden, without activating the app or taking keyboard focus.
Native focus, minimization, accessibility, and input-method updates are skipped;
internal navigation and input routing are still checked. Each smoke step checks
that no native window became visible or focused. GPU readback waits are bounded.

Omit `RUSTTY_SMOKE_OFFSCREEN` only for an attended foreground run on an idle
desktop. That run shows, focuses, and minimizes native windows to check OS behavior.
The native smoke report includes event counts during its idle phase, distinguishing
unchanged cursor events, actual pointer movement and egui repaint requests.
Set `RUSTTY_SMOKE_HOVER=1` in a foreground run to require a stationary pointer over a shell pane
during the idle interval. Move the pointer into a test pane before the 45-second
timeout; leaving the pane or moving the pointer restarts the interval.

### Copying visual state during an upgrade

Hold **Option** on macOS or **Alt** on Windows while choosing **Quit** to open a
confirmation that offers to save session state for a future Rustty instance.
Enable the checkbox to save before quitting. An ordinary quit adds the same
checkbox when Rustty already needs confirmation, such as when commands are
running; it does not introduce an extra prompt by itself.

The additional **Keep session state across reboots** checkbox is initially off.
Enable it along with saving to keep the export in Rustty's application data
directory, where system temporary-file cleanup will not remove it. Launching
Rustty checks both temporary and persistent exports for import.

Configure the checkbox with:

```ini
session-dump-on-quit = unchecked
```

- `unchecked` is the default: offer saving with the checkbox initially off.
- `checked` offers saving with the checkbox initially on.
- `disabled` removes the checkbox and disables the Option/Alt quit prompt entirely.

Use **File → Dump All Sessions…** to save without quitting. Confirm the warning
that screen contents, scrollback, and working directories may contain personally
identifiable information (PII) and secrets. The dump includes all local sessions
in the current instance; tmuxx sessions are excluded. Canceling writes nothing.
The saved data stays on disk until another Rustty instance successfully imports
it; canceling the chooser does not remove it.

After the old instance quits, replace its application bundle or the complete
Windows application folder and launch Rustty normally. Every new process checks
for saved sessions. If exactly one is available, Rustty imports it automatically.
If several are available, **Import Saved Session State** lists the most recent
first. Select a saved session and click **Import**. Rustty does not open terminal
windows or start shells while the chooser is open. If an automatic import fails,
the chooser displays the error and keeps the dump available for retry.

On macOS, use this command when another Rustty process is still running and you
want a separate instance:

```sh
open -n /Applications/Rustty.app
```

An ordinary Finder or Dock launch may activate the existing process instead.
**Cancel**, Escape, or closing the chooser starts Rustty with the usual window
configuration. Canceling or a failed import preserves all saved sessions for
retry. A successful import removes only the selected saved session.

Import restores local window/tab/panel layouts, scrollback, and displayed contents,
including alternate-screen applications. Dumps omit tmuxx windows; reconnect to
their servers to restore them. Local tabs start their shells when first selected or explicitly sent input;
unopened tabs keep their saved displays unchanged, including across another dump.
Selecting an imported tab shows its layout immediately while its panes restore
their history and start their shells in the background.
Rustty compares each fresh shell's rendered startup
output with the saved tail. Matching output replaces that tail without a divider
or duplicate prompt. Different output gets this bold message above it, centered
in a band of solid Unicode blocks alternating between two subtle shades of the
panel's background color. Narrow panels wrap the message without padding. Empty
panels omit it. Shell integration marks prompt completion; other commands are
compared after 100 ms without output, or after one second of continuous output:

> ███ **Restored Rustty display. A new shell starts below.** ███

Running jobs, shell variables, and PTYs are not saved. Earlier instances save
separate layout backups, so closing one later cannot overwrite the imported
workspace.

Dumps use private, atomically published `.rustty-state.lz4` files and metadata in
`rustty-session-dumps` inside the current user's system temporary directory
(normally `$TMPDIR` on macOS and `%LOCALAPPDATA%\Temp` on Windows). They survive
Rustty exiting, but system temporary-file cleanup can remove them. Dumps
saved with **Keep session state across reboots** instead use `session-dumps`
inside `~/Library/Application Support/com.rustty.app` on macOS or
`%LOCALAPPDATA%\Rustty` on Windows. Both kinds are removed after a successful
import. Large dumps
can pause the saving instance while they are written. Compressed files and
decoded state are each limited to 512 MiB. If saving fails, Rustty stays open so
the user can retry or quit without saving.

### tmuxx windows

The native application **tmuxx** menu and command palette create terminal windows backed by a
multiplexer. Ordinary Rustty windows can remain open alongside any of these:

| Choice | Owner and lifetime |
| --- | --- |
| New temporary window | Embedded engine, reached through typed Rust APIs; ends when its last window closes or Rustty exits |
| New persistent window | Local daemon started with the same executable, `rustty tmux`; survives Rustty exiting |
| Connect | Existing native tmuxx endpoint or stock tmux over SSH |

**Copy Window to Persistent Session** copies all tabs in the current Rustty
window into a new local persistent tmuxx session and opens it in a new Rustty
window. The command palette calls this **Copy Window to Persistent tmuxx Session**;
the keybinding action is `copy_window_to_tmuxx` (no default shortcut). The copy
preserves tab order and names, panel working directories, split layouts, the
active tab and panel, and whole-panel zoom. Split sizes are rounded to terminal
cells; tab colors and custom panel titles are retained in the new Rustty window.
Panels start fresh shells using the destination server's defaults; running
programs and scrollback are not copied. The original window stays unchanged.
Copying supports local tiled windows with existing working directories. Remote
windows and floating panels are rejected before creating a session; quadrant
zoom is not carried over.

Connected windows show a **tmuxx** tag at the top right and in the outliner.
Hover over the tag for the connection source. Ordinary windows have no tag.

Both window types can coexist. Each connection projects only its attached
sessions. A server session appears as a Rustty window, a server window as a tab,
and a server pane as a Rustty panel. Splitting, resizing, zooming, selecting tabs,
and **New floating panel** change the server's objects. Floating panels use the
same native rendering and input controls as tiled panels; their server geometry,
stacking, clipping, and modal input policies remain authoritative.

Native tmuxx sessions using `window-size=latest` follow the first Rustty view
that reports dimensions. Other views do not take control by resizing or focusing
their own windows. Closing the owner transfers sizing to the next view.

For an open session, choose **tmuxx → server → session → Fit to This Window**
to use its local window's dimensions and take control. This switches its tabs
to `latest`, including tabs left in `manual` by old window copies. Automatic
size reports preserve other policies; `aggressive-resize` still delays inactive
tabs until selected. Ownership requires an updated server. On older native
servers, Fit sets a fixed size until used again. Stock tmux over SSH retains
standard tmux sizing.

Native tmuxx windows keep your selected pane locally when the server selects a
different pane. A small target marker in the pane's lower-right corner shows
that server-selected pane; hover it for an explanation. Rustty still sends
`select-pane` when you select a pane.
The marker represents the latest shared selection, including commands and
scripts; the protocol does not identify who selected it. It disappears when
the server selection matches yours. Tab selection, zoom, and modal panels stay
shared, so hiding or closing your selected pane can require a focus change.
Splits and floating panels you create receive focus only in your local view.
Stock tmux over SSH retains its existing focus behavior.

Closing a panel kills its pane. Closing a tab kills its server window, including
linked views in other sessions. Closing a persistent or SSH window asks whether
to terminate its session or close only the local window. The tmuxx menu lists
sessions and their tabs and offers explicit session/server termination.
Closing a temporary window terminates its session; there is no
persistent detach choice for an engine bound to that window's lifetime. A linked
session visible in another temporary window keeps that engine alive until its
last window closes.

Local servers remain in the menu after **Disconnect**, closing their last local
window, or terminating all sessions. Rustty releases their terminal snapshots
and keeps a lightweight management connection. Choose **Reconnect** to browse
the sessions again. **Stop Idle Server…** is available when the connection count
is zero; the server checks again before stopping, so a client connecting while
the confirmation is open prevents shutdown. Stopping also ends any programs
still running in detached sessions.

Temporary and persistent native servers show live connection counts without
transferring terminal contents. Counts include GUI, window-size, and CLI
connections; management-only observers and internal server jobs are excluded.
Older servers show **Connections: unavailable**; live counts and guarded idle
shutdown require an updated server. Connected views retain the existing
**Stop Server…** confirmation, which also stops work in other clients. On macOS
the daemon's small PTY watchdog exits with the daemon.

Server-stop confirmations use neutral wording when no other clients are connected,
excluding the current view's own connection and sizing clients. Other local and
remote connections trigger a warning; an unavailable count is stated explicitly.

On application restart Rustty restores ordinary windows and reconnects saved
local native-daemon attachments. It verifies the daemon incarnation and session
ID before restoring a view, so an unrelated replacement daemon cannot inherit an
old attachment. Local transport interruptions retry without replaying input or
ambiguous commands. SSH connections require an explicit reconnect, and are not
saved for automatic reopening. Stopping a daemon disables retries for it.

Disconnected windows keep their terminal contents available for local scrolling,
searching, and copying. Terminal input and application mouse capture stop, queued
input is discarded, and the tmuxx label shows `disconnected` until the reconnected
view is ready. Window controls and reconnect menus remain available.

These commands use the same implementation as the menu:

```sh
rustty --tmuxx=embedded
rustty --tmuxx=persistent
rustty --tmuxx=local:default
rustty --tmuxx=local:work
rustty --tmuxx=unix:/path/to/default.native
rustty --tmuxx=pipe:default
rustty --tmuxx=ssh:my-host
rustty --tmuxx=system-ssh:my-host
```

`local:NAME` selects a local owner on both Windows and Unix. `local:default`
uses the same default as **New persistent window**, including `TMUXX_ENDPOINT`
when configured. Other names select the owner used by `tmuxx -L NAME` (or
`rustty tmux -L NAME` on Unix), using `TMUX_TMPDIR` on Unix. Names contain 1–64
UTF-8 bytes, may include spaces and Unicode, and cannot contain control
characters, `/`, or `\`, or be `.` or `..`.

Local connections display as **local · work**. Their menu offers **Copy Connection
Address** (for example, `local:work`) and retains the full **Native endpoint** in
the submenu. Existing saved connections keep their endpoints. Custom endpoints
without a matching short name retain their full address.

`unix:PATH` still selects an explicit Unix socket; `pipe:INSTANCE` still selects
a Windows owner. Full local pipe paths and absolute Unix socket paths are also
accepted. On Unix, Rustty's embedded CLI manages its default native owner:

```sh
rustty tmux new-session -d -s work
rustty tmux list-sessions
rustty tmux kill-session -t work
rustty tmux kill-server
```

On Windows, `tmuxx.exe list-sessions` and other commands use the same default
named-pipe owner as the GUI. Create one with **New persistent window** or
`rustty.exe --tmuxx=persistent` before using those commands. A headless owner
can instead run as `tmuxx.exe -D` in a separate process; use another terminal
to run `tmuxx.exe new-session -d -s work`. Command clients and SSH bridges
attach to existing Windows owners; they do not start one implicitly. Unix command
clients retain their automatic daemon startup.

`tmuxx.exe` is a Windows console executable, so shells wait for it and retain its
exit status and redirected stdin/stdout/stderr. Use it for Windows command-line
and SSH access; the GUI starts persistent owners with this same sidecar and no
console window. Both executables share the bundle's `resources/conpty` runtime.

The embedded `rustty tmux` entry remains available. It exposes only the native
endpoint by default. No tmux command socket is needed for native SSH access.
On Unix, the bundled standalone CLI can reach a GUI owner with
`tmuxx --native-endpoint ENDPOINT list-sessions`, using the endpoint shown in
Rustty's tmuxx submenu. Plain `tmuxx` retains stock command/control socket
discovery, including inherited `TMUX`; the GUI's native-only owner has no such
command socket. Ordinary interactive `tmux attach` is not
an implemented tmuxx client mode. The GUI still supports a real stock tmux server
through its SSH control-mode adapter.

Native GUI and CLI endpoint discovery share `TMUXX_ENDPOINT`, or the native
`default.native` socket in the per-user tmux directory (`TMUX_TMPDIR` or `/tmp`)
on Unix. Windows uses an account-scoped named pipe for the `default` instance.
Temporary menu windows use unique endpoints, shown in their connection submenu.
For a known embedded endpoint, start with
`TMUXX_ENDPOINT=/path/to/private/socket.native rustty --tmuxx=embedded`.
Only one owner can bind a given endpoint. To configure servers started by Rustty,
add this to the active application settings file (including Ghostty Local when
it is the active fallback):

```ini
tmuxx-config-file = ~/tmuxx.conf
```

Temporary and newly started persistent servers load this file before creating
their first session. Relative paths use the directory of the declaring settings
file (the working directory for command-line overrides); `~/` uses your home
directory. An empty value resets the setting. Missing or unreadable files prevent
startup. Configuration command errors are recorded in the server's `show-messages`
log. Connecting to an existing local or remote server keeps its settings;
reloading Rustty settings affects only future servers. Choose **Settings…** in a
local server's tmuxx submenu to open all its startup configuration files in your
text editor. Opening configuration is unavailable for SSH servers. Choose
**Reload Configuration** in the same submenu to reload the startup files in
order, or use `source-file` from the CLI. This applies settings without restarting
the server or its shells. These actions are unavailable for older owners that
do not report their files and for servers started without configuration files.
Headless servers can load the same file with
`rustty tmux -f /path/to/tmuxx.conf ...` or `tmuxx.exe -D -f PATH`.

On Windows, new tmuxx servers inherit the server account's Windows Terminal
default shell and its arguments, just like ordinary Rustty terminals. This
includes Git Bash's login flags and applies to later sessions, tabs, and panes,
including panes created by clients connected over SSH. The preference is read
when the server starts. If it cannot be resolved, tmuxx uses its existing
`SHELL`, `COMSPEC`, or system `cmd.exe` fallback.

Explicit tmuxx `default-shell`, `default-command`, `window-default-command`, and
pane commands take precedence over inherited shell arguments. To choose a shell
independently of Windows Terminal, put this in the **server machine's** tmuxx
configuration file:

```tmux
set -g default-shell 'C:/Program Files/Git/bin/bash.exe'
set -g default-command 'exec "$SHELL" --login -i'
```

Setting `default-shell` alone replaces the inherited profile and its arguments;
the second line explicitly requests Git Bash's login mode. The SSH login shell
still launches the tmuxx bridge, while the tmuxx server chooses each pane's shell.

To use a remote native daemon, install the Rustty executable on that host and
create the session once:

```sh
ssh my-host 'rustty tmux new-session -d -s work'
rustty --tmuxx=ssh:my-host
```

The SSH adapter invokes the configured `tmuxx-ssh-command`, whose default is
`rustty tmux --native-stdio`. That command bridges an existing daemon; it does not
start one. A standalone remote tmuxx installation can use
`tmuxx-ssh-command = tmuxx --native-stdio`. To reach a remote embedded engine,
include its endpoint, for example
`tmuxx-ssh-command = rustty tmux --native-endpoint /private/path/temporary.native --native-stdio`.

Add named connections to the tmuxx menu and override their remote helpers in the
same active configuration file:

```ini
tmuxx-connection = Local work = local:work
tmuxx-connection = Azure Linux = ssh:20.240.208.217
tmuxx-ssh-command-host = 20.240.208.217 = /home/byron/.cargo/bin/tmuxx -L rustty --native-stdio
tmuxx-connection = Stock tmux = system-ssh:legacy-linux
tmuxx-connection = Windows = ssh:windows
tmuxx-ssh-command-host = windows = "C:/Apps/Rustty/tmuxx.exe" --native-stdio
```

Connection labels such as `Azure Linux` appear in the menu and identify attached
windows. Reload configuration to update saved entries; selecting one connects
or offers its existing sessions. SSH connections do not reconnect automatically.
Later entries replace the same display name; an empty `tmuxx-connection =` clears
all saved entries. Targets can use SSH
aliases from `~/.ssh/config`: replacing the IP with `azure-linux` also requires
using `azure-linux` as the helper override key. A helper key matches exactly what
follows `ssh:` in a named connection, `--tmuxx=ssh:linux`, or the Connect dialog,
including any `user@` prefix. Unmatched targets use the global
`tmuxx-ssh-command`. Later entries replace an earlier entry for the same target;
an empty `tmuxx-ssh-command-host =` clears all overrides. A launch-time override
uses the same syntax, for example
`rustty --tmuxx=ssh:linux '--tmuxx-ssh-command-host=linux=/home/byron/.cargo/bin/tmuxx -L rustty --native-stdio'`.
The Azure example connects to the existing daemon named `rustty`; the stock tmux
entry requires an existing ordinary tmux session and no tmuxx helper. The Windows
entry runs the packaged executable through an existing OpenSSH server using its
default cmd.exe shell. If that server uses PowerShell, use a child process with
inherited standard handles so the binary stream bypasses its text pipeline:

```ini
tmuxx-ssh-command-host = windows = $p = New-Object System.Diagnostics.ProcessStartInfo; $p.FileName = 'C:/Apps/Rustty/tmuxx.exe'; $p.Arguments = '--native-stdio'; $p.UseShellExecute = $false; $p.CreateNoWindow = $true; $c = [System.Diagnostics.Process]::Start($p); $c.WaitForExit(); exit $c.ExitCode
```

For `rustty.exe`, use `tmux --native-stdio` as `Arguments`. Remove any old
`| ForEach-Object { $_ }` suffix. Keep the complete `Rustty` folder, including
`resources`, on the Windows host. Native protocol v2 uses binary MessagePack and
per-block LZ4 compression; update Rustty, the remote bridge, and each owner
together, then restart the owners before reconnecting.

Give connection windows different themes with exact SSH targets or session names:

```ini
tmuxx-theme-host = windows = Catppuccin Mocha
tmuxx-theme-host = azure-linux = light:Catppuccin Latte,dark:Catppuccin Frappe
tmuxx-theme-session = work = Gruvbox Dark
```

Session rules take precedence over host rules and also apply to local sessions.
Host rules match the literal SSH target, including any `user@` prefix, for both
native tmuxx and stock tmux connections. Matches are case-sensitive. Later entries
replace the same selector; an empty `tmuxx-theme-host =` or
`tmuxx-theme-session =` clears that set of rules. Reload configuration to apply
changes to open windows. Missing or invalid themes report a diagnostic and use
the global appearance. Theme lookup and light/dark pairs work as with `theme`;
explicit appearance settings in your configuration still take precedence.

These themes change the local window's appearance, including fonts and padding,
without changing shell commands, key bindings, or the remote owner's settings.
They survive pane updates and reconnects. The Themes picker previews the matching
session or host rule. Without a matching rule, tmuxx windows preview a new session
rule; regular local windows preview the global theme. Acceptance shows the same
rule's configuration line and the file where it belongs. Other windows matching
that rule share the preview.

The Windows SSH bridge connects to the running owner's local named pipe under
the same Windows account; SSH authentication and transport remain OpenSSH's
responsibility. With one owner, `tmuxx.exe --native-stdio` discovers it.
If multiple temporary or persistent owners are running, select the exact native
endpoint shown in Rustty's tmuxx submenu:

```ini
tmuxx-ssh-command-host = windows = "C:/Apps/Rustty/tmuxx.exe" --native-endpoint "\\.\pipe\tmuxx-SID-ENCODED_INSTANCE" --native-stdio
```

Replace the example pipe with the displayed endpoint. The bridge carries binary
protocol data over stdin/stdout, without a remote PTY or another terminal window.
Connecting does not change the owner's lifetime: temporary owners end with their
Rustty window/application, and persistent owners survive GUI and SSH disconnects.

This is a remote shell command from trusted configuration: quote paths containing
spaces for the remote shell. Hosts, ports, identities, and jump hosts use normal
SSH aliases/configuration. Agent identities can be shared between applications,
but an app launched from Finder/Dock does not inherit environment changes from
your shell's startup files. If your shell selects a particular agent, configure
its socket in `~/.ssh/config` so menu connections use the same agent, for example:

```sshconfig
Host 20.240.208.217
    IdentityAgent ~/.gnupg/S.gpg-agent.ssh
```

Use your actual agent socket (the shell's `SSH_AUTH_SOCK`), which may differ from
this GPG example. Alternatively, Rustty's existing `env` settings now apply to
SSH children as well as terminal shells:

```ini
env = SSH_AUTH_SOCK=/Users/byron/.gnupg/S.gpg-agent.ssh
```

Rustty honors SSH's configured authentication and host-key checks. For an SSH
askpass helper, set `env = SSH_ASKPASS=/absolute/path/to/helper`; Rustty uses that
helper without requiring a controlling terminal or `DISPLAY`, unless an explicit
`SSH_ASKPASS_REQUIRE` policy overrides it. Agent-owned prompts such as GPG pinentry
continue to be handled by the agent. SSH connections allow two minutes for the
initial authentication and session snapshot. Authentication/helper failures are
shown in Rustty.

For stock tmux, create a remote session with ordinary tmux and use
`--tmuxx=system-ssh:my-host`. No Rust helper is required on that host. This adapter
captures visible text/rendition and exposed cursor/modes, then streams pane output.
Stock tmux cannot export tmuxx's complete atomic terminal/parser/history/graphics
snapshot, and the adapter currently has less restoration fidelity. It does not
route focus, bell, notification, or clipboard actions through Rustty. Native
multiplexer panels retain the latest forked Codex agent report in the server and
restore it on attach, including floating-panel Stream Deck entries. This does not
replay clipboard/notification actions. Stock Codex detection is not added.

Live PTY handover and embedded-to-daemon promotion are deliberately absent.
Create a persistent window from the outset when programs must survive Rustty.
Moving tabs, loading a local layout, and layout undo in tmuxx windows are disabled;
ordinary windows retain their local layout commands. Windows embedded owners,
persistent owners, local named-pipe attachments, and native SSH bridges use the
same tmuxx runtime as the standalone driver.

The [tmuxx crates](../../tmuxx/README.md) live in this repository and use the same
local `rustty-parser` and `rustty-vt` crates as Rustty's renderers and sessions.
Native checkpoint/replay and the server can now evolve in the same workspace.
The workspace also patches `portable-pty` to the local tmuxx copy so Rustty and
tmuxx share the required native PTY backend; its Unix behavior is unchanged.
On Windows Rustty calls the shared
runtime's explicit ConPTY initializer before creating any panes. It verifies the
bundled DLL and OpenConsole executable against the pinned package and retains
them for the process lifetime. Ordinary and embedded panes use the same runtime.

GUI connections and the `rustty tmux` CLI require native protocol v2. Native
clients, SSH bridges, and daemons must be upgraded together; restart older
daemons before connecting. Incompatible native connections fail without falling
back to stock tmux. Restarting a daemon ends its running programs; reconnecting
a compatible GUI client does not require a daemon restart.

Targeted verification:

```sh
cargo +1.95.0 test --locked --offline -p rustty --features sessions --lib tmuxx::tests::
cargo +1.95.0 test --locked --offline -p rustty-app --lib tmuxx_workspace::tests::
cargo +1.95.0 test --locked --offline -p rustty-app --bin rustty tmuxx_host::tests::
cargo +1.95.0 test --locked --offline -p rustty-app --test tmux_cli
```

Windows native lifecycle tests require the bundled ConPTY resources. The CLI
tests also check `tmuxx.exe` beside the selected `rustty.exe`. Build the bundle,
then run (use the equivalent debug paths for a debug bundle):

```powershell
$env:RUSTTY_TEST_EXE = (Resolve-Path target/release/Rustty/rustty.exe).Path
$env:TMUXX_CONPTY_RESOURCES = (Resolve-Path target/release/Rustty/resources).Path
cargo +1.95.0 test --locked --offline --target x86_64-pc-windows-msvc -p rustty --features sessions --lib tmuxx::windows_tests:: -- --include-ignored --test-threads=1
cargo +1.95.0 test --locked --offline --target x86_64-pc-windows-msvc -p rustty-app --bin rustty tmuxx_host::tests:: -- --include-ignored --test-threads=1
cargo +1.95.0 test --locked --offline --target x86_64-pc-windows-msvc -p rustty-app --test tmux_cli_windows -- --ignored --test-threads=1
```

Without `RUSTTY_TEST_EXE`, the CLI tests select Cargo's executable; build both
`rustty-app --bin rustty` and `tmuxx-cli --bin tmuxx` for that target first.

For a hidden macOS smoke capture with disposable panes:

```sh
RUSTTY_TMUXX_SMOKE_DIR=/tmp/tmuxx-smoke RUSTTY_TMUXX_SMOKE_FLOATING=1 \
  target/debug/rustty --tmuxx=embedded --config-default-files=false --stream-deck=false
```

It writes `window.png` and `result.json` and exits without activating a window.
