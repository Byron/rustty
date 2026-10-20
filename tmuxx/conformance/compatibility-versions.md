# C reference versions

The Rust engine follows the pinned current C baseline. Older C servers remain
interoperability targets for the Rust client; their historical engine behavior
is not silently substituted for current semantics. Exact tests are selected by
version. The same raw observations are compared without version normalization.

Observed current versus C3.7c differences:

| Contract | Current baseline / Rust engine | C3.7c | Reproducer |
| --- | --- | --- | --- |
| Process-exit close events | pane-exited, session-closed, window-unlinked | pane-exited, window-unlinked, session-closed | runtime hooks `lifecycle_hook_dead_payload_and_order_matches_c` |
| Hook/event waiting | wait-for -E/-F/-l/-w and custom events | Newer event flags unavailable | runtime scheduler `event_wait_filter_nojobs_verbose_listing_and_wake_match_c` |
| Option arrays/output fields | String keys and modern show-options format metadata | Historical indexing/format behavior | core options `head_array_keys_and_output_formats` |
| Group-name reuse | New unrelated session does not join an existing group with matching name | Historical unintended join | core graph `group_name_reuse_does_not_join_unrelated_group` |
| Existing window -S placement | Explicit-index handling follows current C | Historical selection behavior | core graph `window_placement_replacement_insertion_and_last` |
| Join-pane before geometry/list | Pane list and geometry both insert before the target | Geometry inserts before, list/index order inserts after | core panes `join_and_break_preserve_pane_identity_and_nested_geometry` |
| Format q escaping | Also backslash-escapes braces, newline and tab | Those bytes are left literal | config `format_observations_match_c` |
| Loop metadata | loop_index is populated | loop_index is absent | core formats `contextual_session_window_pane_loops_and_name_checks_match_c` |
| Format t/r | Relative timestamp formatting, empty for future time | Flag unavailable; ordinary timestamp text | config `timestamp_and_time_expansion_match_c` |
| History counters | history_added, history_collected and history_generation are populated | Those fields are absent; history rows/limits/resets match | core `history_reference` |
| Output timestamp | pane_last_output_time records real PTY output | Field is absent | CLI `process_exit_signal_pipe_and_output_format_lifetimes_match_c` |
| Retained-pane pipe lifecycle | Process completion closes pipe-pane and clears pipe PID | Pipe remains through completion and respawn until explicitly closed | CLI HEAD process test; shared supported test explicitly closes the pipe before process exit |

CI uses the full current oracle suites and only the exact shared suites against
C3.7c. These observations are distinct from deliberate parser differences in
[differences.toml](differences.toml). New baseline imports must recheck both lists.

The checked command registry and current command descriptions/usage are derived
from HEAD and tested by `commands-c-reference`; older releases may have different
command text and flags. Source preparation/error grouping has a separate
`source-c-reference` suite. Contextual format depth/output have deliberate limits
recorded in `resource-bounds.toml` and exact C/R difference tests.

Linux and Windows execution are explicitly user-deferred. The current execution
matrix runs on macOS only and does not certify either deferred platform; their
source responsibilities remain required when qualification resumes.

Current HEAD supports `new-pane -O` and `pane_modal_flag`; installed3.7c has
`new-pane` without `-O`, and older floating layouts use duplicated leaves with
legacy `<...>` suffixes instead of current JSON-v2. Neither C baseline exports
modal `-K/-C/-D` policy as format fields, so stock-server clients cannot infer
those flags; native snapshots retain them.

`graphics-c-reference` requires `image_support=1` and fails otherwise. CI builds
C with `--enable-images`; local image runs used `/tmp/tmuxx-c-images/tmux` built
from the same HEAD. The ordinary local `./tmux` binary currently has images
disabled and does not establish graphics evidence. Image-number/nonzero-layer
extensions are recorded separately from overlapping C behavior.
