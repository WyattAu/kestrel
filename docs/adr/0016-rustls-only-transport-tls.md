# ADR 0016: rustls as the Sole Transport-TLS Backend

- **Status:** Accepted
- **Date:** 2026-09-08
- **Deciders:** Kestrel team
- **Supersedes:** the TLS half of the premise recorded against issue #18
  ("imap-next is native-tls-only") — see Context.

## Context

The original architecture audit flagged IMAP as the one transport still on
native-tls in an otherwise rustls codebase, costing us vendored OpenSSL on
Android and musl fuzz friction. Re-verification against the tree shows that
criticism is **already stale**:

- IMAP transport TLS is implemented by us, not `imap-next`: `imap-next` is
  sans-I/O (ADR 0005/0010), and `kestrel-sync::session` upgrades connections
  with `tokio_rustls::TlsConnector` + `rustls_pki_types::ServerName`.
- SMTP is rustls via `lettre`'s `tokio1-rustls-tls` feature.
- sqlx (storage) is `tls-rustls`; reqwest call sites declare
  `default-features = false, features = ["rustls-tls"]`.

However, `openssl-sys` is still in every workspace tree, via exactly two
paths:

1. **Deliberate:** `sequoia-openpgp` with the `crypto-openssl` backend
   (ADR 0012) — OpenSSL as a *crypto primitive* provider, not transport.
2. **Forced workaround:** `mailkit` 0.2.0 ships an ungated `ResendProvider`
   that references `reqwest`, so `kestrel-core` must enable mailkit's
   `resend` feature, whose reqwest dependency carries **default features**
   (i.e. `default-tls` → native-tls). Documented in the workspace
   `Cargo.toml`.

Path 2 has a live consequence beyond bloat: cargo unifies features, so
reqwest is compiled with **both** TLS backends enabled — and when both are
present, reqwest's default connector is **native-tls**. Every
`reqwest::Client::new()` call site (JMAP sync, CalDAV/CardDAV clients,
OAuth token exchanges) therefore runs OpenSSL at runtime today, despite the
rustls-only declarations. The declarations are currently inert.

## Decision

1. **rustls is the sole permitted transport-TLS backend** for all network
   I/O: IMAP (incl. STARTTLS upgrade), SMTP submission, JMAP, CalDAV /
   CardDAV, and OAuth token endpoints. TLS 1.3 preferred, 1.2 minimum
   (matching the `lettre` posture in `docs/sync-engine.md`).
2. **OpenSSL in the dependency tree is restricted to two documented
   exceptions**, neither of which terminates a transport connection:
   - Sequoia's `crypto-openssl` primitive backend (ADR 0012);
   - mailkit's transitive reqwest, until upstream gates `ResendProvider`
     properly — at which point the `resend` feature is dropped and this
     exception expires (tracked in the issue backlog).
3. **Load-bearing pins:** every `reqwest::Client` construction in workspace
   code must call `.use_rustls_tls()`. `reqwest::Client::new()` is banned in
   workspace source (it silently selects native-tls under unification).
   Enforced by `scripts/check-tls-backend.sh` in CI.
4. Frontends never construct HTTP clients (ADR dependency rules); transport
   TLS lives in `kestrel-sync` / `kestrel-crypto` / `kestrel-calcard` only.

## Consequences

- Runtime TLS for all mail/HTTP traffic is rustls+ring, one credential and
  verification stack to audit; native-tls/openssl behavior differences
  (verification flags, proxy env handling) disappear from the transport
  path.
- The Android vendored-OpenSSL build (issue #18) exists **only** because of
  Sequoia's `crypto-openssl`, not imap-next — the stale comment in
  `kestrel-mobile/Cargo.toml` is corrected by this ADR. If a future Sequoia
  release makes `crypto-rust` production-grade (or an equivalent pure-Rust
  backend), dropping `crypto-openssl` would remove the last openssl-sys
  path and with it the vendored build; that is a superseding decision, not
  work done silently.
- oauth-toolkit 0.2.0 constructs its own internal `reqwest::Client::new()`
  in a few non-injectable spots; under unification those are native-tls
  too. Our call sites pass an explicit rustls client wherever the API
  accepts one; the residual internal sites are accepted under exception 2's
  spirit and recorded here. (Upstream fix worth requesting.)
- `scripts/check-tls-backend.sh` fails CI on any `Client::new()` in
  workspace source or any `Client::builder()` whose file lacks
  `use_rustls_tls`.

## Alternatives Considered

- **Accept native-tls as a co-equal backend** — two TLS stacks to audit,
  divergent verification behavior, and the exact class of silent
  feature-unification surprises this ADR closes. Rejected.
- **Ban openssl-sys from the tree entirely (deny-style gate)** — impossible
  while ADR 0012 stands (Sequoia `crypto-openssl`) and mailkit is unfixed;
  a gate that is red today is a gate nobody trusts. Revisited when the
  mailkit exception expires.
- **Migrate off mailkit** — it carries the threading implementation
  (`docs/schema.md` §3.4); replacing it to fix a TLS feature leak is
  disproportionate while the workaround is one feature flag.

## References

- ADR 0005 (IMAP flow), ADR 0010 (imap-next from crates.io)
- ADR 0012 (Sequoia crypto-openssl — the primitive-backend exception)
- `docs/sync-engine.md` (TLS postures per protocol)
- `docs/threat-model.md` §4 (transport security posture)
