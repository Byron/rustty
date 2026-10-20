# Native Windows path formats

On Windows, `#{b:value}` and `#{d:value}` split Windows path text into its final
component and parent. The implementation scans bytes and performs no filesystem
access, Unicode decoding, separator rewriting, environment expansion or path
resolution. Malformed UTF-8, spaces, dots and NUL remain exact format data; the
native file/process admission layer still rejects values it cannot use safely.
Unix builds retain their existing POSIX helpers. Values returned by a remote C
server are not rewritten.

Ordinary Windows paths recognize both `\` and `/`. Drive letters are ASCII and
retain their case. A bare drive prefix is retained; a drive-relative parent stays
drive-relative. UNC server/share pairs are indivisible volume prefixes. Root-only
paths return their complete prefix and one existing root separator, if present,
for both modifiers. This prevents a share or drive root from becoming a relative
path when reused in another format.

| Input | `b` | `d` |
|---|---|---|
| empty | `.` | `.` |
| `one\two\` | `two` | `one` |
| `C:` | `C:` | `C:` |
| `C:file` | `file` | `C:` |
| `C:\` | `C:\` | `C:\` |
| `C:/one\two/` | `two` | `C:/one` |
| `\\server\share` | `\\server\share` | `\\server\share` |
| `\\server\share\file` | `file` | `\\server\share\` |
| `\\?\UNC\server\share\file` | `file` | `\\?\UNC\server\share\` |
| `\\.\pipe\name` | `name` | `\\.\pipe\` |

The exact `\\?\` prefix selects verbatim syntax: only `\` is a separator after
that prefix, so `\\?\C:\one/two` has basename `one/two`. Verbatim drive paths,
case-insensitive `UNC` markers, other verbatim volume names and `\\.\` device
prefixes retain their original bytes. No component case folding occurs.

Trailing separators are removed above the volume/root boundary. Repeated
separators immediately before the final component are omitted from the parent.
Other spelling, including internal repeated separators and `.`/`..` components,
is retained. These are lexical helpers, not path validators: incomplete ordinary
UNC forms fall back to rooted component splitting; an incomplete verbatim UNC
form retains the generic `\\?\UNC` prefix. For example, `\\server` splits into
`server` and `\`, while `\\?\UNC\server` splits into `server` and `\\?\UNC\`.

The two native executable tests cover drive, relative, rooted, UNC, verbatim,
device, mixed-separator and trailing-separator cases, plus Unicode and distinct
malformed bytes through the actual `b`/`d` format dispatcher:

```powershell
cargo +1.95.0 test --locked --offline --manifest-path rust/Cargo.toml -p tmuxx-format --lib windows_paths::tests::
```

`rust/conformance/windows-path-evidence.toml` maps their exact names. These results
establish this Windows path-text policy, not Unix execution or C-reference parity.
