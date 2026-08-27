# ADR 0003: Use `sqlx` for SQLite Access with Compile-Time Checked Queries

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §3.1 requires SQLite with WAL journaling, NORMAL sync, and
foreign keys ON, hosting the metadata for hundreds of thousands of messages
with a > 1,500 msgs/sec ingestion SLA and < 15 ms search-faceting lookups. All
core code is async (tokio, requirements §1.1), and disk I/O must never block UI
threads. We need schema migrations for a long-lived on-disk format.

## Decision

We use **`sqlx`** with the **SQLite** driver in async mode:

- All queries are written inline and verified at **compile time** via
  `sqlx::query!` / `query_as!` against a checked-in migration state
  (`sqlx migrate`); `DATABASE_URL`/offline `.sqlx` query metadata is committed
  so CI and agents build without a live database.
- Migrations are forward-only, append-only SQL files under
  `kestrel-storage/migrations/`, applied at startup under an advisory lock.
- Connection setup enforces the mandated pragmas (`journal_mode = WAL`,
  `synchronous = NORMAL`, `foreign_keys = ON`) plus `busy_timeout`.
- A single write connection (SQLite has one writer) pooled behind a dedicated
  storage task (see ADR 0004); reads use a small bounded pool opened on the
  same WAL file.

## Consequences

- **Correctness gate:** SQL typos/schema drift fail the build, not the user's
  mailbox at runtime. This is the primary rationale.
- **Async-native:** no `spawn_blocking` shims hand-rolled around a sync driver;
  sqlx already runs SQLite on a blocking pool internally and integrates with
  tokio cancellation.
- **Migrations included:** one tool fewer (no `refinery`/`diesel_migrations`).
- **Cost:** sqlx's SQLite implementation multiplexes over `libsqlite3-sys`;
  we must size the pool and hold the single-writer discipline ourselves. Write
  throughput relies on batched transactions from the ingestion pipeline
  (documented in `docs/schema.md`).
- **Offline metadata churn:** `.sqlx` files must be regenerated
  (`cargo sqlx prepare`) whenever queries change; CI enforces freshness.

## Alternatives Considered

- **`rusqlite` + hand-rolled async wrapper** — excellent control, but
  compile-time query checking is lost and every blocking hazard becomes our
  own bug to write.
- **Diesel** — mature, type-safe schema DSL, but sync-first; async usage
  requires the same blocking-pool shims with a heavier macro DSL.
- **redb / sled / redb-native stores** — not SQLite; requirements mandate
  SQLite explicitly.
- **SeaORM** — an extra entity layer over sqlx with runtime cost and leakier
  abstractions; unnecessary for our bounded schema.
