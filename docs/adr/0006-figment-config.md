# ADR 0006: Use `figment` for Configuration with `notify`-Driven Hot Reload

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §7 mandates XDG base directories, human-readable TOML at
`$XDG_CONFIG_HOME/kestrel/config.toml`, and live reload "where feasible" for
keybindings, themes, sync intervals, and accounts. Config must layer defaults,
system, user, and (later) profile values without surprise.

## Decision

We use **`figment`** (with the `toml` and `env` providers) to load a strongly
typed `Config` struct in `kestrel-core`:

- Provider order: compiled-in defaults → `$XDG_CONFIG_HOME/kestrel/config.toml`
  → environment overrides (`KESTREL___SECTION__KEY`, figment `env` style,
  used mainly for tests and CI).
- The struct is **versioned and validated at load** (unknown keys warn;
  invalid values fail fast with precise TOML path spans via figment errors).
- Hot reload: a `notify` file-watcher task re-loads and validates the file,
  then publishes a `ConfigUpdated` snapshot on the event bus. Consumers apply
  reloadable fields at defined safepoints; non-reloadable fields (e.g., paths,
  account identity) require restart and are reported as such.
- A `Config` snapshot is an immutable `Arc<Config>` handed to services — no
  locks on the read path.

## Consequences

- Layered config with provenance and good error messages out of the box.
- Strong typing: a typo in the config file fails validation at startup, not at
  first use in a service.
- **Cost:** figment's API has a learning curve and its error types need
  careful formatting for user-facing messages; we wrap that once in
  `kestrel-core::config`.
- Hot reload correctness burden is ours: every consumer must declare whether
  it honors `ConfigUpdated` (documented per service in
  `docs/message-protocol.md`).

## Alternatives Considered

- **`config` crate** — similar layering; weaker error spans and less
  ergonomic provider composition; historically slower releases.
- **`serde` + `toml` only** — simplest, but no layering/env override and we
  would hand-roll provenance and validation errors.
- **`configparser`/hand-parsed TOML** — rejected: loses schema typing.
