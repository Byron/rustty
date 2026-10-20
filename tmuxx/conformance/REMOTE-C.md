# Native Windows tests with a remote C oracle

The Rust format/parser and terminal implementations execute as native MSVC test
binaries. A separately selected C tmux executable supplies only reference bytes
through authenticated, binary SSH exec without a PTY. A Linux reference host,
including an explicitly selected WSL distribution, does not qualify Windows
ConPTY, process ownership, desktop rendering, security or input behavior.

The adapter exposes the complete existing styled width/trim, byte parser/argv,
POSIX substitution and Unicode cell/cursor/snapshot matrices. Native Windows
counterparts also retain the original capture/raw-grid, graphics reply/pixel/
quota, SIXEL, OSC133 shell-marker, history, clipboard, terminal-option and
key/mouse/focus/paste-byte matrices. See `remote-c-evidence.toml`,
`remote-terminal-evidence.toml` and `remote-core-terminal-evidence.toml` for
exact named tests and fixture counts. These comparisons do not substitute for native
Windows process, desktop or system-client evidence.

## Explicit reference selection

Create a private JSON configuration with these fields. Obtain the executable's
exact version and SHA-256 from the chosen, source-identified C build. Never use
an unverified executable or a placeholder local Windows C binary.

```json
{
  "host": "oracle-account@reference-host",
  "port": 22,
  "key": "C:/private-fixture/identity",
  "known_hosts": "C:/private-fixture/known_hosts",
  "ssh": "C:/Windows/System32/OpenSSH/ssh.exe",
  "binary": "/private/reference/current/tmux",
  "version": "tmux next-3.9",
  "sha256": "<exact lowercase SHA-256 of that C executable>"
}
```

The key and trusted host-key file must already exist. The helper uses strict
host-key checking, only the selected identity, batch authentication and no PTY.
The SSH shell receives a fixed Python bootstrap. Binary selection, versions,
arguments and stdin follow in length-prefixed binary messages; shell
metacharacters and invalid UTF-8 argument bytes never enter shell quoting.

The reference host needs Python 3.11+, a current C tmux with foreground `-D`
server mode, `-N` client mode, empty panes and raw capture, a UTF-8 C locale, and
Linux `/proc` plus `PR_SET_CHILD_SUBREAPER`. Graphics matrices additionally need
image support, zlib and libpng in the actual C build; `image_support=1` alone
does not establish PNG or compressed-image support. The subreaper requirement applies to
this test fixture's cleanup mechanism, not to the supported C-server product
protocol. No remote service or new SSH daemon is installed by the adapter.

The literal-high-byte parser difference also depends on the C target's plain
`char` signedness. Local Unix tests use that target ABI. The current Windows
remote fixture requires the recorded Linux x86_64 reference built with default
C character flags and validates that host identity before asserting signed-char
behavior. A different or unidentified reference ABI fails this prerequisite;
unsigned-char C behavior must not be normalized into the signed-char result.

```powershell
$env:TMUXX_REMOTE_C_CONFIG = 'C:/private-fixture/current-reference.json'
python tmuxx/conformance/remote_c_reference.py --config $env:TMUXX_REMOTE_C_CONFIG --probe

python tmuxx/conformance/runner.py smoke --no-smoke `
  --candidate target/debug/tmuxx/tmuxx.exe --candidate-kind rust `
  --suite windows-remote-c-formats --suite windows-remote-c-unicode `
  --report tmuxx/conformance/artifacts/windows-remote-c.json
```

The probe verifies version and hash without starting a server. Each actual test
fixture owns a C server, a mode-0700 temporary directory and a unique
private socket. Each mutation uses `-N`; a dead incarnation is an error and no
mutation is retried. Input, output, argument count and response lengths are
bounded. Commands have a ten-second deadline and fixtures have a 180-second
lease. EOF, malformed input, timeout and interruption enter the same cleanup.

Capture and reply fixtures upload files only under their private reference
directory. Pixel, quota, shell-marker and native-input observations upload the
original approved Python helper and its exact dependencies. Both ends verify
the reviewed canonical source hashes before executing an observer under a
separate child subreaper and a 120-second deadline. The 64-MiB oracle frame cap
accommodates complete shell snapshot JSON; it does not alter any product
protocol bound. Every observer returns its source hashes and complete cleanup.
The native MSVC test performs every Rust-side operation and comparison.

`remote-runtime-control-evidence.toml` retains all nine original hook tests,
nine client-command tests and ten scheduler tests. Each native client submits
prepared control source through the actual owner queue. Retained C `-C` clients
have real, observed process identities; native trusted embedded contexts use
those identities for a bijective client-name comparison. Guard ordering,
command output and every non-output notification remain exact. The native API's
separate batch completions and snapshots are checked independently. All eight
detach/displacement cases check native session/hangup payloads and actual C
`%exit` plus process exit; the original Unix test still inspects imsg envelopes.

Scheduler files and passive pane applications are private on both hosts. Only
explicit fixture programs are translated. File diagnostics map exact owned
file paths, including Windows canonical paths, rather than replacing arbitrary
separators. Native account/profile and C account/home expansion each have full
independent expected strings. Job shell identity, working directory and
environment are observed in real children. Windows stdin is a pipe with EOF
and abrupt child termination yields status -1; C stdin is a socket with EOF
and SIGTERM yields status143. Both exact diagnostics, outer continuation and
subsequent status reset are required. The installed alternate C fixture shell
may be Bash where zsh is unavailable; the job must still run through `/bin/sh`.

Persistent commands have bounded output, separate stderr, process handles and
explicit EOF. Ordinary command completion never injects graph/list commands,
because even a list command can change hook firing counts. Opening a control
client uses an explicit `list-clients` setup observation to obtain its actual
C client name by PID; fixtures must account for that setup before asserting
hook counts. Cleanup uses subreaper-owned processes and verifies no fixture
descendants remain.

`RemoteReference` and each persistent client share the same leased fixture.
Dropping an observation handle cannot close a fixture still used by another
client. Startup, per-command and persistent-control environments preserve byte values
and explicit removals. Persistent clients accept only the original feature flags
(`-2`, `-u`, `-q`, `-C`, and attached/separate `-T`) before the command;
feature order, unknown names, colon/comma separators and removals are passed
unchanged to C. Endpoint and configuration overrides are rejected. Ordinary and retained clients use the private files directory as
their working directory. `finish_input` closes only client stdin, allowing
held `source-file -` tests to observe the parser's actual EOF barrier.

The initial-configuration fixture runs the original C implicit-start command,
`-f private.conf start-server ; display -p starter`. Its subreaper adopts the
daemon, verifies the actual executable and server identity, and retains a
pidfd. It does not replace startup loading with a later `source-file` command.
The native test kills the actual launching C client during a configuration
barrier, drops another waiter, executes all 140 original malformed IPC clients,
then proves once-only loading and exact completion for a surviving waiter.
The malformed frames remain native-endian C imsg on the reference host; native
Windows uses its actual named-pipe protocol for the corresponding rejection
and slot-reuse contract. Both fixture modes require complete descendant cleanup.

The private Linux helper becomes a child subreaper before starting its server.
After stopping that server it adopts and reaps orphaned pane/background
descendants, including those created during a failed or timed-out command.
Signals target only unreaped children whose ownership was confirmed by
`waitpid`, avoiding PID reuse. Every successful fixture must report no remaining
owned children and no cleanup errors. The local helper closes SSH stdin first
and uses bounded reaping; native collection additionally places it in the
conformance runner's kill-on-close Windows Job Object.

The report records hashes of the configuration, helper, Rust support, SSH
executable, selected key and trusted host-key file, plus the exact remote
executable and host identity. Exact fixture identities, distinct private
sockets, observer completions and cleanup are counted per suite: for example,
46 capture fixtures, twelve Unicode fixtures and seven graphical-pixel
observers. The gate rechecks files and performs another server-free remote
identity probe. Missing access, changed identity, missing test names or
incomplete cleanup invalidate the evidence.

Platform-specific equivalent suites share an assertion ID only when they retain
its complete named-test matrix. Native Windows collection selects the Windows
counterpart; macOS retains every original Unix counterpart. Assertions lacking
a Windows counterpart remain required and pending. The Unix local oracle test
bodies and their required reference identities remain unchanged.

Local transport regressions can run without a host:

```powershell
python -m unittest discover -s tmuxx/conformance -p 'test_remote_c_*.py'
cargo +1.95.0 test --locked --offline --manifest-path Cargo.toml `
  -p tmuxx-config --test remote_c_reference -p tmuxx-terminal --test unicode_reference
```

The latter command executes native framing regressions and explicitly ignores
the C comparisons. Only the conformance command with valid remote access and
`--ignored` in its suite definitions can establish those comparison results.

The `windows-stock-c-ssh-native-views` and
`windows-stock-c-3-7c-ssh-native-views` counterparts also execute the complete
original Rustty system-SSH test on Windows, then launch the packaged native GUI
against a second private C fixture. They require an interactive desktop, the
isolated Rustty checkout, and a separately restricted OpenSSH key. Set
`TMUXX_SYSTEM_C_SSH_CONFIG`, `TMUXX_SYSTEM_C_SSH_HOST` and
`TMUXX_SYSTEM_C_SELECTION`; the SSH config's directory contains its `identity`
and `known_hosts` files, all hashed into evidence. Set
`TMUXX_REMOTE_C_OLDER_CONFIG` for the pinned 3.7c reference.

That key's forced command accepts only the fixed stock control-attachment forms
and the exact buffer-name observation used by the existing Rustty test. It
passes these to `windows_system_c_remote.py` without a PTY or shell expansion.
The selection file points to an existing leased fixture and includes the C
binary hash, socket inode, owner PID and process start identity. The entry
validates private socket ownership and the selected incarnation before using
`-N`; missing or replaced owners never launch another server. Both remote
fixtures, the actual Windows executables, native screenshots and exact named
test results are required. This desktop test is separate from ordinary CI
checks and does not turn its POSIX C panes into Windows process evidence.

The runtime identity adapter retains two native Windows pipe client processes
and two actual C control clients. It verifies each actual PID, OS account and
feature mask before mapping complete identity fields for comparison; private
fixture paths are explicit. No fields, list modes or original absent-context
vectors are removed. The native side uses the authenticated account SID where
the C side has a POSIX UID. One remote fixture and its cleanup completion are
required for the complete named identity matrix.

`windows-stock-c-ssh-client` and `windows-stock-c-3-7c-ssh-client` retain the
complete original `ssh_reference` matrix. Set the three system-C variables
above and `TMUXX_SYSTEM_C_SSH` to the native SSH executable. `ssh -G` resolves
the actual key and known-host file; their hashes, config, executable and host
alias are recorded and rechecked. The oracle only owns the private C fixture.
SystemClient itself sends and receives raw stock control bytes through a
second actual SSH process, which the test terminates during an unobserved
mutation. Reconnect must preserve the live pane PID and apply that mutation
exactly once. Serialize these suites and the native-view suites because the
restricted SSH fixture selects one live server incarnation at a time.
