# Windows format matching contract

Windows has no UCRT POSIX regex or fnmatch API. `tmuxx-format` uses this pure Rust
engine for native ERE matching, substitution and translated globs. Unix builds
retain the existing private-locale libc implementation. The Rust implementation
does not call C tmux, change remote C-server values, or reinterpret protocol data.

Matching chooses the earliest whole-match start, the longest whole match at that
start, and then the longest earlier numbered captures. Captures refer to original
byte offsets. A valid UTF-8 scalar is one character; a malformed byte occupies its
own symbol above the Unicode range. It cannot alias U+FFFD or another malformed
byte. Regex syntax accepts POSIX ERE operators, bracket classes, single-character
collating symbols and equivalence classes. Backreferences must refer to an
already closed capture. NUL is rejected by glob inputs; regex NUL inputs retain
the existing C-string admission behavior (no match or unchanged substitution).

Windows matching uses a deterministic Unicode locale, independent of the GUI
thread's CRT locale. General categories come from Unicode 16.0, pinned by
`unicode-general-category` 1.1.0. Letter categories include letter numbers;
decimal and hexadecimal digit classes use ASCII. Unicode separators and U+0009
through U+000D are whitespace. Ranges use scalar order, not a machine's installed
language collation. Case comparison uses the pinned Rust toolchain's Unicode
lowercase mapping; multi-character uppercase expansion never expands the input.
Regex case folding also applies to class membership; glob FNM_CASEFOLD-style
matching preserves explicit upper/lower class membership.

An equivalence class such as `[[=e=]]` compares canonically decomposed base letters
after case mapping and removal of combining marks. Standalone combining marks
remain distinct. A single-character collating symbol such as `[[.界.]]` is literal.
This explicit Windows locale is not a claim of identical collation across every
Unix language locale. Native executable expectations cover these rules; complete
C-reference parity remains an independent conformance requirement.

Substitution retains C tmux's replacement spelling (`\0` through `\9`), literal
digit behavior for absent or empty captures, leading-`^` single replacement, and
one-byte progression after a suppressed empty match. That progression can split
UTF-8 in the output, exactly as the byte-oriented C substitution loop does; no
replacement-character conversion hides the result. Ordinary matches advance by
Unicode characters and return exact byte spans.

Resource errors propagate through `content_match`, the core pane-content format
dispatcher and format expansion. They never become a negative match or an
unchanged replacement. Limits are 65,536 pattern bytes (including translated glob
syntax), 128 parser frames, 1,023 captures, 16 MiB input, 65,536 expansion nodes,
1,000,000 capture-history entries and 2,000,000 matcher work steps. The work budget
is shared across all searches of one global substitution. Substitution output
retains the existing 1 MiB format limit. Input decoding is lazy to avoid repeatedly
decoding the unsearched tail during substitutions. Invalid regex syntax retains
C tmux's no-match/unchanged-substitution behavior.

Native Windows validation:

```powershell
cargo +1.95.0 test --locked --offline --manifest-path rust/vendor/posix-regex/Cargo.toml
cargo +1.95.0 test --locked --offline --manifest-path rust/Cargo.toml -p tmuxx-format --lib
cargo +1.95.0 test --locked --offline --manifest-path rust/Cargo.toml -p tmuxx-core --lib
```

`rust/conformance/windows-format-evidence.toml` maps the exact tests. This includes
6,561 short malformed/nullable pattern candidates and explicit complexity/input/
output failures. These are native algorithm and core-dispatch tests; they do not
replace ConPTY, desktop, SSH or complete format-reference qualification.
