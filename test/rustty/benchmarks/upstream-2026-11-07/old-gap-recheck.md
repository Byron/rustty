# Recheck of older behavior findings

Read-only follow-up for the report. No source changes, builds, heavy tests or
timings. Ran only small live-generator comparisons, after the performance agent
explicitly allowed them outside its timing window. Used existing binaries with
explicit `--rust-bin target/debug/examples/parity --zig-bin zig-out/bin/vt-oracle`.
Exact executable hashes are in `old-gap-binaries.json`.

## Saved-cursor resize: already resolved before this reintegration

The October 26 report describes Rust column 4 versus native column 5; October 28
repeats it. The November 4 report names the minimized fixture:

`terminal/resize/retained-viewport-pin-before-saved-cursor`

Its `generated_sources` field is `generated/2/187`. The exact minimal case starts
at 12x4, feeds `TAB LF ESC[?1049h`, resizes to 6x4, then feeds `ESC[?1049l`.
The original generated case remains reproducible from
`parity_reference.generated_requests(2,188)` by selecting ID `generated/2/187`.
Two related live grid requests also remain in the frozen grid archive:

- `grid/viewport-retention/empty/active/none/6/False`
- `grid/viewport-retention/empty/active/none/6/True`

November 5 fix `c170d01492edd9786455e46a682348b6153b19df` is an ancestor of the
initial Rustty `c06197283`; the current rebased representation is
`d97e6c277a56facbcccc2e23fd0c774059027d53` / `osvyuyn`. The old/new Git hashes are
different because this task rebased the stack. The correction computes `pin_x`
at the start of each source row from the actual destination position, using zero
after a hard break or pending wrap. The previous code retained the prior row's
column. This corresponds to native `afded91dfa`, which precedes old main
`6ba144158`.

Existing committed evidence `test/rustty/validation-saved-cursor-2026-11-05.json`
reports 109 focused tests and 21 resize comparisons. Current source tests include
`resize_clamps_saved_and_external_pins_after_a_hard_line_break` and
`resize_blank_pins_follow_hard_breaks_soft_continuations_and_pending_wrap`.

Fresh command:

```
python3 test/rustty/parity_reference.py --no-build \
  --rust-bin target/debug/examples/parity --zig-bin zig-out/bin/vt-oracle \
  --fixtures /private/tmp/rustty-reintegrate.leadij/old-saved-cursor-cases.json \
  --artifacts /private/tmp/rustty-reintegrate.leadij/old-saved-cursor-after
```

Result: **15/15 pass**: minimal case, original generated case, two live viewport
variants, and snapshot cross-decode, each in full/scalar/chunked delivery. Both
engines end the minimal case at zero-based **x=5,y=1,pending_wrap=false**. Raw
responses are `old-gap-witnesses-{zig,rust}.json`; runner log is
`old-saved-cursor-after.log`. Remove this mismatch from current limitations.

## Blank initial line press: earlier limitation retracted

The earlier behavior audit incorrectly claimed native had an untrimmed fallback
for initial blank line selection that Rust lacked. In fact, both current and
pre-range native `SelectionGesture::pressSelection` call trimmed `selectLine`,
just as Rust `SelectionGesture::press` does. The untrimmed fallback is in the
drag path, including its anchor. An all-blank initial line therefore returns no
selection in both implementations.

Fresh existing generator case
`grid/gesture/line/(0, 1)/(8, 0)/None` starts on the blank second row of
`alpha beta CRLF CRLF word.two_more...`, then drags into the first text row.
That case and its press-only prefix match in **6/6 comparisons**. The fixtures
and log are in `preexisting-gap-cases.json` / `preexisting-gaps-after/`.
This is not a current parity gap and must not appear in the report's limitations.

## Origin-mode formatter: concrete preexisting difference

Native fix `23c7a3e5a` (October 7) is an ancestor of old main `6ba144158`.
Rust's absolute cursor emission is present in the original `c06197283` tree.
The new margin-pending-wrap port did not change this coordinate convention.

Two exact native formatter regression streams at 20x10, exporting all extras:

| Input | Final native CUP | Final Rust CUP |
| --- | --- | --- |
| `ESC[3;8r ESC[?6h ESC[2;5H` | `ESC[2;5H` | `ESC[4;5H` |
| `ESC[?69h ESC[3;8r ESC[4;15s ESC[?6h ESC[2;3H` | `ESC[2;3H` | `ESC[4;6H` |

All **6/6 comparisons reproduce** the output difference. Restored DECOM makes
CUP relative to the scrolling margins, so the absolute Rust values are offset a
second time on replay. These are `grid/terminal-format/preexisting-origin-{0,1}`
in `preexisting-gap-cases.json`; raw full exports and diffs are under
`preexisting-gaps-after/failures/`. Keep this only as a preexisting limitation.

## Local image eviction priority: concrete preexisting differences

Use an 8-byte graphics budget and three 1x1 RGBA images (4 bytes each),
transmitted in order **9,1,3**. All payloads are `01 02 03 ff`.

- **Placed priority:** image 9 uses `a=T,C=1`; images 1 and 3 use `a=t`.
  Native evicts unused image 1 and retains [3,9]; Rust evicts oldest image 9
  and retains [1,3], also removing its placement.
- **Transient priority:** all images use `a=t`; image 1 additionally uses `N=1`.
  Native evicts transient image 1 and retains [3,9]; Rust retains [1,3].

Both witnesses have explicit graphics observations and reproduce in all three
delivery modes: **6/6 expected differences**. Correct fixtures are
`preexisting-eviction-cases.json`; log, summaries and raw failures are in
`preexisting-eviction-after/`. Direct raw outputs are also in
`old-gap-witnesses-{zig,rust}.json`.

The native priority tiers already exist at `6ba144158` in
`graphics_storage.zig:Candidate`. Original Rustty `c06197283` lacks those tiers
and evicts by generation. Incoming `4441aabca` changes only native age from
generation to transmission, within the existing tiers; Rust port `725beff76`
corrects age to stable identity without adding the older absent tiers. The
incoming-age regression cases deliberately use equal placement/transience.

Evidence caveat: the first two draft image requests in
`preexisting-gap-cases.json` omitted `observe` under kind=input, so their six
passing comparisons did not inspect image state. They are not evidence of
eviction equivalence. The separate observed fixtures above replace those checks.

## Report disposition

Current older limitations supported by this recheck: **origin-mode cursor export
and local placed/transient image eviction priority**. Saved-cursor shrink is
already resolved; blank initial press is equivalent. No new regression in the
incoming November range was found by these checks, and no complete parity claim
is implied. The final broader fixture refresh remains root-owned.
