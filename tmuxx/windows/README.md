# Native Windows owners and bridges

Build on x64 Windows with the MSVC Rust 1.95.0 toolchain, Windows SDK and Python
3.11 or newer. This work executes on Windows Server 2025 x64; Windows 11 remains a
distinct target until tested there. From the repository root:

```powershell
./tmuxx/windows/build.ps1
# Reuse a downloaded, hash-verified archive without network access:
./tmuxx/windows/build.ps1 -Offline -ConPtyPackage C:/cache/microsoft.windows.console.conpty.1.24.260710001.nupkg
```

The bundle is `target/debug/tmuxx` (`release/tmuxx` with `-Release`). Keep
`resources` beside `tmuxx.exe`. The script uses static CRT, stages a complete
bundle and renames it into place. Never copy a new DLL over a running owner's
resources. A running owner keeps verification handles open to prevent resource
replacement. Close an old owner before removing its old bundle.

`conpty.json` pins the NuGet archive and the size/SHA-256 of `conpty.dll` and
`OpenConsole.exe`. Both embedded Rustty and standalone tmuxx call the same
initializer before portable-pty selects its process-wide API. Missing, modified,
incompatible or differently initialized resources cause explicit startup errors.
There is no in-box ConPTY fallback. Tests may set `TMUXX_CONPTY_RESOURCES` to a
private verified bundle; packaged applications use their sibling resources.

Start a standalone owner explicitly, then attach command clients:

```powershell
./target/debug/tmuxx/tmuxx.exe -D -L work
# From another terminal:
./target/debug/tmuxx/tmuxx.exe -L work new-session -d -s example
./target/debug/tmuxx/tmuxx.exe -L work list-panes -a -F '#{pane_pid}'
```

Command clients and `--native-stdio` only attach to existing owners. They never
launch an owner or ConPTY as a connection fallback. `-L` accepts 1–64 UTF-8 bytes,
including spaces and Unicode, without NUL. The native bridge accepts no command
or instance arguments: its first Hello carries `instance` and optionally the
expected `instance_id`. Omitted selection requires exactly one discoverable
owner. Mismatched versions/incarnations fail before requests are submitted.

Windows OpenSSH runs the fixed executable with `--native-stdio` through an exec
channel without a PTY (`ssh -T`). The executable path must be quoted for the
server's configured cmd.exe or PowerShell shell. Instance selection never
appears in shell source. Keep stdin/stdout as byte streams; diagnostics use
stderr. Native protocol v2 is binary MessagePack with per-block LZ4 compression;
update the GUI, bridge, and owner together and restart the owner. Direct network
named-pipe clients are rejected; SSH reaches the owner's local pipe.

With cmd.exe as the OpenSSH shell, use a directly quoted executable:

```ini
tmuxx-ssh-command-host = windows = "C:/Apps/Rustty/tmuxx.exe" --native-stdio
```

With PowerShell, start the child with inherited standard handles:

```ini
tmuxx-ssh-command-host = windows = $p = New-Object System.Diagnostics.ProcessStartInfo; $p.FileName = 'C:/Apps/Rustty/tmuxx.exe'; $p.Arguments = '--native-stdio'; $p.UseShellExecute = $false; $p.CreateNoWindow = $true; $c = [System.Diagnostics.Process]::Start($p); $c.WaitForExit(); exit $c.ExitCode
```

For the Rustty executable, set `FileName` to its path and `Arguments` to
`tmux --native-stdio`. Remove `| ForEach-Object { $_ }` and other PowerShell text
pipelines from old configuration: they corrupt binary protocol bytes.

Pipe names contain the Windows account SID and an injective instance encoding.
The protected DACL and peer-token checks enforce account access across actual
desktop and SSH logon sessions. Namespace discovery and server incarnation are
metadata, not credentials. A live owner cannot be replaced by binding its name.

App-owned panes end with their Rustty process. Standalone panes remain under
their original owner through GUI restart. Per-pane and per-job kill-on-close Job
Objects are assigned atomically during `CreateProcessW`, including the interval
before pane registration. Background descendants remain owned after the initial
process exits. Windows process-name/current-path formats describe that retained
initial process; ConPTY has no POSIX foreground process group API.

## Native conformance prerequisites

Ordinary CI executes MSVC builds, exact named tests, resource negatives and
cleanup regressions. Desktop qualification additionally needs an interactive
desktop, packaged Rustty and tmuxx, working same-account OpenSSH authentication
into a different logon session, and an actual unauthorized-account probe.
Use private temporary credentials; remove only the installed test key afterward.
The native fixtures write exact results, binary identities and cleanup results.
Install their MessagePack/LZ4 dependencies into the Python interpreter used by
`TMUXX_REMOTE_PYTHON`, including for direct Cargo identity tests:

```powershell
python -m pip install -r tmuxx/conformance/requirements-native.txt
```

Portable frame tests and cross-compilation do not qualify Windows named pipes,
PowerShell/OpenSSH byte preservation, or actual desktop behavior; rerun those
fixtures on the Windows host after the protocol upgrade.

The complete Windows profile retains shared C-reference obligations. A native
collection does not need a fictitious local C executable, but cannot count a
missing supported-C interoperability test as passed. See
[`../conformance/README.md`](../conformance/README.md). Commit finished sources
before collecting final evidence, then run:

```powershell
python tmuxx/conformance/runner.py gate --report tmuxx/conformance/artifacts/windows.json --profile windows
```
