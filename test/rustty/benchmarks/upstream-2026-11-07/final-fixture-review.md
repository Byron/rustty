# Final fixture refresh review

All eight preservation-audit review categories are accepted as explicit fixture transformations at `f2b7e1344`. The original `final-fixture-audit.json` remains unchanged with `status: review_required`, zero errors and eight review categories. This is a source and archive review, not runtime parity validation.

`review-final-fixture-sources.py` reproduced all 363 changed requests, including their coverage lists, from the exact historical/current Git sources and saved native probe replies. It opened no oracle. `final-fixture-source-review.json` records source hashes, probe hashes and exact generated case IDs.

| Review category | Concrete reason |
| --- | --- |
| Two OSC requests | Upstream `c7be56a04` replaced both OSC 3008 corpus seeds with hierarchical context metadata and escaped values. The archived bytes match those files exactly. |
| 247 page-layout requests | Independent hyperlink-map capacity adds only `hyperlink_cells`: 183 explicit zero values and 64 independently seeded random values. Other request fields remain unchanged. |
| 76 page requests | 57 lifecycle lengths follow capacity; 12 requests embed regenerated native snapshots; five style cursor/lifetime cases use capacity plus one; two style split cases use new canonical-hash collisions. |
| 16 removed page IDs | Each still has its boundary role covered at the new native capacity. Some replacement roles reuse an existing numeric ID, so the review records exact role mappings rather than inventing one-to-one renames. |
| Three removed native probes | Their replacements still write `x\r\n * (capacity + 31)` before snapshot capture: 5,356→5,361 lines at width 8; 622→623 at width 80; 252→253 at width 215. |
| Seven changed native probe responses | Only page-layout fields differ: style items become 32 bytes with 16-byte alignment, style storage 4,000→3,584 bytes, metadata 26,124→25,708 bytes, and `hyperlink_cells=64` is explicit. These direct layout probes do not exercise runtime carry sampling. |
| 37 snapshot-wire requests | The 36 collision cases and one displacement/refcount-tie case choose values for the new canonical style hash, while preserving source-defined wire and resource construction. |
| One snapshot request | Its RGB stream now uses the first 33 values colliding in bucket 127 under the canonical hash. Dimensions and operations are unchanged. |

Native row capacity changes from 15,975→15,993 at width 2; 5,325→5,330 at width 8; 591→592 at width 80; 221→222 at width 215. Width 1,024 stays at 46 rows. The compact exact native field/probe deltas are in `final-pages-probe-review.json`; all 16 removed-ID role mappings, request hashes and coverage lists are in `final-fixture-review.json`.

The refresh adds 188 IDs and removes those 16 capacity-derived IDs, for 172 additional cases. New IDs cover 54 mixed-width cases, 44 linked-batch cases, 48 independent layout cases, three saturated snapshot hints, 11 semantic ED gate cases, five graphics cases, three pending-wrap formatter cases, one line-drag case, three fresh Rust DND cases, and 16 replacement capacity IDs. Two additional page probes query the new batch cases' 200-column capacity; one snapshot-wire probe captures their independent-map saturation template at 200×150 cells. Every added ID is listed and attributed in the JSON.

No coverage tag count decreases, per-case coverage losses or source membership losses occur. The three original Rust DND request/coverage records remain byte-identical under their original IDs, alongside three current Rust and three Zig cases. Preservation does not claim successful runtime cross-decoding: root is assessing current and baseline runtime results separately. No raw runtime failures were suppressed or relabeled here.
