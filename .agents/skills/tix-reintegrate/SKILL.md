---
name: tix-reintegrate
description: Update the Rustty Tix stack onto mainline and port the incoming features, fixes, performance improvements, and benchmark coverage. Use when reintegrating or catching Rustty up with Ghostty mainline.
---

# Tix Reintegrate

Updating the base is only the first step. Complete the applicable mainline
improvements in Rustty and leave a commit-by-commit account of the incoming range.

## Preserve the range and history

Read the repository's `.agents/skills/tix/SKILL.md`. If it is absent, locate and
read the installed `tix` skill; do not invent its history-editing commands. Follow
its shared rules and `tix rebase-update [FILE]` workflow. This is a skill-level
workflow using `tix rebase todo --update-base`, not an installed `rebase-update`
subcommand.

Before applying the generated plan, retain the starting branch, checkout change
ID, old base, new base, and original visible commits and forks. Read prior
`test/rustty/UPSTREAM-*.md` reports to distinguish completed ports from outstanding
work. Keep the exact old-base-to-new-base range after the update; recomputing the
merge base afterwards loses the incoming changes. If the base is already current,
use the last recorded integration range and its unfinished items instead of
assuming there is nothing to port. Establish a missing boundary from history and
ask only when the evidence cannot resolve it.

## Reintegrate behavior

Inventory every commit in the range, including merges, and inspect the complete
diff. Group merges with their underlying changes without skipping their conflict
resolutions. Trace each behavior through Rustty's implementation, callers, native
hosts, tests, fixtures, configuration, exports, resources, and packaging as needed.
Ghostty production code remains a read-only reference; its changes inherited
through the rebase are expected. Reference tools may be adapted.

For each change, record one disposition with evidence:

- Ported: implement the behavior in Rustty's existing layers and carry its tests
  and consumers through to completion.
- Already equivalent: inspect the actual Rust path and demonstrate the relevant
  guarantee with existing checks or a focused regression.
- Inherited or not applicable: explain the concrete shared resource or different
  subsystem, such as a GTK-only callback or Zig allocator implementation.
- Unfinished: identify the remaining behavior and blocker explicitly.

A missing Rustty implementation is not by itself evidence of inapplicability.
Port features that belong on an existing Rustty surface. Avoid duplicating a
foreign toolkit or ABI where Rustty already provides the behavior. Keep older,
unrelated parity gaps distinct from regressions introduced by this range.

## Include performance and benchmarks

Inspect upstream hot paths, allocation changes, benchmark sources, fixtures, and
new workload definitions even when commit titles do not mention performance.
Reuse Rustty's existing harnesses in `test/rustty/` and `crates/rustty-vt/benches/`.
Port applicable new benchmarks with equivalent inputs, output checks, and setup
boundaries. Check native adapters for assumptions made obsolete by upstream;
an adapter's duplicated guard can hide a fixed production behavior.

For performance ports, check correctness first, then compare the before and after
versions under the same compiler, build options, workloads, and machine conditions.
Repeat measurements and account for noise; keep an optimization only when it
helps its intended workload without a material regression. Separate primitive
throughput from UI latency and idle CPU use. If no relevant performance change or
new benchmark arrived, say so; do not invent workloads or claim a speedup.

## Validate and finish

Use focused regressions and fresh native comparisons for changed behavior. Refresh
frozen fixtures and their source checksums when their generators or native
assumptions change; preserve coverage and historical compatibility cases. Run the
affected benchmark self-checks and appropriate Rust integration checks, excluding
interactive native suites unless they are needed. Keep build caches.

Commit each validated semantic change through Tix before beginning the next.
Amend related follow-ups into their existing commit, following Tix's review-mark
rules, and replay back to the intended checkout after ancestor edits.

Write `test/rustty/UPSTREAM-<date>.md` with the exact base range, complete commit
inventory, dispositions, port change IDs, validation, performance evidence, and
remaining gaps. Verify the new base's ancestry, retained visible history and forks,
clean task state, and absence of a saved rebase operation. Report local commit IDs
and push status. This workflow does not authorize a push, issue, or PR.
