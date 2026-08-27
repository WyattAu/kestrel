# ADR 0004: Typed Service/Actor Pattern on tokio Channels (No Actor Framework)

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §1.1 mandates tokio (multi-threaded), typed async
message-passing between the core engine and frontends, and a hard
non-blocking guarantee for UI threads. The sync engine is inherently
stateful (connection state machines, IDLE loops, outbox backoff), calling for
actor-like isolation. We must choose between an external actor framework, an
in-house pattern, or shared-state-with-locks.

## Decision

We implement an **in-house typed service/actor pattern** on `tokio::sync`
primitives, specified fully in `docs/message-protocol.md`:

- Each long-running unit (SyncService per account, StorageService,
  IndexService, OutboxService, CredentialService, SearchService) is a **task**
  owning its state; no `Arc<Mutex<...>>` shared-state services.
- Commands arrive on **bounded `mpsc`** channels; replies use **`oneshot`**;
  domain events fan out on a **`tokio::sync::broadcast`** bus.
- Every service implements a common `Service` trait with uniform startup,
  graceful shutdown (CancellationToken), and panic containment (task supervisor
  restarts a crashed service with typed backoff and emits a `ServiceDegraded`
  event).
- Frontends are pure clients of this protocol — they hold senders and an event
  subscription, nothing more.

## Consequences

- **KISS / zero extra dependencies:** the pattern is ~200 lines of supervision
  glue; behavior is fully debuggable with plain tokio tooling.
- **Deterministic shutdown:** cancellation tokens + drain ordering give
  ordered, non-data-losing shutdown (outbox flushed last), which an opaque
  framework would hide.
- **Backpressure is explicit:** bounded channels force documented overflow
  policy (see message-protocol) instead of unbounded queues eating the 25 MB
  TUI memory budget.
- **Cost:** we own supervision/restart logic that frameworks (ractor, xtra)
  provide; accepted because our supervision needs are simple (per-service
  restart + backoff) and keeping it in-tree keeps the concurrency model
  legible to every contributor.
- Rule of thumb codified: **locks guard small leaf data structures (caches,
  config snapshots); services own business state.**

## Alternatives Considered

- **`ractor`** — good actor library with supervision trees; rejected: adds a
  dependency and its own typing/serialization idioms for needs we can meet
  with channels; its restart semantics would still need our domain-level
  rehydration logic.
- **`xtra`** — similar trade-offs; lighter but less active.
- **Shared state (`Arc<RwLock<EngineState>>`)** — rejected: invites blocking
  UI threads, deadlocks under load, and untraceable state transitions; the
  single-writer-per-service actor pattern makes every state change auditable.
- **Message broker / IPC (e.g., postcard over UDS)** — unnecessary; both
  frontends run in-process. Revisit only if we split frontends into separate
  OS processes.
