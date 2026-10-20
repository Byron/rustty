# tmuxx portable-pty fork

Based on portable-pty 0.9.0 from crates.io. Archive SHA-256:
`b4a596a2b3d2752d94f51fac2d4a96737b8705dddd311a32b9af47211f08671e`.
The upstream MIT license is retained in LICENSE.md.

Windows changes require explicit initialization of a verified bundled ConPTY
runtime, verify the loaded API module path, and atomically assign each pane's
initial process to a kill-on-close Job Object using
PROC_THREAD_ATTRIBUTE_JOB_LIST. The master retains that job after a shell exits,
so foreground and background descendants remain owned until the pane closes.
Unix behavior is unchanged. This fork must be shared by embedded and standalone
owners to avoid multiple process-wide ConPTY selections.
