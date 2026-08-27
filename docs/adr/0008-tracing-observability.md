# ADR 0008: Observability — `tracing` with Structured, Level-Filtered Logging

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

A distributed-feeling async system (sync engines, supervisors, storage
pipelines) with hard SLAs is undebuggable without structured telemetry. Logs
must correlate a single message's journey (IMAP fetch → parse → SQLite →
Tantivy → UI event) across tasks and threads, in production binaries and in
CI test harnesses, without measurable cost when disabled. Some log content
(headers, addresses) is privacy-sensitive.

## Decision

We standardize on the **`tracing`** ecosystem:

- `tracing` + `tracing-subscriber` with `EnvFilter` (default level `info`;
  `kestrel_<crate>=<level>` per-crate control; config-file overrides via
  ADR 0006).
- Every service creates a named span tree (`account=%id`, `folder`, `uid`); the
  message-protocol crate emits span context on event-bus messages so UI-origin
  actions correlate with sync/storage activity.
- Output formats: pretty (terminal default), JSON (`RUST_LOG_FORMAT=json`,
  for field extraction/OTLP export later — Phase 5 hook point).
- **Privacy by default:** no message bodies, subjects, addresses, or tokens at
  `info`; such fields only at `trace` behind an explicit opt-in flag
  documented in the privacy section of the threat model. IDs (hashed
  account/folder/uid) are the default correlation keys.
- Metrics hooks: counters/histograms are emitted as events now; wiring to a
  `metrics`-facade crate is additive later without code churn.

## Consequences

- One idiomatic stack, zero-cost when the subscriber filters out a level.
- Correlated multi-service traces become greppable (`account=a1 uid=42`).
- **Cost:** spans must be maintained as a convention (PR checklist); we accept
  the discipline in exchange for SLA debuggability.

## Alternatives Considered

- **`log` facade only** — no spans/correlation; insufficient for async
  pipelines.
- **`slog`** — structured-first with good ergonomics; smaller ecosystem, and
  the industry/tooling center of gravity is `tracing`.
- **OpenTelemetry-first (direct)** — premature complexity; the event-bus +
  JSON format keeps the door open without the dependency weight today.
