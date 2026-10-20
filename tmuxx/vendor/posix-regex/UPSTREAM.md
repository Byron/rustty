# POSIX regular expression engine

The original Rust parser, matcher, tree and persistent-vector implementation is
vendored from `posix-regex` 0.1.4 (MIT), published by the Redox project:
<https://gitlab.redox-os.org/redox-os/posix-regex>.
Source commit: `063882aa6051b5075062d20d00b41ddc0fb8cd89`.
Crates.io archive SHA-256:
`66df580334caab2f744839ab1be85493d7ec731a92d6cf928008ab0b212bf3bc`.

This copy is kept independent of C tmux and does not link C implementation code.
Windows adaptations add a Unicode scalar alphabet with distinct invalid-byte
tokens, byte-offset capture projection, explicit locale character classes and
reviewed POSIX leftmost/longest capture ordering. Resource limits must reject
bounded operations explicitly rather than silently returning a negative match.

The upstream tests remain with the source. Additional Windows format and
malformed-input tests qualify the adapted behavior; source provenance alone is
not evidence of compatibility.

See [WINDOWS.md](WINDOWS.md) for the exact native locale, byte/capture semantics,
limits and executable validation commands. The inherited self-reference test now
expects the parser error its upstream comment required; the other inherited
assertions remain, with result handling adjusted for explicit resource errors.
