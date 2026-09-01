# Kestrel Engineering Standards

Status: **v1.0 — binding** · The bar: correctness-critical systems practice
(avionics/defence-style discipline) applied pragmatically to a small team.

This document defines **how we work**. **What we build** is
`requirements.md`; **why it is shaped that way** is `docs/adr/`.

---

## 1. Principles → Concrete Rules

| Principle | Rule in this repo |
|-----------|-------------------|
| **KISS** | No dependency without an ADR-level justification; prefer std/tokio primitives; delete code aggressively |
| **YAGNI** | No abstraction until the second concrete case exists in-tree (exception: ADR-mandated trait seams) |
| **SOLID** | S: one service = one inbox + one responsibility (ADR 0004). O/L: protocol enums are closed, extend by new variant. I: frontends see `Command/Reply/Event` only. D: everything depends on `kestrel-core` vocabulary, never on concrete engines |
| **DRY** | Domain types live once in `kestrel-core`; storage/protocol structs map at the boundary via `From` |
| **Fail-fast, run-forever** | Invalid states rejected at edges (config load, parse); service runtime degrades, never crashes the process (ADR 0004 supervision) |
| **Determinism first** | Injected clock/ids (architecture §8); no wall-clock in logic; property tests over example tests where possible |
| **No silent anything** | Every error surfaced with recovery class (ADR 0007); every degradation is an event; every unsafe block is `// SAFETY:` commented and audited |

## 2. Rust Conventions

- **Toolchain:** pinned via `rust-toolchain.toml`. MSRV: the version specified
  in `workspace.package.rust-version` in the root `Cargo.toml` (canonical
  source of truth).
- **Formatting:** `cargo +nightly fmt` (imports granularity). Formatting is
  never reviewed manually — CI rejects unformatted code.
- **Clippy:** workspace `lints` table with `clippy::pedantic` enabled and a
  small audited allowlist; `#![deny(warnings)]` in CI. No `unsafe` outside
  audited sites (each requires a `// SAFETY:` comment + issue link).
- **Banned APIs** (clippy/custom lints): `unwrap`/`expect` outside tests and
  documented invariants; `std::time::SystemTime::now` outside the clock
  abstraction; `Arc<Mutex<...>>` crossing an `await`; unbounded channels;
  `Box<dyn Error>` in library crates.
- **Naming/typing:** typed IDs (`AccountId`, `FolderId`, `MessageId`,
  `OutboxId`, `BlobHash`) — never raw `String`/`u64` at crate boundaries;
  `snake_case` modules mirroring crate names; no `util` modules — name the
  concept.
- **Docs:** every `pub` item has `///` docs; every crate root links to its
  owning design doc.
- **Comments:** explain *why* (and link the ADR/issue), never narrate *what*.
  Invariants get `// INVARIANT:` markers.

## 3. Git & Review

- **Branching:** trunk-based — short-lived feature branches off `main`.
- **Commits:** Conventional Commits (`feat(tui): ...`, `fix(sync): ...`,
  `docs(adr): ...`); rebase before merge; no merge commits except release
  tags.
- **PR size:** target ≤ 400 changed lines of reviewable diff; larger changes
  are stacked PRs.
- **Reviews:** 1 approval for code, 2 for anything touching the message
  protocol, security (threat-model mitigations), migrations, or an ADR.
  Author does not self-merge.
- **PR checklist (enforced by template):** fmt/clippy/tests green; query
  metadata fresh (`cargo sqlx prepare --check`); ADR updated if decision
  changed; threat-model updated if attack surface changed; benchmarks
  attached if a hot path changed.
- **CI is the gate:** nothing merges on red CI, including docs links.

## 4. Testing Strategy (summary — full doc: `docs/testing-strategy.md`)

| Layer | Tool | Gate |
|-------|------|------|
| Unit | per-crate `#[cfg(test)]` | all new logic |
| Property-based | `proptest` | parsers, threading, query builder, sanitizer |
| Fuzz | `cargo-fuzz` (MIME, IMAP responses, link classifier) | corpora committed; OSS-Fuzz target when public |
| Integration | `cargo-nextest`, Dockerized Dovecot + Greenmail for IMAP/SMTP | sync engine paths |
| Security | threat-model §7 test matrix | mandatory for merged mitigations |
| Benchmarks | `criterion` + custom SLA harness | SLA table below |

- **Coverage:** `cargo-llvm-cov` on changed lines; new code requires tests in
  the same PR — "tested" means a test that fails if the code regresses.
- **Fix-first policy:** every bug fix lands with a regression test that
  reproduces the bug (issue number in test name).

## 5. Performance Budgets & CI Gates

The SLA table from `requirements.md` §8 is executable:

| Benchmark (criterion/SLA harness) | Gate |
|-----------------------------------|------|
| `bench/cold-start-tui` | < 50 ms target, 150 ms hard fail |
| `bench/cold-start-gui` | < 200 ms target, 500 ms hard fail |
| `bench/ingest` (envelopes/sec) | fail below 800/sec, warn below 1,500/sec |
| `bench/search-100k` | fail above 50 ms, warn above 15 ms |
| `bench/search-500k` | fail above 30 ms first-50 |
| `bench/scroll` (frame pacing, recorded) | no dropped frame on reference runner |
| `bench/idle-mem` (TUI/GUI) | warn 25/120 MB, fail 40/200 MB |

Rules: hot paths (`messages` ingestion, list windowing, Tantivy commits) may
not regress a benchmark > 10% without an ADR-accepted justification.
Baselines live in `benches/baselines/` (JSON) and are compared in CI; a
> 10 % regression gate is enforced on the hot paths above. Benchmarks run
pinned (fixed CPU governor, `--disable-default-features` noise control) in a
dedicated CI job; results posted to the PR.

## 6. Supply Chain & Licensing

- `Cargo.lock` committed; deps updated via audited, isolated PRs
  (Renovate config allowed).
- Project license: **Apache-2.0**. Gates in CI: `cargo audit` (advisories),
  `cargo deny` (licenses: Apache-2.0-compatible permissive only; GPL/lgpl
  dependencies are flagged and require an ADR — Slint's Royalty-Free Desktop
  License is the sole pre-approved exception, see ADR 0001).
- New dependency checklist: maintenance signal, license, `unsafe` usage,
  transitive footprint, and an ADR if architecture-significant
  (ADR 0000 §Scope).
- Releases: `cargo build --locked` from tag; artifacts signed; SBOM
  generated (Phase 5).

## 7. Observability Conventions (ADR 0008)

- Span names: `service.<name>`; mandatory fields `account`, `folder`, `uid`
  where applicable.
- Log privacy rules are threat-model §6 — binding; violations are release
  blockers.
- New subsystem PRs must include the spans/events they emit in the PR
  description.

## 8. Documentation Rules

- `docs/` is the source of truth; code comments link to it, never restate it.
- Doc changes ride with the code PR that made them true (single source of
  truth at every commit).
- Every ADR status change requires a `docs(adr)` commit referencing the
  deciding discussion.
- Dead docs are deleted, not left to rot.
