# Stream Deck CPU spikes, 2026-10-27

The supplied `russty-CPU-spikes.trace` points to repeated Stream Deck discovery
and device-open attempts. These threads account for **96.9% of the sampled
CPU weight**. The recording covers 7.512 seconds and contains 193 running-thread
samples, each weighted at 1 ms. Sample weights are statistical estimates, not
independent CPU accounting.

| Thread group | Sample weight | Share |
| --- | ---: | ---: |
| `rustty-deck-discovery` | 99 ms | 51.3% |
| `rustty-stream-deck` | 88 ms | 45.6% |
| Other Rustty threads | 6 ms | 3.1% |

Seven distinct discovery/worker thread pairs produce bursts beginning at
**0.496, 1.529, 2.557, 3.589, 4.620, 5.654, and 6.687 seconds**. The mean interval
is **1.032 seconds**, with individual intervals between 1.028 and 1.034 seconds.
Each pair spans about 26–32 ms of observed samples. These are sampled burst
boundaries, not exact thread creation or completion times.

Discovery stacks run through
`deck_device::discover → HidApi::new → add_devices → hid_enumerate`. Worker
stacks repeat HID enumeration, then reach
`open_serial → hid_open → hid_open_path → IOHIDDeviceOpen`.
`hid_enumerate` appears in **171 of 193** stacks and `HidApi::new` in **164**;
these inclusive counts overlap. No sampled worker stack reaches renderer
creation, image upload, HID reads/writes, or the steady device loop. Short
statistical samples cannot prove that an unsampled or inlined function never
executed.

## Device ownership

The trace profiles the repository build at
`target/release/Rustty.app/Contents/MacOS/rustty`. A separate process check found
an installed Rustty instance, and an IORegistry inspection showed that
installed instance registered as a client of the Stream Deck MK.2. These observations
identify two distinct application instances; the client observation was not
captured by Time Profiler itself.

The pinned `hidapi` 2.6.7 macOS implementation opens devices exclusively with
`kIOHIDOptionsTypeSeizeDevice`; Rustty does not enable its shared-device feature.
A device already controlled by the installed instance therefore plausibly
explains the repository build's repeated open attempts. The trace contains no
open return code, so a specific ownership conflict remains an inference; a
different device-open error could produce the same pattern.

## Relation to cursor rendering

The observed one-second retry cadence differs from the cursor's **600 ms**
phase interval. The sampled stacks identify HID discovery and opening, and
none show the Metal/terminal rendering path. Together, those observations
attribute these spikes to Stream Deck connection work rather than cursor
blinking.

The [cursor comparison](render-experiments.md#retained-cursor-layer-2026-10-27)
disabled Stream Deck in its smoke configuration. Its idle-rendering results
therefore do not measure this connection workload or predict total CPU usage
when Stream Deck is enabled.

## Change

Rustty backs off consecutive failed discovery/open attempts by **1, 2, 4, 8, 16, then
30 seconds**, retaining the 30-second cap on further failures. Finding a device
does not reset this delay before opening it succeeds. A successful connection
or a configuration restart resets the delay. Ordinary checks that find no
device retain their one-second interval. Worker and discovery thread creation
failures also back off. Windows uses the same retry scheduling; neither
platform's renderer changes.

## Verification

- `cargo test --locked --offline -p rustty-app --bin rustty deck_host::tests::`:
  all three tests pass, including backoff progression, cap, history retained
  across successful discovery, and reset on an explicit stop.
- `cargo clippy --locked --offline -p rustty-app --bin rustty -- -D warnings`:
  passes. The broader `--all-targets` check reports three existing desktop-test
  lints: two `chunks_exact_to_as_chunks` and one `type_complexity`.
- `cargo check --locked --offline -p rustty-app --bin rustty --target aarch64-pc-windows-msvc`:
  passes.
- `nu crates/rustty-app/build.nu --release --offline`: the release app bundle
  builds and passes signature verification.
- The rebuilt app passes `RUSTTY_SMOKE_DECK=only` with
  `RUSTTY_SMOKE_OFFSCREEN=1` and isolated state, including the new assertion
  that `Action::Connected` clears accumulated backoff. USB remains disabled;
  the fixture checks that no native window is visible or has keyboard focus.
  Its hidden reporting pane is explicitly started to accommodate lazy tab
  startup. Native minimization and quick-window animation are skipped.
- `cargo fmt --all --check` and `git diff --check`: pass.

No before/after CPU improvement has been measured for this change. The retry
cap reduces attempt frequency, not the cost of one failed attempt. Reconnecting
after another controller releases the device may take up to 30 seconds.
