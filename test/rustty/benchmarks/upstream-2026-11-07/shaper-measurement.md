# Production text-run and uncached-shaping benchmark

Committed: dbfbbc9 / mokoonq. Parent: 6aced98bc / tpnlmqr. This change ports the applicable ShaperRows benchmark from b01ca003c,1461f0687 / 3bd54e025. It is benchmark coverage, not a throughput optimization or UI latency claim. The private production row_text loop is compiled in rendering, run-finding and shape-every-run modes; only rendering emits rasterized quads. The existing shape path retains its result cache in production and bypasses both lookup/insertion for shape-every-run.

## Validation

- 62 rustty-render library tests passed; one new timing test is explicitly ignored by ordinary suites. New tests cover style splits, sprite boundaries, wide cells, combining text, invisible text, split-color wide cells, no rasterization, no result-cache use and the rendering specialization.
- Rust 1.95.0 Windows MSVC test-target compilation passed. Windows runtime was not available.
- Strict no-dependency Clippy for rustty-render/all-targets passed; scoped rustfmt and diff checks passed.
- The explicit shared 3,499-byte fixture passes all three native/Rust terminal delivery comparisons.
- Both native ShaperRows modes execute the same fixture successfully with one full-screen pass. Native benchmark was rebuilt with its supported -Dsentry=false flag to remove unrelated background crash-collector initialization from isolated tool checks. Earlier default build logged SentryInitFailed on its background thread; corrected runs contain no errors.
- Independent review found no implementation or timing-boundary issue. Native and Rust run-finding stages differ: native selects fallback fonts and ignores background-only style differences during run iteration; Rust uses actual row_text collection and performs font fallback inside shaping. Counts and timings are not interchangeable across those backends.

## Measurement boundaries

Apple M4 Max, Mac16,5, 16 logical CPUs, 64 GiB. cargo +1.95.0 --release --locked --offline; workspace thin LTO, one codegen unit. Binaries were built before measurement and frozen. No other agents built or measured during the serial comparisons. Existing caches were retained.

The new benchmark uses 120 columns,80 rows,200 full-screen passes,12-point bundled JetBrains Mono plus platform fallback. Its explicit Latin/Arabic/CJK/combining/emoji input and generator are shaper-fixture.vt and generate-shaper-fixture.py; exact hash is in shaper-fixture.json. File reading, terminal feed, font setup, foreground paints and one warmup pass are outside the timer. Temporary shapes drop per run; Rust has no separate end-frame arena. Modes exclude rasterization, quads, GPU and windows. Shape mode runs real shaping every time even when identical text repeats.

Three serial rounds alternated mode order. Each pass visits the 80 terminal rows and observes120 text-run/sprite items in runs mode or1,980 shaped-glyph/sprite items in shape mode. Raw JSON and logs are shaper-timings.json and shaper-{runs,shape}-{1,2,3}.log. Median per-pass times: runs26.72us (26.70..27.02us); shape-every-run1856.48us (1832.62..1859.70us). These are operation timings, not a before/after speedup, direct native comparison, or end-to-end UI latency.

## Production rendering control

The same prepare_frames source was built before and after this change. frame_compare.py measured four existing workloads serially in forward and reverse engine order,50 warmup plus50 measured frames per process (100 measured frames per variant/workload). Both use the same Menlo font,120x40 terminal and1200x850 render size. Snapshot capture and terminal advance are outside the renderer preparation timer. Raw samples, binary hashes and settings remain in shaper-frame-comparison/.

| Workload | Before median ns | After median ns | Change | Median Rust allocations |
|---|---:|---:|---:|---:|
| Cached redraw |6167|6041|-2.04%|6 ->6|
| Styled scroll |9333.5|9417|+0.89%|8 ->8|
| Mixed Unicode |11959|12041|+0.69%|7 ->7|
| Alternate repaint |113708.5|112854|-0.75%|6 ->6|

No material timing regression was observed; no speedup is claimed from this small range. Median requested allocation bytes also match. Exact allocation sequences are not identical: one extra26,760-byte allocation occurs in1-2/100 scrolling/repaint samples on both variants, with the affected cases varying. The raw samples preserve these outliers; they are not omitted to claim exact allocation equality.

Build and source evidence: shaper-frame-before.json, shaper-frame-after.json, shaper-test-binary.json; build/test/check logs share the shaper- prefix. No binaries or large generated corpora belong in the final documentation commit. Preserve this report, small shared fixture/generator, manifests, raw timing results and relevant validation logs there.
