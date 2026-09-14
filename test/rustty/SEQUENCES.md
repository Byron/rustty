# Rustty CSI/OSC implementation audit

This inventory follows the commands recognized by Rustty through terminal state,
PTY replies/input, rendering, and the macOS application. A state change is useful
when another operation consumes it: tab stops, margins, protected cells, and saved
modes do not need an immediate visible change. Parsing an arbitrary CSI final or
OSC number does not make that command supported.

The implementation paths are:

- [VT dispatch](../../crates/rustty-vt/src/terminal.rs): `csi`, `osc`, `osc9`,
  `osc133`, `sgr`, `set_mode`, and `set_stream_mode`.
- [Modes](../../crates/rustty-vt/src/modes.rs),
  [input encoding](../../crates/rustty-vt/src/input.rs),
  [colors](../../crates/rustty-vt/src/color.rs), and
  [query replies](../../crates/rustty-vt/src/query.rs).
- [PTY sessions](../../crates/rustty/src/session.rs): ordered effects, writes,
  actual host geometry, focus, visibility, and appearance reports.
- [Desktop](../../crates/rustty-app/src/desktop.rs),
  [native services](../../crates/rustty-app/src/platform.rs), and
  [rendering](../../crates/rustty-render/src/prepare.rs).

This is an implementation audit with selected regressions, not a claim of full
Ghostty parity. Parameter boundaries, snapshots, allocation failures, and native
reference failures remain tracked separately in [coverage.json](coverage.json)
and [the comparison guide](README.md).

## CSI commands

`CSI` means `ESC [`; spaces and intermediate characters in the table are
significant. Values describe the recognized operations, not arbitrary parameters.

| Command | Effect and consumer |
| --- | --- |
| `A`, `B`, `C`, `D`, `E`, `F`, `G`, `H`, `` ` ``, `a`, `d`, `e`, `f`, `j`, `k` | Cursor motion/position, including margins and origin mode; consumed by writes, cursor rendering, and position reports. |
| `I`, `Z` | Forward/backward traversal of the current tab stops. |
| `0g`, `3g`; `0W`, `2W`, `5W`; `?5W` | Clear/set/reset tab stops, consumed by tabulation. |
| `J`, `K`, `?J`, `?K` | Display/line erasure; selective forms honor protected cells. Erasure updates visible content, history, links, and image placement state. |
| `@`, `P`, `X`, `L`, `M` | Insert/delete/erase cells or lines with wide-cell and margin handling. |
| `S`, `T` | Scroll the selected region. |
| `b` | Repeat the preceding printable character. |
| `m` | SGR styling; see the attribute table below. |
| `>4;2m`, `>...n` | Enable/disable the supported xterm modify-other-keys mode; consumed by key encoding. Other modify-other-keys levels do not enable level 2. |
| `h`, `l`, `?h`, `?l` | Set/reset the supported ANSI/DEC modes listed below. |
| `?s`, `?r` | Save/restore supported DEC modes; restore performs their transitions and initial host reports. |
| `>s`, `>0s`, `>1s` | XTSHIFTESCAPE requests whether the application captures Shift with mouse input. The desktop applies `mouse-shift-capture` policy to both buttons and scrolling; snapshots preserve the request and RIS resets it. |
| `r` | Top/bottom scrolling margins, consumed by indexing, scrolling, erasure, and origin-relative positioning. |
| `s` | Left/right margins while mode 69 is enabled; otherwise saves the cursor. |
| `u` | Restore the saved cursor, attributes, origin, and character-set state. |
| `0 q` through `6 q` | Default/blinking/steady block, underline, or bar cursor; rendered by the desktop. |
| `0"q`, `1"q`, `2"q` | Character protection for subsequent writes and selective erasure. |
| `?Ps$p` | DEC mode status reply, including unsupported/permanently-reset states. |
| `5n`, `6n` | Device status and current cursor-position replies. |
| `?996n`, `?998n` | Actual host color-scheme and pane-visibility replies. |
| `c`, `>c`, `=c`, `>q` | Primary/secondary/tertiary device attributes and version replies. Version identifies as Ghostty for CLI compatibility. |
| `?u`, `>u`, `<u`, `=u` | Query/push/pop/change Kitty keyboard flags on the active screen; consumed by desktop key and composed-text encoding. |
| `14t`, `16t`, `18t` | Text-pixel, cell-pixel, and cell-grid size replies using the session's host geometry. |
| `21t` | Title reply when `title-report = true`; disabled by default, applied at startup and reload. |
| `22t`, `23t` | Title-stack operations: intentionally unimplemented in both reference handlers and Rustty. |
| `!p`, `$p` | DEC soft reset and ANSI mode query: ignored by the reference stream and Rustty. |
| `$}` | Select the status display; selecting the status area suppresses main-screen printing. There is no rendered status area. |
| `$~` | Status-display type declaration: no effect, matching the reference. |

## ANSI and DEC modes

The numbers below cover the entries in `modes.rs`. Save/restore and status
reporting alone do not imply a desktop implementation; state-only modes are
identified explicitly.

| Mode | Effect and consumer |
| --- | --- |
| ANSI 2 | Keyboard lock; physical keys and IME commits both use the VT encoder. |
| ANSI 4 | Insert mode during character writes. |
| ANSI 12 | Send/receive versus local echo: stored only; neither desktop implements local echo from this mode. |
| ANSI 20 | Received LF also performs CR; PTY input translates CR to CRLF, including ordered mode changes and RIS reset. |
| DEC 1 | Application cursor-key encoding, also used by alternate-screen scrolling. |
| DEC 3, 40 | Permission-controlled 80/132-column grid and clearing. Ordinary redraws retain it; a real host resize or mode-40 transition restores the host grid. |
| DEC 4 | Smooth-scroll flag: stored/reportable; no animated scroll implementation in either desktop. |
| DEC 5 | Reverse default foreground/background, including renderer color fallbacks. Explicit palette colors retain their values. |
| DEC 6 | Origin-relative cursor operations and reports; setting it homes the cursor. |
| DEC 7 | Automatic wrapping and primary-screen resize/reflow policy. |
| DEC 8 | Autorepeat flag: stored/reportable; native keyboard repeat is not controlled by it in either desktop. |
| DEC 9, 1000, 1002, 1003 | X10, normal, button-motion, and all-motion mouse reporting. |
| DEC 12, 25 | Cursor blinking and visibility, including both screens. The focused cursor uses the existing blink clock; typing restarts its visible phase. |
| DEC 45, 1045 | Reverse wrapping, with the extended mode's boundary behavior. |
| DEC 47, 1047, 1049 | Alternate-screen transitions, clearing, and cursor save/restore as appropriate to each mode. |
| DEC 66, 67 | Application keypad and Backspace/Delete encoding. |
| DEC 69 | Left/right-margin interpretation and operations; disabling restores full-width margins. |
| DEC 1004 | Current native focus is reported on enable/restore, then on host focus changes. |
| DEC 1005, 1006, 1015, 1016 | UTF-8, SGR, urxvt, and pixel mouse-coordinate encoding. |
| DEC 1007 | Without mouse reporting, scrolling the hovered alternate-screen pane sends cursor keys; disabling sends none. |
| DEC 1035, 1036 | NumLock/keypad policy and Alt escape-prefix encoding. |
| DEC 1039 | Stored/reportable; there is no separate consumer in the reference or Rustty. |
| DEC 1048 | Save/restore cursor state. |
| DEC 2004 | Bracketed paste encoding and paste-safety handling. |
| DEC 2026 | Synchronized output retains the complete prepared pane, including its cursor. End/timeout releases it; resize and atlas replacement invalidate it. Text/image animation pauses during the batch. |
| DEC 2027 | Grapheme-cluster construction and width handling. |
| DEC 2031 | Subscribe to actual host appearance changes. |
| DEC 2033 | Initial visibility report and subsequent host visibility changes. |
| DEC 2048 | Initial and resize-triggered in-band size reports. |
| DEC 5522 | Kitty clipboard/paste protocol selection and host policy. |

DEC 117 is additionally reported as permanently reset; it is not a mutable mode.
Unknown mode numbers do not create stored state or imply support.

## SGR attributes

| Values | Effect and consumer |
| --- | --- |
| `0` | Reset current rendition for subsequent writes. |
| `1`, `2`, `3`; `22`, `23` | Bold, faint, italic and their resets; font selection and foreground opacity. |
| `4`, `4:0` through `4:5`, `21`, `24` | No/single/double/curly/dotted/dashed underline and resets; renderer decorations. |
| `5`, `6`, `25` | Text blinking and reset. Both blink forms use the same 600 ms phase. Visible blinking text/decorations schedule redraws even with a hidden/steady cursor; unfocused panes remain visible without blink wakeups. |
| `7`, `27`; `8`, `28` | Inverse and invisible text with resets; resolved foreground/background and glyph/decorations visibility. |
| `9`, `29`; `53`, `55` | Strikethrough/overline and resets; renderer decorations. |
| `30`–`37`, `40`–`47`, `90`–`97`, `100`–`107` | Indexed foreground/background colors. |
| `38`, `48`, `58` | Extended palette/RGB foreground, background, and underline colors; semicolon and supported colon forms. |
| `39`, `49`, `59` | Reset foreground, background, and underline colors to defaults. |

Native Ghostty retains the blink attribute without rendering blinking text;
Rustty's existing visual blink implementation now has independent scheduling.
Unknown attributes and malformed colon groups do not enable unrelated styles.

## OSC commands

`OSC` means `ESC ]`; the command ends in BEL or a supported string terminator.
Capture limits and invalid input can reject a request before these operations.

| Command | Effect and consumer |
| --- | --- |
| 0, 2 | Window/tab/pane title and activity updates. OSC 0 uses the title path; icons are not changed. |
| 1 | Icon title: deliberately ignored, as in Ghostty. |
| 4, 104 | Set/query/reset palette entries 0–255; the renderer consumes the palette, and queries return PTY replies. Special indices 256–260 have no implementation. |
| 5, 105 | Special-color set/reset: parsed but ignored in both implementations. |
| 7 | Working-directory state; the desktop decodes local paths/file URIs for labels and new-session directories. |
| 8 | Explicit/implicit hyperlink state attached to cells; link hit-testing and the open action consume it alongside regex links. |
| 9 | Desktop notification, except for recognized ConEmu extensions below. Native notification delivery follows macOS permissions. |
| 9;4 | Remove/ranged/error/indeterminate/paused progress; pane progress bars, tab activity, timeout, and animation scheduling. |
| 9;9 | Working-directory alias, using the same consumer as OSC 7. |
| 9;12 | Fresh-line/prompt semantics. |
| Other recognized ConEmu extensions | Sleep, message boxes, tab title, wait-input, GUI macro, comments, emulation, environment output, and process launching are deliberately ignored by both terminals. |
| 10, 11, 12; 110, 111, 112 | Set/query/reset default foreground, background, and cursor colors; used in rendered frames and replies. |
| 13–19, 113–119 | Pointer/Tektronix/highlight colors and resets: intentionally unsupported in both implementations. |
| 21 | Kitty palette/foreground/background/cursor query, set, and reset. Selection colors, cursor text, visual bell, and second transparent background keys are parsed but unsupported, matching the reference. |
| 22 | Validated W3C pointer shapes and supported xterm/foot aliases; the native pointer follows the hovered terminal pane, with UI/divider cursors taking precedence. |
| 52 | Clipboard read/write requests, binary decoding, reply terminators, and native policy/confirmation. The macOS primary selection is unsupported. |
| 66, 99 | Kitty text sizing and Kitty notifications: no terminal/desktop effect, matching the reference's unimplemented callbacks. Generic notifications use OSC 9/777. |
| 72 | Kitty drag-and-drop state, callbacks, and host APIs exist in the VT crate; the native desktop drag handshake is missing in both apps. Ordinary file drops insert paths into the hovered pane. |
| 133 | `A`, `N`, `P`, `L` prompt/fresh-line handling; `B`, `I` input/EOL semantics; `C`, `D` output and command lifecycle. Prompt navigation, resize/redraw policy, activity, and completion notifications consume these. |
| 777 | `notify;title;body` produces a native notification. |
| 1337 | `CurrentDir` uses the directory consumer; `Copy=:` uses OSC 52 decoding and clipboard policy. Other iTerm2 keys are unsupported. |
| 5522 | Kitty clipboard transfers, MIME selection, grants, paste events, bounded decoding, and replies; native clipboard access honors configured policy. |

OSC 133's `cl` and `click_events` hints are retained/exposed with semantic metadata
but do not drive editor click forwarding in the desktop. Prompt `redraw` and `k`
options do affect terminal behavior. OSC 3008 context signals are not dispatched
by Rustty; Ghostty recognizes their metadata but also has no terminal callback.

Shift overrides mouse reporting for local selection by default. XTSHIFTESCAPE
can change that preference unless `mouse-shift-capture` is `always` or `never`.
This inventory does not equate arbitrary parser events, snapshot-only fields,
or native parser recognition with Rustty support.

## Regression coverage

This audit repaired reverse video, OSC 22 pointer shapes, alternate scrolling,
initial/current focus reports, IME keyboard lock, linefeed input translation,
DEC column mode, text-blink scheduling, and title-report configuration.

The focused VT run covers terminal operations, modes, SGR, colors, OSC strings,
queries, host/shell effects, semantics, stream validation, clipboard, and DND.
Session tests exercise actual PTY bytes for linefeed/focus reports and repeated
host geometry for column mode. Renderer tests compare visible geometry across
blink phases, including clipped/hidden text and whitespace.

The [native smoke test](../../crates/rustty-app/src/smoke.rs) additionally checks
rendered reverse colors, retained 80/132-column grids, hovered pointer/scroll
targets, synchronized frames/cursors, and text-blink deadlines. Its existing
hidden-tab and idle checks guard against extra pane work and wakeups.
Offscreen Metal capture checks prepared rendering, not physical presentation.

One audit smoke run produced 360 redraws during the three-second masked-title
phase. Changing pane focus during the animation reproduces this failure: the
check stopped progress on the newly focused pane and left the original pane
animating. The smoke check now retains its animation target and deliberately
changes focus before stopping progress. This reproduction needed no production
scheduling change; the original run lacked diagnostics to establish its cause.
Failures include event counts, pane progress, the host deadline, and egui repaint
causes so a recurrence can identify its source without production tracing.

Run focused checks from the repository root, without Zig:

```sh
cargo test -p rustty --features sessions --offline
cargo test -p rustty-app -p rustty-render --offline
cargo test -p rustty-vt --offline --test terminal --test modes --test sgr \
  --test colors --test osc_strings --test queries --test host_queries \
  --test host_effects --test host_shell_events --test semantic \
  --test stream_controls --test clipboard_kitty --test dnd
```
