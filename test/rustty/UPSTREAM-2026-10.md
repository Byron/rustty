# Ghostty upstream review: October 2026

Reviewed `e2e53f861482e080bf45054ba49ef471f9849937..a2e440396403385680008e456a741a7776091412`
(484 commits), and rebased the 19 existing Rustty changes onto the latter local
`main`. The original change IDs and both Rustty branch tips were preserved.
All new and replayed Rustty commits use Codex authorship and conventional titles.

## Selected correctness ports

Each selected bug was reproduced in Rustty before its fix. Existing tests that
encoded the older Ghostty behavior were updated to the corrected behavior.

| Upstream | Rustty change and evidence |
| --- | --- |
| `5c9a3d8a9`, `cc53940b4` | CR now triggers paste protection like LF; framing terminators remain rejected. VT and desktop policy matrices cover bracketed-paste settings. |
| `520d8f55a` | CAN/SUB discard cancelled OSC commands. Title, clipboard, and palette regressions cover every input split; direct dispatch preserves pending stream input. |
| `f9ab34f10` | Resize repairs live pending-wrap state. Tests cover widening, shrinking, height changes, saved cursors, and wide characters. |
| `157c44f64`, `e55c687a4` | Mode 1005 UTF-8 encodes extended mouse buttons; Ctrl+Alt+Shift+Backspace emits ESC+BS. Tests also retain DECBKM and modifyOtherKeys behavior. |
| `c4f15c884`, `a3e80a685` | Word selection stops at hard newlines and resolves wide-character spacers, including soft wraps and custom boundaries. |
| `6220a3617`, `c706451fe` | Legacy character sets preserve Unicode above their range; SS2/SS3 apply to one incoming scalar, including combining characters. Previously written graphemes are never remapped. |
| `bb20f8e45` | RIS restores the configured palette and clears palette overrides. Later theme updates apply again; dynamic-color behavior remains intact. |
| `ff779564c` | ED2 at a prompt retains output above trailing blank rows. Real output below the prompt still prevents prompt-specific scrollback preservation. |
| `b10ba7ea2` | Grapheme widening retains the source text through scrolling. Regressions cover lost ZWJ suffixes and combining marks stolen from unrelated history. Existing shared text and bounded allocation paths are reused. |
| `f9e8ef02d` | macOS checks detected and OSC8 links before OS dispatch: canonical local-file validation, executable/script/application denial, and consent for custom schemes. Regression tests never open applications. Windows dispatch policy is unchanged. |

Bash exit-status and PROMPT_COMMAND-array fixes (`591ccacbd`, `4dfa44ecd`,
`ed7f046ee`, `a4031c8e3`) arrive through the shared shell-integration resources
copied by `build.nu`; they do not need duplicate Rust implementations.

## Performance evaluation

The selected hypothesis is `a4f0d9f4a`: batch DEC Special Graphics and British
character-set writes. Rustty still used scalar printing for these runs, while
ordinary ASCII already had a packed-cell batch path. The new benchmark module
is [charset_bench.rs](charset_bench.rs).

Before timing, both frozen release executables validate cell mappings, styles,
widths, cursor/pending-wrap state, bounded history, and REP after changing the
active character set. The before executable includes all selected VT correctness
fixes. Identical workloads run serially in forward and reverse order with 50
samples per direction. Timing setup excludes verification and terminal creation.

The candidate was retained: DEC repaint improves **10.25×**, DEC streaming
**9.00×**, British repaint **13.08×**, and British streaming **10.57×**.
Both measurement orders improve mapped workloads by more than 88%; the worst
ordinary ASCII regression in either order is 1.73%, below the 5% guardrail.
These are VT feed microbenchmarks, not end-to-end GUI frame-rate claims.
See [the measurements and reproduction commands](CHARSET-BENCHMARK-2026-10.md).

## Already covered or unsuitable for direct porting

| Candidate | Why it was not copied |
| --- | --- |
| Cursor-page UAF (`65ff54c67`) | Rustty owns styles and shared hyperlinks rather than retaining the unsafe native page pointer. |
| Kitty alias leak (`44c4c8f55`) | Replacing the Rust vector already releases the old aliases. |
| Snapshot capacity (`6f3a4fade`) | Rustty's page-layout validation already checks capacity. |
| No-reflow wide truncation (`e6db5b633`) | Existing `repair_wide` handles the cut. |
| Reverse wrap (`d4f45bee3`) | The Rust loop already guards the remaining count. |
| PTY resize ordering (`f2571a3ec`) | Rustty resizes terminal state under its lock before queueing the PTY resize. |
| Window-state reload and IME font thickening | Rustty already reads current configuration and shares its configured glyph rasterizer. |
| Shared GPU device, Wuffs/font-cache changes | Rustty already shares its device; image decoding and font-cache architectures differ. |
| Single-pass integer parsing | Already present in Rustty. |
| Sorted mode lookup (`079502e23`) | No measured Rustty gain; constant queries already optimize away. |
| Cross-page grapheme performance (`6dae79db9`) | Correctness was repaired first; no independent measured speedup is claimed. |

## Deferred correctness candidates

These remain explicit follow-ups, not claims of parity with every upstream fix:
ConPTY scrollback-pull policy (`c55f213aa`, needs Windows validation), saved-cursor
widening (`afded91df`, Rustty uses row identities; needs a repro), empty OSC22 reset
(`b1bfd1d9c`), bare tab-clear CSI g (`d0c0d4008`), private DECRQM identifier masking
(`3beb6d717`), semantic line-selection trimming (`13b5ab204`), VT-export cursor
restoration with margins/blank pending-wrap cells (`23c7a3e5a`, `bae1c01fc`), and
numeric underscore rejection (`73768913b`). These were outside this batch of
proven, higher-impact fixes. Windows URL-dispatch hardening also needs a native
policy and runtime validation rather than a mechanical copy of macOS checks.

## New features available for individual selection

No new feature in this table was automatically ported.

| Feature | Upstream / selection notes |
| --- | --- |
| Detach a tab into its own macOS window | `1a8f331f1`, #14216. Rustty currently reorders tabs but has no detach action. |
| Close Split in the terminal context menu | `44bdcc623`, #11174. Existing Rustty CloseSurface action can back the UI entry. |
| Application-requested window resizing | `714fe9b90`, `219cca932`, #14375. Requires CSI 8 t, host handling, and the opt-in setting. |
| Unicode 18 character/grapheme data | `12542b392`, #14293. Rustty currently uses Unicode 17. |
| Soft terminal reset (DECSTR) | `f473f3109`, #14538. Currently ignored by Rustty. |
| Extended cursor position and screen dimensions | `fbda61b87`, `6104c78f1`, #14673/#14633: DECXCPR and CSI 19 t. |
| ANSI DECRQM queries | `9dc0d974e`, #14236. Separate from Rustty's existing private-mode queries. |
| Screen checksums for terminal test tools | `9272a2f7c`, #14483: DECRQCRA/XTCHECKSUM; opt-in because replies expose screen contents. |
| Preserve cell hyperlinks in VT exports | `510b253db`, #14711. Rustty currently emits per-cell links only in HTML. |
| Refresh bundled themes | `66031576b`, #14634. Rustty assets are the August 31 snapshot; main has October 5. |

GTK fractional scaling and inverted close buttons are specific to Ghostty's GTK
host, while Rustty ships macOS/Windows. Theme-chooser Ctrl-D/Ctrl-U needs an
interactive chooser Rustty does not currently provide. Overscan is groundwork,
not a completed smooth-scrolling feature. OSC7501 is ignored by the Ghostty app;
its new C API prompt navigation already has a Rustty equivalent.

## Validation

Passed the full Rustty VT suite (374 tests), parser unit/continuation/corpus
suite (16 tests), all 42 app binary tests, six native link-policy tests, and all
six charset benchmark output checks. Native `test-vt-primitives`, `vt-oracle`,
and filtered `test-lib-vt` checks for cursor resources, mouse UTF-8, truncation,
and cross-page clearing passed after resolving the rebase conflicts.

Workspace Rust formatting and whitespace checks pass. App Clippy passes with
warnings denied. Parser/VT all-target Clippy passes with the pre-existing
`chunks_exact_to_as_chunks` lint allowed: Rust 1.99 introduces two warnings in
unchanged `test/rustty/kitty_bench.rs` assertions. No unrelated code was rewritten
to silence them.

POSIX shared-memory tests required execution outside the filesystem sandbox.
No interactive GUI/URL dispatch, Windows runtime, full Ghostty application
build, or exhaustive cross-engine parity sweep was run. New upstream features
and the deferred candidates above prevent claiming complete Ghostty parity.
No push or remote CI run was requested.
