# ADR 0002: Use Stalwart `mail-parser` Behind a Core Trait

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §4.1 demands parsing of complex nested MIME
(`multipart/alternative|mixed|related`, `message/rfc822`), robust transcoding of
legacy charsets via `encoding_rs`, and graceful failure on malformed input
(broken RFC 2047 words, missing semicolons). The parser sits directly on the
network attack surface (threat model: untrusted input), runs on the hot
ingestion path (> 1,500 msgs/sec SLA), and feeds both the SQLite metadata store
and Tantivy indexing.

## Decision

We use **Stalwart's `mail-parser`** crate for all MIME parsing and header
decoding, wrapped behind a `MimeParser` trait defined in `kestrel-core`:

```rust
pub trait MimeParser {
    type Output; // parsed, indexed view of a message
    fn parse(raw: &[u8]) -> Result<Self::Output, ParseError>;
}
```

- `kestrel-sync` and `kestrel-storage` depend only on the trait; the concrete
  `mail-parser` implementation lives in `kestrel-core` (or a thin
  `kestrel-mime` adapter module).
- Charset transcoding uses `mail-parser`'s built-in `encoding_rs`-backed
  conversion; we add our own fuzzing and property tests around the adapter
  boundary regardless of upstream quality.

## Consequences

- **Robustness:** `mail-parser` is the parser embedded in the Stalwart mail
  server — production-hardened against real-world broken email, zero-copy over
  the raw buffer, and actively fuzzed upstream.
- **Performance:** zero-copy structure building supports the > 1,500 msgs/sec
  ingestion budget without intermediate `String` allocations.
- **Swappability:** the trait boundary keeps us honest — if `mail-parser`
  stagnates, a replacement (or a winnow-based custom parser for specific
  paths) is a localized change, consistent with the DIP principle.
- **Cost:** trait indirection on a hot path; mitigated by parsing in batch
  worker tasks (never on UI threads) where virtual dispatch cost is noise
  compared to I/O.
- We must still handle `mail-parser`'s "best-effort" outputs (it rarely hard
  fails): the adapter maps degradation to typed `ParseWarning`s surfaced in
  logs, never panics.

## Alternatives Considered

- **`mailparse`** — mature and simple, but copying (allocation-heavy) and
  weaker on deeply nested/malformed structures; also effectively in
  maintenance mode.
- **Custom zero-copy `winnow`/`nom` parser** — rejected for now: writing a
  security-critical RFC 5322/2045/2047 parser that never crashes on hostile
  input is a multi-month effort with high defect risk; defence-grade rigor
  means using a battle-tested implementation and investing our verification
  budget in fuzzing the integration instead. The trait (this ADR) keeps the
  door open.
- **`mail-parser` + `chumsky` hybrid for headers** — unproven benefit.
