# Kestrel Sync Engine Design

Status: **Planned — must be completed before Phase 2 milestone exit**
(`docs/roadmap.md`). Inputs: `requirements.md` §2, ADR 0005, ADR 0004,
`docs/message-protocol.md`.

## Required contents (outline)

1. **State machine (normative):** Disconnected → Connecting/Handshake →
   Authenticating → Hierarchy Sync → Delta Sync → IDLE loop; transitions,
   timers, and event emissions per state (`AccountConnection` events).
2. **Delta sync:** `HIGHESTMODSEQ`/`UIDVALIDITY` cursor protocol; QRESYNC
   `VANISHED` handling; CONDSTORE-only and no-extension fallback matrix
   (full UID range scans, resync windows).
3. **UIDVALIDITY reconciliation:** detection, folder purge, re-thread/
   re-index pipeline, user-visible effects (`MessagesChanged`).
4. **Fetch policy:** envelope-first, body/blobs lazy + prioritized
   (open message → immediate; background fill by recency), per-folder caps.
5. **IDLE handling:** RFC 2177 wake processing, keepalive/timeout policy,
   polling fallback for servers with broken IDLE.
6. **Outbox:** queue semantics, exponential backoff + jitter table, final
   failure policy (requirements §2.2), Sent APPEND handling.
7. **JMAP mapping (Phase 5):** state tokens ↔ sync cursors.
8. **Test matrix:** scenario table mapped to Dovecot/Greenmail fixtures and
   recorded-session fuzzing.
