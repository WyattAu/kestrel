# Kestrel Testing Strategy

Status: **Planned — must be completed before Phase 1 milestone exit**
(`docs/roadmap.md`). Inputs: `requirements.md` §8,
`docs/engineering-standards.md` §4–5, `docs/threat-model.md` §7.

## Required contents (outline)

1. **Layout:** unit tests co-located (`#[cfg(test)]`); integration tests in
   `tests/` per crate; shared fixtures in `testkit` module of `kestrel-core`
   (clock/id/path injection — architecture §8).
2. **MIME corpus:** `tests/mime-corpus/` — curated broken/legacy messages
   (malformed headers, RFC 2047 edge cases, deep nesting, legacy charsets,
   bombs); corpus loader API; licensing/provenance rules for samples.
3. **Property testing:** `proptest` strategies for parsers, threading
   (generated reply graphs), sanitizer, query builder; shrinking budget
   rules in CI.
4. **Fuzzing:** `cargo-fuzz` targets (MIME adapter, IMAP response decoder,
   link classifier); corpus storage & CI cadence; regression seeding from
   crashes.
5. **Integration harness:** Dockerized Dovecot (+ Greenmail for SMTP);
   `--profile integration` nextest config; deterministic account fixtures;
   network-namespace checks for the webview (threat model §7).
6. **SLA benchmarks:** criterion benches + harness scripts mapping 1:1 to
   engineering-standards §5 gates; reference-runner spec; baseline
   management.
7. **Coverage & gates:** `cargo-llvm-cov` changed-lines policy; what is
   exempt (main, FFI glue) and why.
