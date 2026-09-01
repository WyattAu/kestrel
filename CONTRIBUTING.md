# Contributing to Kestrel

Thanks for helping build Kestrel — a modular, offline-first email client in
Rust. This document covers the practical workflow. For **how we engineer**
(standards, testing gates, review rules) read `docs/engineering-standards.md`.
For **why the architecture is shaped this way**, read the ADRs in `docs/adr/`.

## Prerequisites

- Rust stable (pinned via `rust-toolchain.toml`; install with rustup)
- A C compiler and `pkg-config` (SQLite, TLS backends)
- Docker (integration tests only)

## Getting started

```bash
git clone https://github.com/WyattAu/kestrel.git
cd kestrel
cargo build --workspace
cargo nextest run --workspace          # unit + integration (docker tests are opt-in)
cargo clippy --workspace --all-targets
cargo +nightly fmt --all --check
```

Read `docs/architecture.md` and `docs/message-protocol.md` before writing
code — the message protocol is a frozen contract and changes to it follow the
ADR process.

## Workflow

1. **Discuss first** for anything touching: the message protocol, storage
   schema/migrations, security posture, or an ADR — open an issue before
   writing code.
2. Branch: `feat/<short>`, `fix/<short>`, `docs/<short>` off `main`.
3. Commits: Conventional Commits (`feat(tui): add thread collapse`).
   Reference the issue (`Closes #123`).
4. Open a PR. Complete the PR checklist (template provided). CI must be green
   — fmt, clippy pedantic, nextest, `cargo sqlx prepare --check`, doc links.
5. Review: 1 approval normally; **2 approvals** for message protocol,
   security mitigations, migrations, ADR changes. No self-merge.

## PR checklist

- [ ] `cargo +nightly fmt --all --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo nextest run --workspace` green
- [ ] New logic has tests; bug fixes have regression tests (issue # in name)
- [ ] ADR added/updated if this changes a recorded decision
- [ ] `docs/threat-model.md` updated if attack surface changed
- [ ] `cargo sqlx prepare --check` clean if queries/migrations changed
- [ ] Benchmarks attached if a hot path changed (standards §5)

## Testing & benchmarks

See `docs/testing-strategy.md` (unit/property/fuzz/integration layout) and
`docs/engineering-standards.md` §5 (SLA gates). Summary:

```bash
cargo nextest run --workspace
cargo nextest run --profile integration   # docker-based IMAP/SMTP tests
cargo bench --bench ingest -- --save-baseline
```

## Testing Providers

To test against real email providers:

1. Set up test credentials (see `tests/integration/providers/README.md`)
2. Run individual tests: `./tests/integration/providers/gmail.sh`
3. Run all tests: `for s in tests/integration/providers/*.sh; do bash "$s"; done`
4. Document results in the PR description

## Security reports

Do **not** open public issues for vulnerabilities. See `SECURITY.md`
contacts (or contact maintainers directly until it exists); threat model is
in `docs/threat-model.md`.

## Licensing

By contributing you agree your contributions are licensed under the project
license (see `LICENSE`).
