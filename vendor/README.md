# Vendored dependencies

## Winit 0.30.13

`winit/` contains the published crates.io source package for Winit 0.30.13,
including its normalized Cargo manifest, original manifest, source metadata,
examples, tests, and Apache-2.0 license. The workspace uses this copy through
`[patch.crates-io]` so macOS quick-terminal window support can be maintained
without modifying a developer's Cargo cache or depending on an external fork.

- Upstream: https://github.com/rust-windowing/winit
- Source: https://static.crates.io/crates/winit/winit-0.30.13.crate
- Upstream revision: `e9809ef54b18499bb4f2cac945719ecc2a61061b`
- Archive SHA-256: `a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`
- Published contents: 217 files, 2,680,680 bytes
- License: [Apache-2.0](winit/LICENSE)

The initial vendoring commit preserves every published file byte for byte.
Cargo's local `.cargo-ok` marker is not part of the source package. Keep native
behavior changes separate from that baseline to make the local patch reviewable.

Local patch: macOS `WindowAttributesExtMacOS::with_nonactivating_panel` constructs
a real `NSPanel` while retaining Winit's existing view/delegate ownership. Its
nonactivating style survives Winit's later style updates, and focusing the panel
does not activate the application. Ordinary windows keep their existing behavior.
The modified source files and manifests carry Rustty modification notices.

Native regression check (main thread, hidden windows; macOS desktop required):
`cargo test -p rustty-app --test native_panel --offline -- --ignored`.
