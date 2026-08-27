# AGENTS.md — Guide for AI Coding Agents

Everything an agent needs to work in this repository safely. Humans:
`CONTRIBUTING.md` covers the same ground for you.

## Read this first

1. `requirements.md` — the specification. If code and spec disagree, the spec
   wins; flag the conflict, don't silently pick one.
2. `docs/architecture.md` — crate graph, dependency rules, concurrency model.
3. `docs/message-protocol.md` — frozen core↔frontend contract.
4. `docs/adr/` — binding decisions with rationale. **Never contradict an
   accepted ADR.** If a change requires a new decision, propose an ADR in the
   PR instead of working around one.
5. `docs/engineering-standards.md` — the bar your code must clear.

## Commands

```bash
cargo build --workspace                    # build everything
cargo nextest run --workspace              # tests (fallback: cargo test --workspace)
cargo clippy --workspace --all-targets -- -D warnings   # MUST be clean
cargo +nightly fmt --all --check           # formatting check (apply: without --check)
cargo sqlx prepare --check                 # if you touched queries/migrations
cargo doc --workspace --no-deps            # doc build (broken intra-doc links fail CI)
```

Integration tests (Docker needed): `cargo nextest run --profile integration`.

## Hard rules

- **No ADR violations.** Check `docs/adr/0001`–`0008` before adding
  dependencies or touching concurrency, storage, parsing, config, errors, or
  logging.
- **Frontends** (`kestrel-tui`, `kestrel-gui`) depend only on `kestrel-core`.
  Core crates never import frontends. No lateral crate imports
  (`kestrel-sync` cannot use `kestrel-storage`).
- **No `unwrap`/`expect`** outside tests and documented invariants.
- **No new dependencies** in a PR you opened yourself without noting it in
  the PR description (standards §6 checklist applies).
- **No panics on untrusted input paths** — parser/storage limits are
  security-critical (`docs/threat-model.md` §4).
- **No secrets or PII in logs/tests** (threat model §6).
- Migrations: append-only under `kestrel-storage/migrations/`; regenerate
  `.sqlx` metadata in the same PR.
- Docs travel with code: if your PR changes behavior described in `docs/`,
  update the doc in the same PR.

## Conventions

- Typed IDs (`AccountId`, `FolderId`, `MessageId`) — never raw strings/u64
  across crates.
- Errors: `thiserror` enums with a recovery class (ADR 0007).
- Observability: `tracing` spans with `account`/`folder`/`uid` fields
  (ADR 0008).
- Time/ids: through the core abstractions only, never `SystemTime::now()`
  inline.
- Tests accompany new logic in the same PR; bug fixes add a regression test
  named after the issue.

## Definition of done for a task

1. Builds, clippy pedantic clean, formatted, tests added and green.
2. Docs/ADR touched where the change is described there.
3. `docs/engineering-standards.md` PR checklist satisfied.
4. PR description states what changed and why, referencing issue/ADR.

## Current status

Documentation phase complete; Phase 1 (core storage & parsing) is the first
development milestone — see `docs/roadmap.md`.
