# Kestrel Testing Strategy

Status: **v1.0 — binding** · Governs every phase's exit criteria
(`docs/roadmap.md`). Inputs: `requirements.md` §8,
`docs/engineering-standards.md` §4–5, `docs/threat-model.md` §7.

---

## 1. Layout

| Layer | Location | Runs under |
|-------|----------|------------|
| Unit tests | `#[cfg(test)]` co-located with the code | default profile |
| Property tests | same module as the unit under test, `proptest` | default profile |
| Integration tests | `crates/<crate>/tests/*.rs` | default profile |
| Docker-gated integration | `crates/*/tests/integration.rs`, every test `#[ignore]`d and named `integration_*` | `--profile integration` |
| MIME corpus | `tests/mime-corpus/` at workspace root, loaded via `kestrel-core::testkit::mime_corpus()` | default profile |
| Fuzz targets | `fuzz/` workspace member (`cargo-fuzz`) | scheduled/CI-cron, smoke in CI |
| Benchmarks | `crates/kestrel-storage/benches/`, TUI/GUI start harnesses | `cargo bench`, CI compile + nightly run |

Shared fixtures live in `kestrel-core::testkit`: injected clock (`FakeClock`),
deterministic ID generator, overridable `Paths` (temp dirs), corpus loader.
**No test outside `testkit` may call wall-clock or real-`Paths` APIs** —
that is what the injection seams exist for (architecture §8).

## 2. MIME corpus (`tests/mime-corpus/`)

Curated raw `.eml` files exercising the hostile/broken input space:

| Group | Cases |
|-------|-------|
| `broken-headers/` | missing semicolons, un-escaped folds, 8-bit in headers, truncated encoded-words |
| `rfc2047/` | nested encodings, unknown charsets, split words, long runs |
| `charsets/` | ISO-8859-1..15, Windows-1252, Shift-JIS, GB2312, KOI8-R, invalid byte sequences |
| `nesting/` | `multipart` depth 1..64 (valid) and 65+ (must hit `ParseError::Limit`), `message/rfc822` chains |
| `bombs/` | base64/quoted-printable expansion bombs (must hit decoded-size limits), decompression ratio cap |
| `ambiguous/` | duplicate headers, conflicting Content-Types, missing boundaries, boundary at EOF, LF-only line endings |

Rules:

- Every file must parse (or fail) **deterministically** — corpus tests assert
  the expected outcome class, not just "does not crash".
- Provenance: samples are hand-crafted for this repo or derived from public
  RFC examples; no third-party private correspondence. A `README.md` in the
  corpus directory records origin per file.
- Every fuzz crash found upstream becomes a corpus file named
  `regression-<issue>.eml`.

## 3. Property testing (`proptest`)

| Property | Module | Strategy sketch |
|----------|--------|-----------------|
| Threading is idempotent | `kestrel-core::threading` | generate random reply graphs (message-ids, in-reply-to chains, subjects, dates); `thread(gen) == thread(thread(gen) applied)`; root count ≤ message count; acyclic |
| Sanitizer removes all C0/C1 + OSC | `kestrel-core::sanitizer` | arbitrary `Vec<u8>` → sanitized output contains no ESC (0x1b), no other C0 except `\t\n\r` |
| Parser limits hold | `kestrel-core::mime` | random nesting depth > 64 ⇒ `ParseError::Limit`; decoded size cap enforced |
| Link classifier | `kestrel-core::links` | generated homograph/punycode/display-mismatch tables must classify exactly as the fixture table |
| Query builder round-trip | `kestrel-storage::search` | random structured queries → Tantivy query → parse back ⇒ equal |
| CAS write idempotence | `kestrel-storage::blob` | random bytes; write twice ⇒ one file, refcount stable; delete with live refs ⇒ keep |

Shrinking budget: CI runs proptest with `cases = 256`, `max_shrink_iters = 4096`;
local dev defaults (`cargo nextest`) use `cases = 64` via env override to keep
the suite fast. Seeds for failures are pinned in-code via `proptest!(#seed)`.

## 4. Fuzzing (`cargo-fuzz`)

Targets (in `fuzz/`):

1. `fuzz_mime_adapter` — raw bytes → `kestrel_core::mime::parse` (no panic, no
   abort, limits enforced).
2. `fuzz_link_classifier` — URL/display-text pairs → classifier must return
   a decision, never panic.
3. `fuzz_imap_response` — bytes → `imap-next` receive path wrapped in our
   session decoder (regression shield for upstream codec bugs).
4. `fuzz_html_sanitizer` — bytes → HTML sanitizer/transform must terminate
   (time-bounded) and never emit active content.

Cadence: each CI run executes each target for 60 s against the committed
corpus (`fuzz/corpus/`); a weekly cron job runs 10-minute sessions. Crashes
are minimized, added to `tests/mime-corpus/` as regression fixtures, and filed
as issues with the seed.

## 5. Integration harness

- `docker compose` fixture set under `tests/integration/`:
  - **Dovecot** (IMAP): pre-seeded mailbox tree (INBOX + Archive + Sent with
    known UIDs, flags, a `UIDVALIDITY` bump script via `doveadm`).
  - **Greenmail** (SMTP): accepts any auth, records submitted messages;
    asserted via IMAP APPEND-to-Sent round trip.
- Tests connect using deterministic fixture accounts; every test is
  `#[ignore]`d and prefixed `integration_` so the default profile never needs
  Docker (`--profile integration` runs them with retries; see
  `.config/nextest.toml`).
- The GUI webview network-isolation test runs its assertions with
  `unshare -n` when available and skips (with a loud log) where namespaces
  are unavailable; CI always has them (threat model §7).

## 6. SLA benchmarks (engineering-standards §5 mapping)

| Harness | Where | Gate |
|---------|-------|------|
| `bench/ingest` | `crates/kestrel-storage/benches/ingest.rs` | fail < 800 msgs/s, warn < 1 500 msgs/s |
| `bench/search-100k` / `bench/search-500k` | `crates/kestrel-storage/benches/search.rs` | fail > 50 ms / > 30 ms first-50; warn > 15 ms |
| `bench/cold-start-tui` | `crates/kestrel-tui` start harness (hyperfine-style, in-crate) | fail > 150 ms; target < 50 ms |
| `bench/cold-start-gui` | `crates/kestrel-gui` start harness | fail > 500 ms; target < 200 ms |
| `bench/idle-mem` | harness reads RSS after quiescence (`/proc/self/statm`) | TUI warn 25 MB/fail 40 MB; GUI warn 120 MB/fail 200 MB |

Baselines are committed under `benches/baselines/` (JSON) and compared in CI;
a > 10 % regression on a hot path requires an ADR-accepted justification
(standards §5). Benchmarks pin CPU governor expectations in CI (dedicated
runner job) and always measure release builds.

## 7. Coverage & gates

- `cargo-llvm-cov` with a changed-lines policy on PRs: new code must be
  exercised by a test that fails if the code regresses ("tested" definition,
  standards §4).
- Exempt from line-coverage: `main()` wiring, webview FFI glue, keyring FFI
  glue — covered instead by integration/security assertions.
- Security matrix (threat model §7) is **merge-blocking**: each mitigation
  row maps to a named test listed in the threat model; CI runs them in the
  `security` group and the PR template asks for the mapping.

## 8. Fix-first policy

Every bug fix carries a regression test named after the issue
(`issue_<n>_<slug>`), placed in the module that owns the defect (corpus file
for parser defects). PRs without the named test are rejected by review
checklist.
