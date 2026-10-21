# Windows native evidence

Windows execution is active. The authorized local host is Windows Server 2025
x64/MSVC, not Windows 11. Record actual OS/build, compiler, Python and ConPTY
identity in every collection. This document maps executable coverage; a gate
pass requires fresh results for every required shared and native assertion.

| Existing obligation | Executable suites | Limits of the evidence |
| --- | --- | --- |
| `windows.named-pipe-acl-desktop-ssh` | `windows-runtime`, `windows-cli`, `windows-desktop-ssh` | Requires actual distinct-account denial, local-only pipe denial, same-account desktop/SSH logons and live shared pane/graph identity. |
| `windows.bridge-no-duplicate-server` | `windows-cli`, `windows-rustty-tmux-driver`, `rustty-native-ssh-command-config`, `rustty-native-adapter`, `windows-desktop-ssh`, `windows-desktop` | Missing/replaced instances, cmd/PowerShell exec forms, the trusted Rustty driver command, bridge termination, shared/duplicate links, persistent desktop startup and standalone GUI restart must all pass. |
| `windows.conpty-runtime-packaging` | `windows-runtime`, `windows-cli`, `windows-desktop`, `windows-conpty-synchronized-output` | Both actual shipped bundles, pinned hashes, resource negatives, spawn-time ownership and the custom ConPTY harness must pass. |
| `windows.input-resize-descendant-lifetime` | `windows-runtime`, `windows-cli`, `rustty-native-adapter`, `rustty-native-workspace`, `windows-desktop`, `windows-desktop-input`, `windows-desktop-ime`, `windows-desktop-clipboard` | Actual HWND keyboard/mouse/resize/paste, real Japanese TSF preedit/commit, exactly-once native OSC52 effects/replies, app/standalone descendants and GUI restart must all pass. |

Exact names are in `windows-evidence.toml`, `windows-format-evidence.toml`,
`windows-path-evidence.toml`, `windows-admission-evidence.toml` and
`rustty-evidence.toml`. The obligation proofs are explicit ALL-of mappings in
`manifest.toml`; any omitted, failed or stale member keeps them blocked. An IME
capability probe, synthetic Unicode key event or backend-only encoder cannot
qualify real composition. This server host has the Japanese TSF engine and
dictionaries already present, with its user profile initially disabled. The
explicit desktop fixture temporarily enables only that profile and restores it.
Unicode graph/path bytes are tested separately from installed
font coverage; the native view fixture verifies CJK tab glyph support and retains
rendered screenshots. Missing glyphs are not a full rendering pass.

The runtime ConPTY startup regressions call the production `openpty` and
`spawn_command` directly without creating a runtime pane or reaper. The pane's
first test phase verifies its innermost kill-on-close Job contains only its own
PID, then creates foreground and background descendants. Abrupt owner death
must signal held pane, descendant and OpenConsole handles while the outer
fixture Job remains open and an unrelated process stays alive. A separate test
repeats complete invalid-PE `CreateProcessW` startup/close cycles, checks stable
post-cleanup handle counts and held host exits, and successfully opens another
pane. The owner's preexisting console remains alive until owner exit. Both the
ConPTY and stdio launchers suppress blocking Windows startup dialogs only on the
spawning thread, capture the native error before restoring that thread's policy,
and leave process-wide policy unchanged. Failure and success regressions verify
the caller's error modes. These bounded private tests remain exact names under
`windows.atomic-spawn-job-ownership`.

## Collection

Build both bundles after committing finished sources. The runner fingerprints
Rustty and tmuxx in this workspace. Set `TMUXX_C_SOURCE` to the external pinned C
checkout for source comparisons and upstream fixtures. The standalone candidate
is `target/debug/tmuxx/tmuxx.exe`; Rustty is
`target/debug/Rustty/rustty.exe`.

Ordinary hosted CI builds and runs the named backend/format/resource subsets.
The runner passes its actual native Python interpreter to the fixture panes;
direct Cargo checks should set `TMUXX_REMOTE_PYTHON` to `sys.executable` too.
Install `python -m pip install -r tmuxx/conformance/requirements-native.txt`
into that interpreter: native peers use MessagePack and LZ4 for protocol v2.
The identity peer retains JSON only on its private Rust-parent test channel.
Endpoint and special-file vectors require Windows symlink privilege or Developer
Mode. CI checks that capability with a private link before testing; failure is a
missing prerequisite rather than permission to omit those vectors.
The desktop runner additionally requires an interactive desktop, working
same-account Windows OpenSSH authentication and an actual distinct-account
process. Configure these text/path variables with private test credentials:

```powershell
$env:TMUXX_WINDOWS_SSH_HOST = 'account@127.0.0.1'
$env:TMUXX_WINDOWS_SSH_KEY = 'C:/private-fixture/identity'
$env:TMUXX_WINDOWS_SSH_KNOWN_HOSTS = 'C:/private-fixture/known_hosts'
$env:TMUXX_WINDOWS_SSH_VIEWER_KEY = 'C:/private-viewer-fixture/identity'
$env:TMUXX_WINDOWS_SSH_VIEWER_BRIDGE = 'C:/private-viewer-fixture/tmuxx.exe'
$env:TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_KEY = 'C:/private-rustty-viewer-fixture/identity'
$env:TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_BRIDGE = 'C:/private-rustty-viewer-fixture/Rustty 雪 &^ % !; (bridge)/rustty.exe'
$env:TMUXX_WINDOWS_SSH_DENIAL_DIRECTORY = 'C:/private-fixture/denial'
```

Run `python tmuxx/conformance/windows_ssh.py --deny-helper DIR` as the distinct
account immediately before the desktop account collects `windows-desktop-ssh`.
The helper waits at most 180 seconds and answers one fresh endpoint/incarnation/
nonce challenge. The controller checks its exact script identity and a real
different account SID. A saved denial JSON cannot replace this live fixture.
Select the SSH suite first, before long ordinary suite runs.

The viewer credential must be a separate loopback-only key forced to invoke the
exact `TMUXX_WINDOWS_SSH_VIEWER_BRIDGE --native-stdio` executable. The fixture
copies and verifies the candidate there, then launches another real Rustty with
a private `RUSTTY_TMUXX_SSH_CONFIG` file; account-wide SSH configuration and PATH
remain unchanged. Native SSH and per-window resize clients connect through real
OpenSSH, retain the original pane PIDs, and render external shared/duplicate tabs
and splits. Graceful and abrupt viewer exits must leave the desktop owner alive.
The abrupt case checks local SSH child handles before closing the fixture job,
so fixture cleanup cannot substitute for product process ownership.

The trusted-command case uses a separate restricted credential and the actual
Rustty executable at `TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_BRIDGE`. Its fixed dispatcher
must compare `SSH_ORIGINAL_COMMAND` with the configured PowerShell exec string
before executing that command with binary stdio inherited. It must reject a
different command. Per-launch receipts bind the accepted command to the actual
child process; a forced bridge that ignores the requested SSH command does not
qualify this case. The GUI passes `--tmuxx-ssh-command` separately from
`--tmuxx-instance`; native views and resize connections retain the same owner and
pane identities. Graceful and abrupt exits check local SSH helpers and remote
driver processes before fixture cleanup.

`windows-rustty-tmux-driver` runs the packaged application's CLI dispatch and
retained-process tests. `windows-desktop` separately exercises actual
`--tmuxx=persistent` HWND views, GUI restart and explicit missing-instance
attachment. Driver-only tests do not establish desktop rendering. Runtime and
Rustty adapter assertions also retain AgentStatus while views are absent,
install snapshots atomically, and prevent replay of host effects on reattachment.

The native input and host-clipboard fixtures require an empty clipboard before mutation. They use
only known fixture text and check clipboard sequence/owner before cleanup.
If other clipboard data exists or appears during the test, preserve it and
report the unmet prerequisite or failure. Never overwrite newer user content.

`windows-desktop-clipboard` additionally exercises application-generated OSC52
through two actual Rustty GUIs. Opt-in native guards check the elected foreground
HWND, process, expected sequence and emptiness under `OpenClipboard` before any
write. Reads require the recorded successful write's exact sequence and owner.
The fixture checks one OS write, one OS read and one reply to the original pane;
reconnecting a native viewer must replay none of them. Cleanup reads only its
known value while sequence and owner still match, and preserves any newer data,
including changes from the same HWND. Normal application clipboard behavior is
unchanged when the fixture environment variable is absent.

`windows-desktop-ime` requires the reviewed disabled Japanese profile baseline
and explicit `TMUXX_WINDOWS_IME_PROFILE=temporary-japanese`. It records and
flushes the profile registry value and ancestor-key existence before changing
anything, then requires a live independent restoration watchdog with a retained
owner-process handle. It activates the existing profile only on the owned GUI
thread, sends real keyboard input to that exact foreground HWND, observes Winit
preedit without application input, and verifies a non-ASCII commit reaches the
original pane exactly once. It never installs language components, changes the
language list, or accesses the clipboard. The profile, pointer position and all
test-owned processes are restored or checked at completion.

The IME suite job alone permits explicit `CREATE_BREAKAWAY_FROM_JOB` for the
bounded restoration watchdog; ordinary suite and product jobs do not. An outer
collection job must permit that explicit breakaway too, while retaining
kill-on-close ownership for the collector. The watchdog verifies that it escaped
all jobs before any profile change and restores after parent exit or its lease
deadline. `python -m unittest test_windows_native_ime -v` from this directory
includes native regressions proving missing breakaway aborts before mutation
and abrupt fixture death restores the exact baseline and reaps the watchdog.

External fixture adapters verify one exact JSON completion record, every named
test, report-file equality, zero remaining owned processes, unchanged executable
hashes and unchanged Rustty sources. Native fixture JSON and PNG paths are kept
in the parent report. The custom `windows_conpty` harness uses its own exact
structured result, not libtest-shaped text or a zero-test Cargo exit.

The Windows report also needs applicable shared command/protocol/terminal and
supported C-server evidence. Native collection without a C reference is valid
for independent suites; a complete profile cannot invent a local Windows C
tmux or reuse historical macOS evidence. Missing C servers and unported
reference fixtures stay blocking. Do not remove obligations to make a subset
green.

The [remote C oracle adapter](REMOTE-C.md) executes complete native Windows
counterparts for format/parser, Unicode cell/cursor/snapshot, capture/raw-grid,
graphics reply/pixel/quota, SIXEL, OSC133 shell-marker, history, clipboard,
terminal-option and key/mouse/focus/paste-byte matrices against a pinned
genuine C server. Its SSH reference host supplies
only C observations. Platform-specific suite selection preserves all original
macOS evidence requirements and leaves every unported assertion pending.

```powershell
python tmuxx/conformance/runner.py gate --report tmuxx/conformance/artifacts/windows.json --profile windows
```

Retain failed reports and exact commands. Remove temporary public-key lines and
private credentials after collection, using line-specific removal that preserves
all other authorized keys. Kill-on-close job regressions cover success, failure,
timeout, interruption, failed assignment and abrupt collector death; final
cleanup also checks each fixture's recorded process handles/PIDs.

### Shared native frame and lifecycle obligations

`windows-hidden-native-frame` runs the packaged Windows GUI hidden and unfocused,
with two panes and a floating pane clipped at a signed negative origin. It records
the actual renderer, OS, graph, draw order and PNG identity. The historical
`rustty.hidden-native-metal-frame` assertion ID is retained; Windows evidence
identifies its software renderer explicitly and does not claim Metal execution.

`windows-cli-process-lifetimes` executes the four shared lifetime obligations
using real packaged standalone owners and separately killable production stdio
bridges. Windows Job Objects provide the owner boundary. The owner-death fixture
suspends the exact owner to prevent ordinary reaper cleanup from masking abrupt
death, and checks foreground/background descendants with the leader alive and
already exited. A separately owned server and unrelated process survive. The
retention case kills the last bridge, checks a byte-exact detached terminal reply
without any live viewer, then verifies retained PID, snapshot and shared links
through a fresh bridge. Every death assertion precedes outer fixture cleanup.

The nine `runtime-native-views` tests execute natively on Windows. Their raw
applications use the Windows console API. Cell geometry reads ConPTY rows and
columns through `GetConsoleScreenBufferInfo`; CSI14t/CSI16t replies verify real
terminal pixel and cell dimensions, including new panes and both respawn paths.
ConPTY does not expose the Unix `TIOCGWINSZ` pixel fields.

`runtime-host`, `runtime-host-lifetimes`, `runtime-transactions` and the ten
ordinary `runtime-scheduler` tests execute on Windows with real named pipes,
ConPTY, process handles and Job Objects. Scheduler fixtures explicitly invoke
PowerShell through the production system `cmd.exe` shell. The complete nine
hook, nine client-command and ten ignored C scheduler matrices use the pinned
remote C adapter. They retain all original vectors and exact test names; the
native process side uses Windows mechanisms, including independently checked
account profiles, stdin pipe EOF and abrupt termination status. The targeted
command-only job regression also checks that a literal pane identifier survives
the later time-formatting stage.

The ownership-reference suite retains both original C ignored-SIGHUP descendant
categories (`setpgid` and `setsid`) against the real pinned server. Native
`CREATE_NEW_PROCESS_GROUP` and `DETACHED_PROCESS` counterparts verify their
actual console attachment and remain alive after their pane leader exits. Both
terminate with the Windows owner Job, while a separately owned process remains
alive. Exact native process handles and remote pidfds identify descendants; the
reference lease reaps the C survivors after each case.

`windows-control-pipe-eof` retains the shared silent-input obligation with an
actual `-C` process. EOF and detach with a still-open writer release the client
within five seconds and preserve its duplicated pipe's blocking mode and handle
flags. Queued immediate output and completed guards precede exactly one `%exit`;
late waited-job output stays discarded and the original pane remains alive. The
Unix suite keeps its platform-specific FIFO polling and `fcntl` flag checks.

The complete runtime identity matrix uses separate native pipe client processes
and real retained C control clients. Windows peer PID, account SID and account
name come from the pipe and its verified impersonation token; a supplied USER
value cannot replace them. Original clipboard/RGB feature bits, absent tty
formats, graph/list selection and ordering, grouped-session attachments,
previous sessions and rendering counters remain executable on Windows.

The pane-move, native UI policy and stream-consistency suites use actual ConPTY
processes and native named pipes on Windows. The endpoint suite inspects the
kernel object's owner and protected single-account DACL, checks atomic owner
replacement and retains the original filesystem-redirection preservation case.
The special-file suite rejects real named pipes, links to pipes and console
devices for load/save/source, preserves binary regular-file links, and checks
NUL after absolute path resolution. It also rejects reserved-device aliases;
source-file enumeration does not open a link's pipe target before admission.

Windows time conversion retains checked Gregorian years beyond UCRT's year 3000
epoch limit through an equivalent 400-year calendar. Literal templates and
numeric year fields retain the full year; epochs outside the signed tm_year
range still fail explicitly. This includes the original upstream year 100000
literal-template regression and the native full-year numeric regression.

`windows-cli-state` retains the seven shared CLI state obligations while recording
the native Windows selection and startup contract. A private caller default uses
`TMUXX_ENDPOINT`; explicit labels and endpoints select independent owners and
reject conflicting selectors. Actual command/control checks wait-exit and
capability negotiation. Eight explicit owner starts elect one replacement, then
eight command clients create one pane each. Missing-owner command requests never
start a replacement owner. The only simulated peer advertises an old capability
set on a private same-account pipe and verifies that no command is submitted.

The `windows-cli-config-lifetime-c-reference`, `windows-cli-runtime-defaults-c-reference`,
`windows-cli-source-stdin-c-reference` and `windows-cli-selectors-c-reference` suites preserve the complete configuration
startup, runtime defaults, source/stdin and malformed selector matrices. Their
nine named tests use 19 separately identified and cleaned-up real C fixtures.
Windows starts its standalone owners explicitly; the C configuration fixture
retains the original autostart-client mechanism. Both reject 140 malformed
connections during held configuration startup and execute the configuration
once after the initiating and replacement command clients are killed.

Source/stdin coverage includes all thirteen file, flag, group and repeated-input
cases, plus a client held before EOF. Negotiated native file streaming prepares
the prefix once, permits another client to change the environment, then resumes
the source and suffix without replay. Only explicitly listed platform paths
differ in the expected observations. The
selector matrix sends malformed UTF-8 through the actual binary stdio bridge;
Windows Unicode argv cannot represent these bytes. Genuine U+FFFD names remain
distinct, rejected mutations preserve the graph, and control clients remain
usable. Runtime defaults separately assert the native shell and trusted OS
fallback PATH before comparing shared child, attachment and editor behavior.

`windows-cli-discovery-c-reference` combines actual native pipe/bridge discovery
with independently executed C socket selection. The Windows half identifies two
different owner incarnations and confirms that fixed stdio bridges and local
pipes see the same running pane for labels containing Unicode and shell
metacharacters. The C half uses the real executable's default namespace,
inherited `TMUX`, `-L` and `-S` precedence across four private owners, including
an explicit path containing spaces, Unicode and a comma. No wrapper forces a
common socket. The original local Unix C/R client pairing remains executable
and required on Unix.

The `windows-upstream-*` suites retain all 28 named upstream tests, including the
two existing scope/raw-storage difference tests, against the pinned real C
oracle. C executes every original shell script unchanged. Native Windows uses
the MSVC candidate, explicit named-pipe owners and the installed Git for Windows
test shell. Private labels replace Unix fixture sockets, and temporary files use
native absolute paths. Control stdin relays terminate with their actual CLI;
they cannot retain a shell after wait-exit. Git's byte locale is used for its
literal UTF-8 grep patterns because that grep rejects non-BMP ERE literals in
its UTF-8 locale. The modifier test's idle native sleep processes preserve its
original equal-title premise. Command output and error text are never rewritten.

Four native counterparts replace Unix-specific application or pipe fixtures:

- Malformed input retains all nine exact byte vectors through empty-pane
  `display-message -I`, before ConPTY can parse and reserialize the stream.
  A separate real ConPTY application reads exactly one negative DECRQSS reply.
- Buffers retains every ordering, limit, format, filter, error and binary-file
  vector. Five real raw-console applications check translated/raw/separator and
  delete-after paste. A native stdout pipe consumes exactly eight bytes before
  closing during a 100000-byte save; the CLI must report the failed write.
  This uses supported `save-buffer -`; named-pipe pathnames retain their explicit
  native special-file rejection.
- Slow and unread control-output fixtures use native anonymous pipes. An
  infinite real ConPTY producer writes 512 bytes every 20 ms, ten times the
  nominal slow-reader rate. A bounded capture first observes a complete terminal
  row before attachment. The slow reader drains at about 2.5 KiB/s until 8000
  bytes, then detaches the client and renames five times. Its fast drain retains
  the original three-second and 512 KiB limits, and must reach actual EOF with
  exactly one bare `%exit` as the final newline-terminated record, exit code zero
  and stdin still held open. Forbidden post-detach notifications must be absent
  from that complete transcript. The unread fixture performs no output read and
  requires the exact owner process to exit after kill-server. Both verify the
  original client name/PID is still attached, the producer has filled a terminal
  row and the pipe has unread bytes immediately before the tested action. The
  exact identity check prevents independent eviction from replacing that action.

The native harness separately exercises unlimited real `cat`/`tr` flooding on
script success, early failure and timeout. Each explicit owner joins a fixture
Job before its first pane command. Cleanup kills the product owner and requires
its full Job process list to become empty before closing the fallback fixture
Job, while an unrelated process remains alive. All reference leases also report
empty process inventories. Shutdown serializes with in-flight fixture startup
and rejects later requests, so timeout cannot start an owner after cleanup.
These tests supplement, and do not replace, the
packaged GUI, cross-logon, SSH and Windows 11 qualification obligations.
