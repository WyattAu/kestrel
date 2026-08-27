# Kestrel Error Taxonomy

Status: **v1.0 skeleton — extends as Phase 1–2 crates land** · Governs ADR
0007. Every error variant in every crate maps to exactly one recovery class;
UI wording lives in frontends, never in `Display` strings.

---

## 1. Recovery classes

| Class | Meaning | Engine behavior | UI behavior |
|-------|---------|-----------------|-------------|
| `Retryable` | Transient; retry with backoff is safe | Supervisor/outbox/backoff applies policy | Status line / passive notification |
| `UserAction` | Requires user input (auth, certificate, quota) | Service parks in degraded state + event | Blocking prompt / dialog |
| `Permanent` | Will not succeed by retrying (bad request, not found) | No retry; logged once | Inline failure state |
| `Bug` | Invariant violated; unrecoverable state | Contained panic of owning service (ADR 0004) + report | Restart notice; issue prompt with logs |

## 2. Top-level kinds (`kestrel-core::KestrelError`)

| Domain enum | Crate | Example variants → class |
|-------------|-------|--------------------------|
| `ConfigError` | core | `InvalidToml{path, span}` → UserAction; `UnknownKey` → UserAction (warn) |
| `ProtocolError` | core (message protocol) | `Busy` → Retryable; `Cancelled` → Permanent; `MalformedCommand` → Bug |
| `AuthError` | crypto | `CredentialsRejected` → UserAction; `OAuthRefreshFailed` → UserAction; `KeyringUnavailable` → UserAction |
| `TransportError` | sync | `TlsHandshake` → Retryable; `ConnectionLost` → Retryable; `CapabilityMissing{cap}` → Permanent |
| `ImapError` | sync | `UidValidityChanged{folder}` → special (triggers reconciliation, requirements §2.2); `FetchAborted` → Retryable |
| `SmtpError` | sync | `RelayRefused` → Permanent; `Transient4xx` → Retryable; `MessageRejected` → Permanent |
| `StorageError` | storage | `DbCorrupt{db}` → UserAction (rebuild prompt); `MigrationFailed` → UserAction; `BlobMissing{hash}` → Permanent (re-fetch) |
| `IndexError` | storage | `Corrupt` → Retryable (rebuild); `CommitFailed` → Retryable |
| `ParseError` | core (MimeParser, ADR 0002) | `Malformed{detail}` → Permanent (degraded message view); `Limit{kind}` → Permanent (threat model §4.2) |
| `CryptoError` | crypto | `OpenPgpUnsupported` → Permanent (Phase 5); `SigningFailed` → Retryable |
| `OutboxError` | sync | `RetryExhausted{n}` → Permanent (draft preserved); `DraftInvalid` → UserAction |

## 3. Rules

1. Variants are added only with: recovery class, owning service, example
   trigger, and (if user-visible) a UI message key. Update this file in the
   same PR.
2. `Display` strings are stable identifiers for tests; user wording is a
   frontend table keyed by variant.
3. `source` chains preserve the underlying error (`#[from]`); no flattening
   to strings across crate boundaries.
4. Frontends must handle `KestrelError` exhaustively — new variants are
   compile errors in both UIs (that is the point).
