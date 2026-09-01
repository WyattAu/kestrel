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

Shrinking budget: CI runs proptest with `cases = 256` (via `KESTREL_PROPTOP_CASES`
env var), `max_shrink_iters = 4096`; local dev defaults to 128 (overridable via
`KESTREL_PROPTOP_CASES`). The `proptest_cases()` helper reads
`KESTREL_PROPTOP_CASES` to allow per-target tuning.

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
5. `fuzz_terminal_sanitizer` — bytes → terminal sanitizer must strip all
   escape sequences and C0/C1 control chars, never panic.

Cadence: each CI run executes each target for 60 s against the committed
corpus (`fuzz/corpus/`); a weekly cron job runs 10-minute sessions. Crashes
are minimized, added to `tests/mime-corpus/` as regression fixtures, and filed
as issues with the seed.

### Running 24-hour fuzz sessions manually

Long-running fuzz sessions catch deep bugs that short CI runs miss. To run a
24-hour session locally:

```bash
# Run a single target for 24 hours (86400 seconds)
cargo +nightly fuzz run <target> -- -max_total_time=86400 -max_len=4096
```

Replace `<target>` with one of: `fuzz_mime_adapter`, `fuzz_link_classifier`,
`fuzz_imap_response`, `fuzz_html_sanitizer`, `fuzz_terminal_sanitizer`.

#### Crash artifacts

When a crash is found, libFuzzer writes the triggering input to:

```
fuzz/artifacts/<target>/
```

Each crash file is named with a hash of the input. These files are
**not** committed to the repository; they are ephemeral local artifacts.

#### Adding crash regressions to the corpus

After triaging a crash:

1. Minimize the crashing input: `cargo +nightly fuzz tmin <target> <artifact_file>`
2. Copy the minimized file into the committed corpus:
   ```bash
   cp fuzz/artifacts/<target>/<crash_file> fuzz/corpus/<target>/
   ```
3. Add a corresponding regression test in `tests/mime-corpus/` if the crash
   exercises the MIME parser or another core module.
4. Commit the corpus file and regression test together.

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
runner job) and always measure release builds. Baseline JSON files live in
`benches/baselines/` at the workspace root and are regenerated by
`cargo bench -- --save-baseline <name>`.

## 7. Coverage & gates

- `cargo-llvm-cov` with a changed-lines policy on PRs: new code must be
  exercised by a test that fails if the code regresses ("tested" definition,
  standards §4).
- Exempt from line-coverage: `main()` wiring, webview FFI glue, keyring FFI
  glue — covered instead by integration/security assertions.
- Security matrix (threat model §7) is **merge-blocking**: each mitigation
  row maps to a named test listed in the threat model; CI runs them in the
  `security` group and the PR template asks for the mapping.

## 8. Provider integration tests

Provider tests validate real IMAP connections against live email providers.
Every test is `#[ignore]`d and gated by `KESTREL_PROVIDER_INTEGRATION=1`.

### Running provider tests

**Single provider via shell script:**

```bash
KESTREL_PROVIDER_EMAIL=user@example.com \
KESTREL_PROVIDER_PASSWORD=app-password \
./tests/integration/providers/gmail.sh
```

**Generic provider via cargo:**

```bash
KESTREL_PROVIDER_INTEGRATION=1 \
KESTREL_PROVIDER_NAME=gmail \
KESTREL_PROVIDER_IMAP_HOST=imap.gmail.com \
KESTREL_PROVIDER_IMAP_PORT=993 \
KESTREL_PROVIDER_EMAIL=user@gmail.com \
KESTREL_PROVIDER_PASSWORD=app-password \
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
```

### Environment variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `KESTREL_PROVIDER_INTEGRATION` | Yes | — | Set to `1` to enable tests |
| `KESTREL_PROVIDER_NAME` | Yes | `generic` | Provider name for logging (e.g., `gmail`, `yahoo`) |
| `KESTREL_PROVIDER_IMAP_HOST` | Yes | — | IMAP server hostname |
| `KESTREL_PROVIDER_IMAP_PORT` | No | `993` | IMAP server port |
| `KESTREL_PROVIDER_EMAIL` | Yes | — | Full email address |
| `KESTREL_PROVIDER_PASSWORD` | Yes | — | App password or account password |

### What each test validates

1. **Connect + authenticate** — TLS handshake and SASL auth (PLAIN/LOGIN/SCRAM-SHA-256)
2. **LIST folders** — Server returns folder tree with INBOX present
3. **SELECT INBOX** — INBOX is selectable and returns valid status
4. **FETCH envelopes** — First 5 messages have UID and ENVELOPE data
5. **Clean logout** — Session terminates gracefully

### Expected results per provider

All 20 provider test scripts in `tests/integration/providers/` invoke the
same `provider_real` test with provider-specific host/port settings.
See `docs/provider-compatibility.md` for the full matrix.

### Known limitations

- Proton Mail tests require Proton Mail Bridge running locally (`127.0.0.1:1143`).
- OAuth2 flows are not tested by `provider_real`; Gmail OAuth2 is tested separately via `imap_real` with `KESTREL_GMAIL_INTEGRATION=1`.
- Migadu has no auto-detection preset; tests pass manual host/port via env vars.
- Verizon addresses may redirect through AOL servers; test with `verizon.net` credentials.
- AT&T legacy accounts may not support IMAP; some `sbcglobal.net`/`bellsouth.net` addresses may fail.

## 9. Provider Validation

Each provider is validated by running the generic IMAP test against real servers.
The test verifies: connect → auth → LIST → SELECT INBOX → FETCH envelopes → disconnect.

Provider-specific notes:
- Gmail: Requires App Password or OAuth2
- Outlook: Requires OAuth2 (basic auth deprecated)
- Proton: Requires Bridge desktop app running
- iCloud: Requires App-Specific Password

## 10. Fix-first policy

Every bug fix carries a regression test named after the issue
(`issue_<n>_<slug>`), placed in the module that owns the defect (corpus file
for parser defects). PRs without the named test are rejected by review
checklist.
