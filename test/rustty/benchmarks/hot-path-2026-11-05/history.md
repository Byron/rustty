# Source revisions after report consolidation

The benchmark reports were moved out of seven implementation commits into the
final documentation commit. Original source revisions in the measurement JSON,
profiles and narrative remain unchanged as historical provenance; the table
below identifies the corresponding rewritten code commits and stable Tix changes.
No measurements were rerun or relabeled as measurements of the new hashes.

Every mapped revision was checked against its original: all code, tests and
non-report files are identical. The lower-bound app commit
`3a37a05276ee93d46367b5d5500e2542cab0e3da` is unchanged.

| Tix change | Original revision | Rewritten revision |
| --- | --- | --- |
| `noslpsr` | `b879380960f40527b13b8462f7e05fbd19ec8f5c` | `5e2dcc63b99fb0f77a4c9bd340e9e1d839af3fd0` |
| `ulwwwyo` | `5e3331bf4f4719d5fffaa085cba5efc7653c2053` | `937b5fb49abfca8c1f6bacda8226d6f874f1983b` |
| `zzroxwq` | `008b239cb6f08d930fe100bef0ac9de18570ba3a` | `7536d97878b1cff2431f78d45a528b1f575c59ac` |
| `pulovmp` | `a5eb4da54e47dd91ada969586d78f2d53b8d988f` | `81a30f4c7c1515843c326936a41a0476a8a97744` |
| `zmytysu` | `0d16175cc20e0b0180ed37b16fbc2a34ed291aa4` | `79f7c80052619c4bc3c2c6d228647b571087c72e` |
| `kkupsml` | `ff5a7de94a10e79fef7435e999e4980c99ac17b2` | `f6d6cb61d6f164ca279da1dc0868e60e4d448fc1` |
| `ymsrytz` | `1d781609cf380663d7562787e84232763c145129` | `9a1746dc6f4eab2bfe5a21707fc789ea4bed77a5` |

Executable fingerprints, parser-source fingerprints, timestamps, samples and
confidence intervals are preserved. The first parser report's build metadata
now names its original parser revision explicitly instead of referring to the
commit containing the report.
