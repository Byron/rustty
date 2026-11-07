# App, resources, packaging and documentation audit

Exact range: `6ba144158..5acdbf154`; Rustty reference: `c06197283` (same Rust code at rebased `55d67c7`). Initial audit was read-only; completed ports and executed validation are recorded at the end. `app-commit-map.json` assigns 129 incoming commits to the areas below. `all-metadata.json` preserves every incoming hash, parent and changed-path list. Every merge in the full range was checked with `git show --remerge-diff`: **zero nonempty merge-resolution diffs**. Thus each merge has exactly its underlying changes; no additional resolution needs a separate port.

## Original port findings (all four completed below)

1. **Direct commands skip repeated spaces** (`90ed851de`, merge `e2593c149`). `rustty/src/config.rs::Command::parse` still uses `value.split(' ')`, preserving empty arguments between spaces. Upstream now tokenizes a literal space, not arbitrary whitespace. Add `.filter(|arg| !arg.is_empty())` before owned conversion; do not use `split_whitespace`, which changes tab semantics. Trace all uses through `Config::apply` command/initial-command, `Session`/`CommandBuilder`, and tmuxx config consumers. One focused parser test should cover leading/trailing/repeated spaces, embedded tab remaining in one arg, empty direct/shell rejection, and unchanged shell text. This is a behavioral fix, not a perf port.
2. **Session window appearance** (`6ba2c2b27`, `dce5a9464`, `4a59fd20a`, `8b27b66f3`, merge `bbccbb546`). New action `set_window_theme:config|system|light|dark` applies app-wide, survives config reloads, and does not persist to config. Rustty has `Config.window_theme`, `ui_theme`, global and scoped tmuxx configs, command palette/action parser and native hosts, but no session override action. Port through those existing layers. Add four command-palette commands; cover parsing, override surviving reload/system change/theme preview/scoped tmuxx configurations, clearing to latest configured appearance, and current/new windows plus outliner/native chrome. See implementation plan appended below. The GTK conditional-theme recursion itself is not present: Rustty already maps Auto plus differing light/dark themes to System and doesn't use libadwaita callbacks. Preserve that behavior; no `ghostty` config mode needs inventing.
3. **Bundle latest themes** (`845736e1d`, merge `2fc8d02c2`). Upstream archive becomes `ghostty-themes-release-20261026-153024-27cbbb5.tgz`, archive SHA-256 `38bb80d98456677bad9a78a76e83a1e4851e88900aec93af74c6251ca32b78a0`, Zig hash `N-V-__8AAIs9BgCF_swp81fS2tAWIJytW8NCUa6EbSOygPE3`. Rustty `crates/rustty-app/resources/themes` is an independent copy still from Oct 19, so inheritance is insufficient. Compare/extract updated Ghostty-format theme files, retain license, update resource provenance. Both build.nu and build.ps1 already copy that directory; no build change should be needed. Validate hash, file inventory, exact extracted-content equivalence, and all themes through existing config loader checks.
4. **Percent-decoded OSC 7 host matching** (`f7b25c812`, merge `042b16b61`). Rustty `ShellInfo::decode_directory` already admits spaces/apostrophes/underscores/non-ASCII/MAC hosts without an RFC1123 validator; these paths need only focused equivalence coverage. However `tmuxx-core/src/admission.rs::reported_path` compares the raw URI authority to local hostname before percent-decoding. Thus `file://h%C3%B6st/tmp` incorrectly fails for local `höst`. Decode and validate authority before the existing local-host check, retaining rejection of foreign hosts, invalid percent/UTF8/NUL and raw `kitty-shell-cwd` path semantics. Use the existing helper if suitable; otherwise minimal decoding shared with the current file-path decoder. Cover the upstream hostname table plus foreign-host negative cases. The original Rustty local path decoder's lack of local-host enforcement is an older unrelated parity gap, not introduced by this range; do not claim full host-security parity.

## Equivalent or inherited areas

| Area | Disposition and evidence |
| --- | --- |
| Empty prefixed commands | Already equivalent: `Command::parse` checks nonempty after stripping either prefix. It rejects `direct:`, `shell:`, whitespace-only tails and NUL before process launch. Add explicit regression alongside direct-spacing fix if absent. |
| Long environment formatting | Inherited Ghostty `+show-config` formatter fix removes a 256-byte stack buffer. Rustty stores environment in `BTreeMap<String,String>` and passes strings to process builders without this limit. It has no `RepeatableStringMap.formatEntry` or full config-export CLI surface; normal parsing/env propagation is unaffected. An optional >300-byte config-env regression can demonstrate no truncation. Do not claim `+show-config` parity. |
| Split navigation precondition | Inherited Zig indexed-handle assertion. Rustty `workspace.rs` models a recursive `Split` tree, and layout/navigation derive IDs from `panes`; it does not index an unchecked Zig node/spatial array. Existing workspace navigation tests cover empty/missing focus. |
| GTK split resize | Inherited GtkPaned sentinel/overflow fix. Rustty `Split::resize_with_minimum` uses computed float rectangles and visible divider bounds with clamped minima; there is no unallocated `max-position=G_MAXINT` state. `constrained_dividers_and_keyboard_resizes_use_the_visible_position` is relevant existing regression. |
| Mac main-actor Surface | Inherited Swift type-safety change. Rustty's native services require `MainThreadMarker`, window/event-loop state lives in the main-thread App, and terminal workers send owned effects. It has no Swift `Ghostty.Surface: Sendable` wrapper. |
| Mac first tab frame | Inherited. Rustty explicitly sets `NSWindowTabbingMode::Disallowed` and draws its tabs; it never looks for `NSTabBarNewTabButton` or uses AppKit native-tab view constraints. |
| Clipboard window termination | Already structurally equivalent. Rustty clipboard consent is `egui::Window` drawn in the existing Winit window (`desktop.rs` around 7650), not an AppKit modal sheet; it cannot acquire `preventsApplicationTerminationWhenModal`. Quit events remain handled by App/event loop. A headless pending-clipboard quit regression is useful if app test harness can exercise it without native dialogs. |
| ghostty.h nullability | Inherited App C/Swift interface annotations and callers adapted to nonnull contracts; Rustty does not link this embedded App ABI. Rust APIs use Option/slices/owned values and native platform guards. Root audits separate libghostty-vt ABI/adapters. |
| SSH terminfo setup | Inherited Ghostty `+ssh`: stderr remains visible, installer uses `tic -x -o ~/.terminfo -`, then verifies `infocmp -x xterm-ghostty`. Rustty tmuxx transports use system SSH/native stdio and do not upload remote terminfo or call the installer; therefore no analogous installer mutation. `tmuxx.rs` drains/stores bounded SSH diagnostics and SSH helper manages authentication/askpass policy. Do not claim interactive SSH-instruction parity: that is a different existing transport UI. |
| Plain color output flush | Inherited `ghostty +list-colors` buffered writer fix. Rustty does not expose that CLI command or writer. |
| Executable path terminator | Inherited Zig fixed-buffer off-by-one. Rustty shell resolution uses PathBuf/OsString and platform process APIs; no manually terminated fixed path buffer. Existing command resolution errors remain returned. |
| Fuzz harness | Inherited Ghostty AFL replay adds OSC and handles negative signal exits; OSC3008 corpus now includes escaped metadata and fuzzer forces lazy fields. Rustty `Terminal::osc` does not implement OSC3008 metadata (it reports UnknownSequence), which is an older unsupported surface; the upstream production OSC3008 parser was unchanged in this range. The shared `test/rustty/osc_requests.py` corpus generator already reads both changed seed files, so refresh its frozen source checksums and compare the new seed through existing adapters. Do not claim hierarchical-field parity. No new terminal feature is introduced here. The Zig deployment-version flags only affect AFL compiler. |
| Localization | Inherited `po/` catalogs (Indonesian, Dutch, Japanese, Latvian and regenerated locations), unlocalized Ghostty glyph command title, and GTK Focus/Untitled translations. Rustty's egui palette/outliner uses its own English strings and doesn't load Ghostty gettext catalogs. No Rustty localization system is introduced. |
| z2d | Inherited 0.13.2 pin/mirror changes. Rustty uses Rust/WGPU and software renderers, not z2d. |
| Nix, Snap and ordinary CI | Inherited GNOME VM service/package repair, store-path GStreamer propagation, Snap audio plugins/environment/LD_LIBRARY_PATH fix, and build-gate/parallel Nix/test-dist CI scheduling. Rustty ships independent macOS/Windows bundles and Rust workspace workflows; it has no Snap/GNOME/GStreamer packaging path to adapt. |
| Contributor metadata | Inherited VOUCHED list entries only, no runtime consumer. |

## Libghostty release, packaging, docs and ABI CI

All changes here are **inherited in native reference tools or not applicable to Rustty's distribution**. Rustty's executable/library packages use Cargo workspace versioning and its own bundle scripts, never link the Ghostty C ABI, and do not publish libghostty-vt artifacts. No Cargo/API version change or duplicate C ABI/release system is warranted.

- Shared library metadata now derives from `lib_version` with `vt-` tags and `LIB_VERSION` source archives. CMake gets consistent real names/SONAMEs/import libraries, semver compatibility and pointer-size rules. Apple native linking sets current/compatibility versions and major-version @rpath; a Zig dependent deliberately takes the Zig Compile step so `dep.artifact` works. `-Demit-lib-vt` no longer defaults to XCFramework, retains library debug info except ReleaseSmall, skips app resources/man-page placeholder and doesn't install libraries into normal app bundles. Root's native-adapter build must compile against the changed `Config.init`/module APIs and use explicit applicable flags.
- `InstallDirs.zig`/CMake/pkg-config generators compute final paths after DESTDIR, support lib64/custom dirs/relocation, and force regeneration when the environment changes. `.pc` files move to `lib/pkgconfig`. Rustty scripts don't consume these paths; searched Rustty crates and harnesses for old `build.zig.zon.{json,nix,txt}`, fixed `libghostty-vt.0` and matching packaging assumptions: no consumers.
- Dependency manifests moved to `deps/`. `LibVtPackages` records fetched/reachable packages and third-party licenses; generator discovers required packages across targets, validates names/version/license/coverage, and `dist_deps` makes deterministic sorted offline tarballs with exact package coverage. Fonts/themes/app-only Zig deps and Wuffs test pixels are lazy/skipped for lib-vt. simdutf license files/version metadata are retained; mirrors change sources without Rust dependency updates.
- New release workflow builds/tests source/dependency tarballs, static/shared libraries, WASM/XCFramework, checksums/signatures and docs; package source and docs guidance describe these artifacts. Nix package tests exercise pkg-config/CMake shared/static consumers and install metadata. macOS dylib checker derives expected metadata from real filename.
- ABI CI builds a public-header probe because Zig DWARF marks exports private, verifies each export is declared and present in the dump, compares twice to separate additions from breaks, allows sized-struct tail additions, and checks SONAME/version nodes. Symbol-version suffixes are stripped for comparisons. The workflow intentionally skips comparison while no baseline exists; intentional major-bump enforcement remains a reviewer duty documented upstream. A separate source type manifest/export check covers other platforms. Root audits runtime ABI contracts/types and their native adapter effects. Packaging checks themselves do not define a Rust protocol change.
- Hidden visibility for simdutf/highway/Wuffs and Zig Highway detector prevents implementation symbols leaking from the native C library; Rustty does not export/link those symbols.
- Doxygen changes reorganize public API documentation, examples and guides (all added guide/convention/getting-started text and complete example diffs inspected); C examples adapt to new typed keys/setters/buffer shapes and versions. New hello/allocator examples demonstrate the same ABI, not new terminal behavior. Root audits ABI runtime changes; no duplicate Rust example or prose port is needed to complete an existing Rustty surface.
- Docs deployment builds static tip and versioned release sites; script validates doc directory/version names, copies a latest release to `/vt/`, applies robots and Vercel redirect rules. These artifacts are unrelated to Rustty app shipping; no deployment was invoked.

Final read-only coverage: complete `test.yml` changes include native ABI/feature-off/Windows install/macOS dependency checks, offline build/test, Doxygen stderr failure, tag-forced tests, and scheduling refactors. Complete release and docs-site workflows and all Doxygen guides/examples were inspected. No additional Rustty port beyond the four above was found. No workflow, release, deployment or benchmark was run by this audit.

No performance claim applies to these app/build/docs changes. The direct command/hostname fixes are correctness changes; appearance overrides should update on an action/system event and avoid any per-frame allocations. Themes are data-only.

## Validation recommended for final integration

- Focused `rustty` config and shell tests; `tmuxx-core` admission tests.
- Headless desktop/theme/action/palette tests and native-host compilation on available macOS. Windows host compilation/runtime remains a platform limit if unavailable.
- Verify all refreshed theme entries parse under existing `ConfigLoader::themes` checks, retain count/license/provenance and both bundle consumers.
- Fresh native tools from root's adapter commit validate inherited build defaults and API contracts. Full libghostty release/offline/Nix/Snap/ABI CI and deployment are not required to prove these Rust changes and were not executed.

## Window appearance implementation plan

Scope before edits:

- `crates/rustty/src/config/keybinding.rs`: parameterized action and parser, invalid-value regressions. Reuse existing `WindowTheme` (System/Light/Dark) inside `Option<WindowTheme>` if clear: `None` means Config. Accept only action values config/system/light/dark, never Auto. Existing config enum remains unchanged.
- `crates/rustty-app/src/desktop.rs`: one session field for override; four entries in existing `palette_commands`; action handler updates appearance and soft-reloads conditional terminal themes without writing config or restarting sessions. `ui_theme` calls during startup/reload/theme-preview/draw must resolve the override. Keep original configured values in loaded/scoped Config so clearing uses the latest config.
- Keep actual OS appearance distinct from effective app appearance: `ConfigLoader.dark_mode` currently does both jobs. The new action must update conditional theme choice as well as chrome and reports. Upstream GTK `handleStyleManagerDark` invokes `core_app.colorSchemeEvent`; macOS `BaseTerminalController::updateColorSchemeForSurfaceTree` reads NSApp effective appearance. Resolve config/system/light/dark to an effective scheme, reload conditional themes once when it changes, retain the last genuine OS value during forced appearance, and restore the latest underlying configured/system selection on Config/System. `Action::ReloadConfig` currently directly resets loader.dark_mode from `event_loop.system_theme` and must respect the override.
- `tmuxx_theme.rs`: global override must apply over both global and scoped host/session window_theme; current borrowed `window_config` remains available for terminal/font values. `window_color_scheme` uses effective appearance and session host reports, while attached clients preserve owner-only replies. Avoid recomputing/cloning Config per frame; a small resolver taking `Config` and override is sufficient.
- New/current native windows and quick terminals: apply Winit `window.set_theme` or platform existing functionality during create/configure and action handling. Native Mac menus/application appearance require care: existing `Platform::system_theme` reads NSApp.effectiveAppearance; setting NSApp.appearance may make this read the override rather than actual OS state. Prefer a genuine system-state source or only per-window overrides with explicit menu support; don't create system-change feedback loops. Windows custom menu already exposes `set_theme_for_hwnd` inside its menu implementation, so route resolved theme there instead of adding another renderer.
- `outliner_host.rs`: it shares egui Context for content but its Winit window/native chrome also needs the effective global override; repaint on action/reset.
- `platform/macos.rs` and `platform/windows.rs`: update native appearance without changing geometry, focus or persistent settings. Conditional terminal palettes must follow the effective app scheme through the existing theme loader and Session::apply_config; fixed themes keep their configured colors. Both currently configure custom titlebar/background but don't apply Config.window_theme to native OS chrome.
- `theme_preview.rs`: preview/cancel copies appearance into loaded configs; session override stays separate and survives both. On Config reset, restore the current underlying preview/config choice, not startup data.
- README next to theme picker: four commands and keybinding spelling, session-only/no persistence/reload survival.

Native source detail: Winit macOS `ActiveEventLoop::system_theme` and Rustty `Platform::system_theme` both read NSApp.effectiveAppearance; Winit per-window `set_theme` sets NSWindow.appearance and suppresses ThemeChanged notifications for forced windows. Windows `ActiveEventLoop::system_theme` reads actual OS preferences. If app-level Mac appearance is used for native menus, use an independent true-system source and ensure OS changes are observed even with every window forced.

Tests: parser exact four variants+bad/missing argument; resolver config+override matrix including dual theme/scoped config; simulate config replacement while override set then clear; paired light/dark theme colors follow action/reset and do not oscillate; genuine OS changes while forced are remembered for System; verify theme preview cancel doesn't erase override; command-palette entries; headless App test for current/new/outliner resolution and color-scheme reports if existing harness supports it. Do not build a broad abstraction or storage layer for four values.

## Complete assigned commit mapping

| Commit | Change | Area |
| --- | --- | --- |
| `c16d204bf` | deps: update z2d to v0.13.2 | z2d dependency |
| `316ab91e4` | po: update code sources | Ghostty localization |
| `bcc1b9faf` | i18n: don't localize the "Ghostty" command's title | Ghostty localization |
| `0cb81fe08` | datastruct/split_tree: assert goto() is given a valid handle | Split navigation precondition |
| `acd72a42f` | i18n: don't localize the "Ghostty" command's title (#14888) | Ghostty localization |
| `057f14896` | deps: update z2d to v0.13.2 (#14869) | z2d dependency |
| `20ba6462d` | datastruct/split_tree: assert goto() is given a valid handle (#14904) | Split navigation precondition |
| `40713449c` | i18n: command palette - Focus and Untitled translation support | Ghostty localization |
| `4c8ad9817` | config: fix formatting of long environment values | Long environment formatting |
| `9ee1031ba` | config: use allocator printing in long-value test | Long environment formatting |
| `0b8b9984e` | config: fix formatting of long environment values (#14908) | Long environment formatting |
| `5ee215000` | nix: drop removed gnome-photos from VM excludes | Ghostty Nix |
| `f563ceda3` | nix: apply GNOME Shell unsafe mode to the user unit | Ghostty Nix |
| `aa16f9429` | nix: drop the revision from the zig deps derivation name | Ghostty Nix |
| `04c759750` | ci: gate expensive jobs on a quick build instead of the full test suite | Ghostty CI scheduling |
| `68b837766` | ci: split build-nix into one job per target and architecture | Ghostty CI scheduling |
| `ecc87922e` | ci: build the source tarball without waiting on its tests | Ghostty CI scheduling |
| `184824102` | ci: run the full test suite once across the GTK and SIMD legs | Ghostty CI scheduling |
| `c27ef4c1b` | ci: improvements to speed things up, step one shorten critical paths for tests (#14911) | Ghostty CI scheduling |
| `7ee1074fb` | Update VOUCHED list (#14915) | Contributor metadata |
| `4d7de0a48` | fix(ssh): show authentication messages during terminfo setup | SSH setup |
| `b2c78c9bd` | nix: fix GNOME VM tests on main (#14910) | Ghostty Nix |
| `8c04d7c2e` | config: reject empty prefixed commands | Empty prefixed commands |
| `539a7f1ce` | nix: use store paths for propagated GStreamer packages | Ghostty Nix |
| `df1c99c8a` | Update VOUCHED list (#14930) | Contributor metadata |
| `93967b7f9` | i18n: command palette - Focus and Untitled translation support (#14055) | Ghostty localization |
| `108d503ac` | Update VOUCHED list (#14936) | Contributor metadata |
| `854e51422` | snap: bundle gstreamer plugins so audio bell works | Ghostty Snap |
| `7c6e1011c` | Update VOUCHED list (#14943) | Contributor metadata |
| `e1e05c1cd` | nix: use store paths for propagated GStreamer packages (#14929) | Ghostty Nix |
| `cf0ab14ae` | config: reject empty prefixed commands (#14921) | Empty prefixed commands |
| `8bb4f4584` | fuzz: fix replay crash detection and add osc target | Fuzz harness |
| `ff2382421` | snap: bundle gstreamer plugins so audio bell works (#12676) | Ghostty Snap |
| `90ed851de` | config: ignore repeated space in direct commands | Direct command spacing |
| `e2593c149` | config: ignore repeated space in direct commands (#14950) | Direct command spacing |
| `86c704afd` | gtk: fix jumps when resizing splits via keybind | GTK split resize |
| `ca91d0a24` | gtk: fix jumps when resizing splits via keybind (#14951) | GTK split resize |
| `32da5e47b` | snap: avoid trailing colon in LD_LIBRARY_PATH | Ghostty Snap |
| `93404a48a` | Update VOUCHED list (#14957) | Contributor metadata |
| `1d23d02ae` | snap: avoid trailing colon in LD_LIBRARY_PATH (#14954) | Ghostty Snap |
| `44893148d` | cli: show SSH authentication messages during terminfo setup (#14916) | SSH setup |
| `a0706b79b` | fuzz: fix replay crash detection and add osc target (#14896) | Fuzz harness |
| `fbed180a5` | Update VOUCHED list (#14960) | Contributor metadata |
| `0a672a914` | Update VOUCHED list (#14968) | Contributor metadata |
| `c911f96ee` | Update VOUCHED list (#14969) | Contributor metadata |
| `f7b25c812` | termio: don't require RFC 1123 hostnames in OSC 7 | OSC 7 hostnames |
| `c7be56a04` | fuzz: exercise hierarchical context in osc harness | Fuzz harness |
| `83b57ac5e` | fuzz: exercise hierarchical context in osc harness (#14977) | Fuzz harness |
| `845736e1d` | deps: Update iTerm2 color schemes | Theme resources |
| `c42f6fb47` | Update VOUCHED list (#14983) | Contributor metadata |
| `2fc8d02c2` | Update iTerm2 colorschemes (#14981) | Theme resources |
| `a5b44039d` | Update VOUCHED list (#14992) | Contributor metadata |
| `a3e349f7a` | i18n: update Bahasa Indonesia translation (#14991) | Ghostty localization |
| `fd29936f0` | macos: make Surface main actor-isolated | Mac main-actor isolation |
| `7cd6fa021` | Sync CODEOWNERS vouch list (#15002) | Contributor metadata |
| `5c6d43eee` | macos: make Surface main actor-isolated (#14999) | Mac main-actor isolation |
| `042b16b61` | termio: don't require RFC 1123 hostnames in OSC 7 (#14971) | OSC 7 hostnames |
| `379bdfabf` | Update VOUCHED list (#15016) | Contributor metadata |
| `825f31c52` | Update VOUCHED list (#15022) | Contributor metadata |
| `0edfc656f` | macOS: fix tab bar frame on macOS 27 when it first appears. | Mac tab frame |
| `0b1729e94` | Update VOUCHED list (#15036) | Contributor metadata |
| `3eb5af718` | i18n: Update Dutch translations | Ghostty localization |
| `0e067ded7` | core: declare pointer nullability in ghostty.h | App pointer nullability |
| `a455842c2` | core: declare pointer nullability in ghostty.h (#15039) | App pointer nullability |
| `b148bf728` | macos: allow quitting with a clipboard confirmation open | Mac clipboard quit |
| `fa3b435cb` | Update VOUCHED list (#15041) | Contributor metadata |
| `6676bbaec` | Update VOUCHED list (#15045) | Contributor metadata |
| `0d1d5e884` | macos: allow quitting with a clipboard confirmation open (#15040) | Mac clipboard quit |
| `060ff6b52` | i18n: update Japanese translation (#15025) | Ghostty localization |
| `2d1f9cf10` | os: reserve space for executable path terminator | Executable path terminator |
| `fef7af4e1` | os: reserve space for executable path terminator (#15058) | Executable path terminator |
| `c7f68a7f4` | macOS: fix tab bar frame on macOS 27 when it first appears. (#15029) | Mac tab frame |
| `def58d386` | cli: flush plain color list output | Plain color list flush |
| `1830cd250` | libghostty: derive shared library version metadata from lib_version | Lib-vt release packaging |
| `8edb49e38` | libghostty: version the lib-vt tarball and vt- tags from lib_version | Lib-vt release packaging |
| `184b01ab3` | libghostty: install a CMake package config and fix lib-vt packaging defaults | Lib-vt release packaging |
| `7b06c0541` | libghostty: record the final install paths in the lib-vt .pc and CMake files | Lib-vt release packaging |
| `3a1457c34` | ci: make the lib-vt leak checks able to fail | Lib-vt release packaging |
| `ac1feec83` | libghostty: link the lib-vt dylib with Zig when Ghostty is a dependency | Lib-vt release packaging |
| `97a5653d3` | pkg/wuffs: give the tests their own module | Lib-vt release packaging |
| `5b500d143` | libghostty: generate the lib-vt dependency list and third-party manifest | Lib-vt release packaging |
| `4f2139ab4` | ci: build and test libghostty-vt offline from its dependency list | Lib-vt release packaging |
| `2eda578e7` | libghostty: ship simdutf's license texts and take its version from pkg/simdutf | Lib-vt release packaging |
| `3de712930` | build: move the generated dependency files into deps/ | Lib-vt release packaging |
| `9fc62a98c` | libghostty: prepare libghostty-vt for tagged releases (#15069) | Lib-vt release packaging |
| `29bf7aa9b` | cli: flush plain color list output (#15073) | Plain color list flush |
| `6581a593c` | ci: add a release workflow for libghostty-vt | Lib-vt release packaging |
| `1a2cf06dd` | docs: add a packaging guide for libghostty-vt | Lib-vt release packaging |
| `281d34115` | build: don't install the man page placeholder for libghostty-vt | Lib-vt release packaging |
| `e95bed633` | libghostty: release workflow and packaging guide for libghostty-vt (#15076) | Lib-vt release packaging |
| `9b4a0076d` | cli: explicitly install terminfo to ~/.terminfo on ssh remote host | SSH setup |
| `081a5871e` | i18n: Update Dutch translations (#15037) | Ghostty localization |
| `8474e9421` | build: load only the packages libghostty-vt compiles | Lib-vt release packaging |
| `fd5e90e8c` | libghostty: publish a dependency tarball for offline builds | Lib-vt release packaging |
| `34afe119f` | mirror deps | Lib-vt release packaging |
| `5255e48b6` | libghostty: publish a dependency tarball with each libghostty-vt release (#15095) | Lib-vt release packaging |
| `1da2e165b` | ci: check the libghostty-vt ABI with abidiff | Lib-vt ABI CI |
| `c757567e4` | ci: build a Zig dependent with the macOS SDK and test the Windows install layout | Lib-vt release packaging |
| `e607b9e97` | ci: strip symbol versions in the ABI export list | Lib-vt ABI CI |
| `6ba2c2b27` | app: add window theme switching action | Window appearance action |
| `4a59fd20a` | i18n: update window theme action catalogs | Window appearance action |
| `dce5a9464` | app: add config value to set_window_theme | Window appearance action |
| `8b27b66f3` | gtk: follow the system for conditional themes | Window appearance action |
| `27eb594d2` | libghostty: test ABI in CI (#15096) | Lib-vt ABI CI |
| `6283611dd` | cli: verify xterm-ghostty resolves after ssh install | SSH setup |
| `bbccbb546` | app: add window theme switching action (#14208) | Window appearance action |
| `de4bc494a` | i18n: updated Latvian translations | Ghostty localization |
| `6095e277a` | pkg: build simdutf, highway, and wuffs with hidden visibility | Native dependency exports |
| `c445f448a` | pkg: build simdutf, highway, and wuffs with hidden visibility (#15102) | Native dependency exports |
| `8a5d14bd0` | i18n: updated Latvian translations (#15100) | Ghostty localization |
| `db663e075` | Update VOUCHED list (#15106) | Contributor metadata |
| `16f5674dc` | Update VOUCHED list (#15107) | Contributor metadata |
| `2c76f2997` | cli: explicitly install terminfo to ~/.terminfo on ssh remote host (#15061) | SSH setup |
| `3c3a8873e` | doxygen: fix the configuration and keep the build free of warnings | Lib-vt API documentation |
| `56919d3ff` | doxygen: register every C example and rename c-vt to c-vt-osc | Lib-vt API documentation |
| `f21f40018` | doxygen: nest the API reference and describe every topic | Lib-vt API documentation |
| `5acbc10eb` | doxygen: move the API conventions out of the main page | Lib-vt API documentation |
| `6f41fbaad` | doxygen: write the Examples page and fix example names and READMEs | Lib-vt API documentation |
| `3a73ef542` | doxygen: move the terminal guides out of the reference pages | Lib-vt API documentation |
| `2bc37a0ee` | doxygen: write the main page and Getting Started | Lib-vt API documentation |
| `734145430` | doxygen: move the rendering and image guides out of the reference pages | Lib-vt API documentation |
| `51bff35a7` | doxygen: move the input, grid, selection, search and snapshot guides out of the reference pages | Lib-vt API documentation |
| `9a0cb2142` | doxygen: give every topic page the same short intro | Lib-vt API documentation |
| `ba6639195` | doxygen: wrap the markdown pages at 80 columns | Lib-vt API documentation |
| `d2ce23a87` | doxgyen: update some docs | Lib-vt API documentation |
| `2f8d9e1f9` | doxygen: update concepts | Lib-vt API documentation |
| `477bfd294` | libghostty-vt: rewritten Doxygen docs (#15120) | Lib-vt API documentation |
| `4d6fb3263` | libghostty-vt: deploy docs from GHA, move to Vercel, prep release site | Lib-vt docs deployment |
| `8b3b025bd` | libghostty-vt: deploy docs from GHA, move to Vercel, prep release site (#15123) | Lib-vt docs deployment |

## Completed app ports

- Direct-command spacing: `5a49523` / `upvquxw`. Repeated literal spaces no longer create empty argv entries; literal tabs/shell commands preserved. Regression failed before fix; all 72 then-current config tests passed on Rust 1.95.
- OSC 7 URI authorities: `16e2217` / `ytlxxys`. tmuxx admission and Rustty ShellInfo decode percent-encoded hostnames before local matching/Windows UNC handling; malformed escapes, NUL and foreign-host guards retained. Both regressions failed before fix; 6 admission and 5 shell tests passed. Windows x86_64-msvc tests compiled; no Windows runtime available.
- Bundled themes: `7aaac22` / `spppnxx`. Upstream archive27cbbb5 verified by SHA256 and Zig package hash; 42 new/5 changed themes, total861, MIT license unchanged. All861 exact source bytes verified and accepted by actual ConfigLoader. Existing Nu/PowerShell packaging already copies themes/licenses.
- Session appearance: `6aced98` / `tpnlmqr`, `feat(rustty): support session window appearance overrides`. `set_window_theme:config|system|light|dark` routes local/global actions and palette entries. Separate true OS state/session override drives native application/window/menu appearance, egui, conditional palettes and color reports for existing/new windows, quick terminals, outliner, scoped tmuxx themes and previews. Overrides survive reload; config reset reads latest saved configuration; actions never write configuration. macOS observes distributed appearance changes with independent NSUserDefaults preference read; Windows uses retained UISettings::ColorValuesChanged registration, removed before WinRT teardown. No per-frame config clones added.

### Appearance validation

- Rust1.95 `cargo test -p rustty --locked --offline --lib config::tests`:73passed.
- `cargo test -p rustty-app --locked --offline --bin rustty`:144passed with required local-socket/Metal access outside sandbox. Initial sandbox run had8 Unix-socket EPERM failures and1 missing Metal adapter; the remaining actual failure was a command-palette test assuming2 `cfg` matches, adapted for the third matching appearance action before passing all144.
- Focused appearance tests:5passed, including resolver/config/scoped/preview matrix, CLI preservation and palette entries; focused input search shortcut test:1passed.
- `cargo test -p rustty-app --locked --offline --test native_panel -- --ignored`:passed on hidden AppKit windows/panels/outliner. Verified forced light/dark NSApp/window effective appearance while real OS query remained unchanged, and restored test process's prior AppKit appearance.
- Final offscreen native app smoke passed54 checks, including3 explicit appearance checks for actions, new-window inheritance, reload/reset. Every live pane's palette/query defaults, native chrome and shared egui theme checked. Artifacts `/private/tmp/rustty-reintegrate.leadij/appearance-smoke-final/`; full log at `appearance-smoke-final.log`. First full smoke also passed before adding result labels and equivalent Boolean lint simplification.
- Windows `cargo check -p rustty-app --locked --offline --target x86_64-pc-windows-msvc --tests`:passed. Windows runtime not run. No real OS preference changes were made, so live OS change delivery was code/API-reviewed, not exercised by modifying user settings.
- macOS `cargo clippy -p rustty-app --locked --offline --all-targets --no-deps -- -D warnings`:passed. Strict dependency lint run stopped on existing rustty-vt/program_status.rs:148 unnecessary_lazy_evaluations; allowing that lint exposed existing tmuxx-runtime/delivery.rs:47 manual_inspect. Optional Windows no-deps strictClippy stopped on existing platform/windows/notifications/tui.rs:262,310 collapsible_if. All untouched; parent owns final workspace QA disposition.
- Rustfmt1.95 and staged diff checks passed; commit includes only13 intended code/docs/test files, no measurement artifacts. No push/issue/PR.
