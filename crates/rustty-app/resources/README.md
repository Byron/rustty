# Rustty app resources

On Windows, `./crates/rustty-app/build.ps1 -Release` creates
`target/release/Rustty` with `rustty.exe` and an adjacent `resources` directory.
Omit `-Release` for debug; use `-Offline` for cached dependencies. Integration scripts
are normalized to UTF-8/LF even in an `autocrlf` checkout. The Windows executable
embeds its icon, version, and per-monitor DPI manifest; the bundle does not require
`tic`, Zig, or a separate DXC DLL. Without a compiled terminfo directory, sessions
advertise `xterm-256color`. Optional per-user registration is documented in the app
README and is never performed by an ordinary build or launch.

Build from the repository root with `nu crates/rustty-app/build.nu --release`.
Omit `--release` for a debug build; add `--offline` to use cached Cargo dependencies.
The script creates `target/release/Rustty.app` (or `target/debug/Rustty.app`)
and signs it ad hoc for local use. It does not install the app.

Application data lives in `Contents/Resources/rustty`: themes, shell integration,
and the compiled terminfo database. The app passes this directory to sessions;
configuration theme discovery uses the same directory. The executable embeds
the production JetBrains Mono and Nerd Fonts Symbols fonts through `rustty-font`.
Their license notices are copied into `Contents/Resources/licenses`.

Resource provenance:

- `themes/` is the unmodified Ghostty-format export of iTerm2-Color-Schemes
  release `release-20260831-151010-752a9c0`, as pinned in `build.zig.zon`.
  Source archive: <https://deps.files.ghostty.org/ghostty-themes-release-20260831-151010-752a9c0.tgz>.
  Verified Zig package hash: `N-V-__8AAEFmBABuDGOKxAI6VMg41b9euMZ-z7HS9EcUdaor`.
  Its MIT license is retained in `iTerm2-Color-Schemes-LICENSE.txt`, fetched from
  <https://github.com/mbadolato/iTerm2-Color-Schemes/blob/752a9c0/LICENSE>.
- `ghostty.terminfo` is a source snapshot of `src/terminfo/ghostty.zig`, last
  changed in `69b9abf09ebad2b11a6850a28271676f6bfeb108`, encoded as documented by
  `src/terminfo/Source.zig`. It retains the `xterm-ghostty` name for protocol
  compatibility. The build uses the system `tic` and does not require Zig.
- Shell integration is copied directly from `src/shell-integration`, including
  zsh's `.zshenv`. Existing `GHOSTTY_*` environment variable names remain part
  of the integration protocol. `TERM_PROGRAM=ghostty` enables compatible CLI
  behavior such as Cargo's OSC progress reports; the application remains Rustty.
- The app icon reuses `macos/Assets.xcassets/AppIconImage.imageset/macOS-AppIcon-1024px.png`.
  Ghostty's MIT notice is included in every app bundle.

The Rust/WGPU executable has no link dependency on the Zig library. Native
macOS frameworks or Windows APIs provide font services, menus and notifications.
