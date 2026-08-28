# ADR 0009: Split SQLite into Rebuildable `cache.db` and Durable `data.db`

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §3.1/§7 place SQLite at `$XDG_CACHE_HOME`, but the `outbox`
(unsent mail) and account records are not re-fetchable from any server: a
cache wipe would destroy unsent user work — the one data class a mail client
must never lose. `docs/schema.md` §1 flagged this as an open issue requiring
an ADR before Phase 1 storage lands.

## Decision

We maintain **two SQLite databases** under the mandated pragmas (WAL,
`synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout`):

- `$XDG_CACHE_HOME/kestrel/cache.db` — syncable metadata (`folders`,
  `messages`, `parts`, `blobs` registry). **Rebuildable**: it may be wiped and
  fully reconstructed by a resync. Breaking migrations may drop + resync it.
- `$XDG_DATA_HOME/kestrel/data.db` — durable records (`accounts`, `threads`,
  `outbox`, `settings`, sync-role mapping). **Never wiped** by migrations or
  recovery; destructive change requires its own ADR and an export/import path.

Cross-database references (`folders.account_id`, `messages.thread_id`) are
enforced in `StorageService` code — SQLite cannot express FKs across files;
violations are `Bug`-class errors (ADR 0007).

## Consequences

- `XDG_CACHE_HOME` wipe loses only re-fetchable data; unsent mail survives
  (`outbox.raw_inline` belt-and-suspenders copy under 64 KiB per
  `docs/schema.md`).
- Two migration directories and two migrators (`kestrel-storage/migrations/{cache,data}`);
  both are sqlx-migrated with committed offline metadata.
- StorageService owns both write connections (single-writer rule per file);
  read pools are per-database.
- The blob CAS remains durable under `$XDG_DATA_HOME` and is referenced by
  both databases; its registry table lives in `cache.db` so a wipe correctly
  orphans blobs for GC sweep rather than losing durable data.

## Alternatives Considered

- **Single database at `XDG_CACHE_HOME`** — outbox and accounts lost on cache
  clear; violates offline-first durability expectations.
- **Single database at `XDG_DATA_HOME`** — safe but ignores the spec's cache
  placement; loses the free "wipe and resync" recovery valve for the large,
  volatile metadata set.
- **SQLite ATTACH with one file per concern** — same two-file outcome with
  extra complexity; cross-file FKs are still impossible.
