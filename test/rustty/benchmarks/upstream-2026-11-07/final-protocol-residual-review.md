# Final protocol residual review

All 177 comparisons reproduce on the preserved pre-carry Rust binary (`3882d7da6`, binary hash verified against `carry-before-build.json`). Both engines' saved reply files are byte-identical between the baseline replay and the final run. The replay used the Rust parity runner: 59 cases, 177 comparisons, 177 failures, no abort and no process error. These remain comparison failures; the audit does not relabel them as passes.

| Category | Comparisons | Complete attribution |
| --- | ---: | --- |
| Charset defaults | 21 | Only Rust's existing optional `POINTER_SHAPES 0x8001` snapshot records differ; every remaining frame byte and response field matches. |
| Host resize/size | 48 | The same pointer-record-only difference, including all snapshots in each request; no host-size field difference remains. |
| Progress reports | 18 | OSC 9;4 states 1, 2 and 4 accept `1_0` and `1__0` as 10 in Rust; native reports no value. Both `osc9` and its underscore-accepting helper are unchanged since the task started. Native rejection was added in `73768913b` on September 26, before the incoming base. |
| Host extended cursor status | 3 | `CSI ? 6 n` produces native `ESC[?1;1;1R`; Rust emits no event. Rust's DSR dispatch is unchanged since task start. Native DECXCPR support is `fbda61b87`, already in the old upstream base. |
| DND move/leave | 3 | Rust sends the offered MIME list only when it changes (or on drop); native includes it on every move. |
| DND acceptance before hover | 3 | Rust invalidates prior acceptance when a new drag enters; native retains the earlier rejection state. |
| DND MIME-limit/re-registration | 6 | Rust resets hover and offered data when registration restarts; native retains that drag state. |
| DND data errors and item limits | 39 | Rust concludes/reset held data after an erroneous request, emits its conclusion event, and rejects subsequent requests without an active drop; native retains the dropped items. All event and state differences are recorded, not only the first error string. |
| DND held-data lifecycle | 3 | The same reset/conclusion policy changes held item, offered and dropped state plus subsequent events. |
| DND data roundtrip and snapshots | 33 | Requests after conclusion/restoration produce Rust's lifecycle `EPERM` versus native `ENOENT`. The 27 snapshot variants were independently attributed earlier. |

The DND production file and its oracle adapter are byte-identical to the immutable pre-task checkout `c06197283`. The lifecycle refinement is the pre-range `fb662cab` (November 5); the incoming native DND delta only replaces two `ArrayListUnmanaged` aliases. These are older behavior differences. Only the documented lifecycle guard is claimed intentional; the other DND differences are not newly justified or repaired here.

The 69 pointer-only classifications use the existing narrow classifier. It compares common frames and stored CRC bytes without independently validating snapshot CRCs. The remaining 108 comparisons are substantive older differences, not optional-record classifications. None is silently excluded.

`final-protocol-residual-attribution.json` records all 177 IDs, every differing field path, current/baseline request and response hashes, binary and source hashes, exact argv, original failure counts and source ancestry. `audit-final-protocol-residuals.py` audits those saved files without launching an oracle. The full current/baseline failures remain in scratch. No repository source or fixture was changed, and no Cargo/build command was run.
