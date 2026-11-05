# ARM64 evidence for the scrolling follow-up

All observations come from optimized frozen binaries identified by SHA-256 in
the stage JSON files. The final binary is the `simd` entry in [final.json](final.json);
that runner label does not mean new SIMD code was added. Rustty uses native CPU
flags, thin LTO and one codegen unit. Disassembly used LLVM objdump on Apple M4 Max.
Static instruction counts include cold blocks and are not cycle counts.

## Retained changes

| Change | Observed code generation | Whole-workload evidence |
| --- | --- | --- |
| Reuse resolved row IDs | Removes the extra Screen::row call from print_ascii | Styled ASCII 1.8–2.3% faster |
| Complete ordinary CSI path | No per-byte state-table transitions or temporary retain_pending calls inside accepted commands | Styled ASCII and Chinese about 8% faster |
| Active rows from page-list tail | row_header_mut 984→488 bytes; frame 80→32 bytes; no full-history summation and forward traversal | Styled ASCII 1.6% faster |
| Group initialized style flags | Wide cursor flag loads; sync Color::eq calls 3→1; setter's three color comparisons inline | Styled ASCII 2.5–3.4% faster |
| Skip disabled capture hooks | One flag test skips both helpers; Unicode also skips its four-way unrolled non-ASCII scan | Styled ASCII 1.0–1.7% faster |
| Reuse prepared cursor location | Ordinary print_ascii avoids the sync wrapper, repeated lookup and nested 48-byte frame; refreshes after wrap or slow sync remain | Styled ASCII 1.5–2.1% faster |

These are sequential stage measurements, not additive percentages. The final
paired comparison measures the combined 16.2% improvement directly.

The final ASCII writer still stores sixteen eight-byte cells per main iteration.
Selected store/loop instructions below omit intervening widening and OR operations:

```asm
1001994b8:     	stp	q1, q5, [x0, #0x60]
1001994d4:     	stp	q3, q5, [x0, #0x40]
1001994d8:     	stp	q2, q4, [x0, #0x20]
1001994e0:     	stp	q1, q2, [x0], #0x80
1001994e4:     	subs	x12, x12, #0x10
1001994e8:     	b.ne	0x100199470 <__RNvMNtCsaO64Y4R4Hfb_9rustty_vt8terminalNtB2_8Terminal11print_ascii+0xb74>
```

There are no per-cell bounds checks in this store loop. Page, slice and resource
checks remain outside it. The row-header optimization retains revision-array
bounds checks, division guards and atomic revision updates. The location change
increases print_ascii by 908 bytes to 3,812 bytes (+31%), but leaves its 224-byte
frame and vector stores unchanged; paired measurements justified that tradeoff.

The grouped-style setter uses two wide flag loads and one combined branch:

```asm
10020087c:     	add	x21, x0, #0x3ba
100200880:     	ldr	x8, [x21]
100200884:     	ldrb	w9, [x0, #0x3d0]
100200888:     	ldrh	w10, [x0, #0x3ce]
10020088c:     	orr	w9, w10, w9, lsl #16
100200890:     	ldr	x10, [x1]
100200894:     	ldrb	w11, [x1, #0x16]
100200898:     	ldrh	w12, [x1, #0x14]
10020089c:     	orr	w11, w12, w11, lsl #16
1002008a0:     	cmp	x8, x10
1002008a4:     	ccmp	w9, w11, #0x0, eq
1002008a8:     	b.ne	0x1002009c4

```

This does not assume a Rust struct layout: source explicitly assembles initialized
fields with from_le_bytes. In cursor validation, LLVM only partly coalesces the
admitted-side loads because its first bool is already loaded for Option's niche
check. The observed gain includes changed color-comparison inlining decisions,
not just wider flag comparison.

## Rejected experiments

[Cached-style measurements](rejected-cache.json) show no useful gain: styled ASCII
10.787→10.835 µs. The cache did remove the style-ID bounds, live-reference and
Option checks, reducing sync_cursor_resources from 209 to 180 static instructions.
However, style installation grew from 214 to 287 instructions and its frame from
80 to 192 bytes: retaining the admitted style across acquisition introduced
copying/spills and reconstruction of 23 individual bytes. This experiment does
not isolate the cost of bounds checks; it shows the complete cache was not faster.

The [style comparison qualification](style-equality.json) also records full color
packing. That variant removes all three sync Color::eq calls and shrinks the frame
from 48 to 32 bytes, but expands sync to 257 instructions versus 225 with grouped
flags. It adds tag dispatch and payload packing for both operands. Styled ASCII
was 10.631 µs versus 10.082 µs for grouped flags; plain ASCII also regressed.
The default path was unchanged, so static assembly alone cannot explain every
part of that regression. Removing calls is not sufficient evidence of a speedup.

A fixed-field style-hash experiment removed the loop and variable 128-bit shifts
but produced only about 1% styled improvement and 1% plain regression; it was
removed. The earlier safe four-cell destination-scan experiment likewise emitted
a shorter vector loop and hoisted mask but changed total time by less than 1%.
No revisions, public cursor checks, pending-input behavior or bounds guarantees
were removed from the retained implementation.

## Remaining work

The [separate final profile](profile.txt) still samples printing, CSI parsing,
row growth/pruning, resource validation and style admission. It was collected for
four seconds at 1 ms intervals during a seven-second profiling run, after the
unsampled comparison completed. Sampled stacks are approximate attribution;
there is no claim that one remaining function explains the entire Ghostty gap.

The workload subtraction in the [report](README.md) puts 68% of the remaining
styled gap in the plain-scrolling baseline. Further investigation should account
for that shared page/scroll/write work as well as styling. The current evidence
does not justify blanket unchecked indexing or a SIMD rewrite.
