# ADR 0011: `kestrel-engine` Assembly Crate

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`docs/architecture.md` §2 binds frontends to depend **only on `kestrel-core`**,
while §7 requires each frontend binary to spawn the full engine (which is
assembled from `kestrel-sync`, `kestrel-storage`, and `kestrel-crypto`
services) in-process. Without an owner for assembly, each frontend would
re-implement service wiring and gain direct crate edges to every core crate —
violating the spirit of the dependency rule and duplicating supervision glue.

## Decision

We add a seventh crate, **`kestrel-engine`**, which is the sole place that
depends on all four core-side crates. It exposes:

- `Engine::spawn(config, paths, clock) -> (CommandSender, EventBusHandle)`
  plus supervision lifecycle (ordered shutdown per architecture §3.3),
- the ADR 0004 supervisor, command router, and event-bus plumbing.

Frontend **binaries** depend on `kestrel-engine` (to spawn it in `main`) and
on `kestrel-core` (for all types). Frontend **code beyond `main`** uses only
`kestrel-core` protocol types; this preserves the binding rule's intent —
no frontend module may touch sync/storage/crypto APIs.

## Consequences

- One owner for service wiring, restart policy, and shutdown ordering;
  frontends stay thin.
- The architecture table gains a row; this ADR is that amendment.
- `kestrel-engine` is deliberately free of domain logic — it only composes;
  moving a responsibility there is a review flag.

## Alternatives Considered

- **Assembly inside `kestrel-core`** — would force core to depend on the
  concrete engine crates, inverting the dependency rule (`kestrel-core` is
  the vocabulary, not the engine).
- **Each frontend assembles its own engine** — duplicates supervision and
  gives both frontends edges to all core crates; two divergence risks.
- **Trait-object registry in core** — runtime plugin machinery for a
  compile-time-known set of services; unnecessary complexity.
