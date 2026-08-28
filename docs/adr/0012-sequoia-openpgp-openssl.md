# ADR 0012: Sequoia OpenPGP with OpenSSL Backend and LGPL Accommodation

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §9 (Phase 5) mandates OpenPGP sign/encrypt via Sequoia.
`sequoia-openpgp` is LGPL-2.0-or-later, while Kestrel is Apache-2.0 and
`docs/engineering-standards.md` §6 flags copyleft dependencies. Separately,
Sequoia 1.x carries RUSTSEC-2025-0136 (attacker-triggered panic via malformed
PKESK/SKESK — a direct violation of our no-panic-on-hostile-input rule,
threat model §4.2), fixed in 2.x. Backend choice: `crypto-nettle` (default;
requires system nettle with pgp headers), `crypto-openssl` (production-ready,
widely packaged), or `crypto-rust` (marked experimental upstream).

## Decision

1. We use **`sequoia-openpgp` 2.x** with the **`crypto-openssl`** backend and
   `compression-deflate`.
2. The LGPL-2.0-or-later obligation is accepted and recorded: Kestrel is
   distributed as free/open-source software, satisfying LGPL combined-work
   terms (source availability enables relinking/recompilation of the LGPL
   part). `deny.toml` carries a crate-scoped license exception referencing
   this ADR; any move to proprietary distribution requires superseding this
   ADR.

## Consequences

- No panic-on-malformed-PKESK class of bugs from the 1.x line; Sequoia 2.x
  API used throughout `kestrel-crypto::openpgp`.
- Build requires OpenSSL headers (`libssl-dev`) — already required elsewhere
  in the GUI/webview toolchain; CI installs it.
- License review gate stays green with a scoped, ADR-referenced exception.

## Alternatives Considered

- **Sequoia 1.x + crypto-nettle** — vulnerable line (RUSTSEC-2025-0136) and
  required nettle headers with `nettle/pgp.h` that current distro nettle
  packages no longer ship.
- **`crypto-rust`** — upstream-marked experimental; incompatible with our
  production-crypto bar.
- **`rpgp`** — pure-Rust and permissively licensed, but thinner feature set
  for signing/encryption interop at Phase 5's scope; requirements name
  Sequoia.
