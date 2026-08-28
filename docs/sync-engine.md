# Kestrel Sync Engine Design

Status: **v1.0 — normative for Phase 2** · Implements `requirements.md` §2 ·
Transport per ADR 0005/0010 · Supervision per ADR 0004.

---

## 1. State machine (normative)

One `SyncService` task per account; states are an enum, transitions emit
`AccountConnection` events (message-protocol §3).

```
            +-------------+   connect timeout 30s / 3 attempts
            v             |
 [Disconnected] ----> [Connecting] --TLS/TCP ok--> [Authenticating]
     ^  |                  |                            |
     |  | user GoOffline   | io/tls error               | rejected -> UserAction park
     |  v                  v                            v
 [OfflineMode]        [Disconnected]              [Authenticating]
                                                        | ok
                           +----------------------------+
                           v
                    [HierarchySync]  LIST/LSUB + NAMESPACE
                           |
                           v
                      [DeltaSync]  <----- restart here after IDLE wake
                        |    ^\
                        |    | \ IDLE unsupported/broken
                        |    |  \---> [PollLoop] (interval 2 min + jitter)
                        v    |
                     [Idle]  | DONE on wake/timeout (29 min < server 30 min)
                        |    |
                        +----+  wake -> DeltaSync -> Idle
```

Rules:

- Every state has a deadline (connect 30 s, auth 30 s, command 60 s, IDLE
  re-issue 29 min); expiry ⇒ `TransportError::ConnectionLost` ⇒ back to
  `Connecting` with supervisor backoff (250 ms ×2 ±20 %, cap 5 min).
- `GoOffline` parks in `OfflineMode` (no network); `GoOnline` resumes via
  `Connecting`. Queue mutations made offline are journaled (§6).
- `AccountConnection` is emitted on **every** transition (frontends render
  it; degradation is never silent).

## 2. Delta sync: cursor protocol

Per folder, cursors live in `cache.db folders` (`uid_validity`,
`highest_modseq`):

| Server capability | Discovery | Delta fetch | Vanished |
|-------------------|-----------|-------------|----------|
| QRESYNC | `SELECT` (QRESYNC) with known `uidvalidity`+`modseq` | `UID FETCH 1:* (FLAGS) (CHANGEDSINCE modseq)` | `VANISHED (EARLIER)` responses; also `UID SEARCH ...` fallback |
| CONDSTORE only | `SELECT` then `HIGHESTMODSEQ` | `CHANGEDSINCE` fetch as above | `UID SEARCH UID 1:*` diff vs known set (batched 1000-UID windows) |
| Neither | `SELECT` + `UIDNEXT`/`UIDVALIDITY` | `UID FETCH <last+1>:* ENVELOPE` for new; periodic full-window flag scan (`1:* FLAGS` in 1000-UID windows, rate-limited) | same UID-set diff |

- New/changed messages fetch `ENVELOPE + FLAGS + INTERNALDATE + BODYSTRUCTURE
  + BODY.PEEK[HEADER]`; raw bodies are **lazy** (§4).
- `MODSEQ` is stored per folder (`highest_modseq`); on `NOMODSEQ` fallback
  the cursor degrades to UID-window scans.
- Every fetch batch is committed in one storage transaction; cursors update
  in the same transaction (crash ⇒ redo from last committed cursor).

## 3. UIDVALIDITY reconciliation (requirements §2.2)

On `SELECT`, if reported `UIDVALIDITY != stored.uid_validity` (stored > 0):

1. Mark folder reconciling (in-task state), purge `messages`/`parts` rows for
   the folder (single transaction; blob CAS refs dropped ⇒ GC handles files
   after grace).
2. Reset `highest_modseq = 0`, store new `uid_validity`.
3. Full re-fetch (envelope-first), re-thread (threads live in `data.db`;
   dangling threads with zero messages are pruned lazily by GC), re-index
   via the pending-index cursor (`messages.indexed_at IS NULL`).
4. Emit `FolderTreeChanged` + `MessagesChanged { removed: n, changed: m }`
   so frontends drop stale UI state.

Detection is mandatory in **every** path that selects a folder (initial sync,
IDLE wake, restart revalidation per ADR 0004).

## 4. Fetch policy (lazy blobs)

- **Envelope-first:** metadata ingest (§2) never blocks on bodies.
- **Priority bodies:** the open-message path (`Command::GetMessage` with
  `BodyPreference`) triggers an immediate `UID FETCH ... BODY.PEEK[]` for the
  single message; background fill fetches recent bodies (newest-first,
  per-folder cap from config, default 200) when connected and idle.
- Raw bytes go to the blob CAS **before** `messages.raw_blob` is set, in the
  same storage transaction (no dangling references).
- Per-message size cap from parser limits (threat model §4.2); oversized
  bodies are listable, flagged, and fetchable on explicit user action only.

## 5. IDLE handling (RFC 2177)

- Enter IDLE after each DeltaSync pass when the server advertises `IDLE`.
- Wake conditions: unsolicited data (EXISTS/FLAGS/VANISHED), DONE timeout
  (29 min), or local wake signal (user command, outbox Sent-APPEND need,
  shutdown).
- After DONE: run DeltaSync for woken folders, then re-enter IDLE.
- Keepalive: TCP keepalive on; servers known to break IDLE
  (config `sync.idle_poll_only_hosts`) use PollLoop instead.
- PollLoop: jittered interval (default 120 s ± 20 %) reuses DeltaSync.

## 6. Outbox (requirements §2.2)

- `Command::ComposeSubmit` builds RFC 5322 (Markdown → `multipart/alternative`
  per requirements §5), writes the raw message to CAS + `outbox` row
  (`next_attempt_at = NULL`), emits `OutboxEnqueued`.
- Flush loop: due rows (`next_attempt_at IS NULL OR <= now`), ordered by
  `created_at`; attempt = SMTP submit (§7) via `kestrel-crypto` credentials.
  - Success ⇒ `sent_at` set, APPEND to Sent folder (UIDPLUS `APPENDUID`
    recorded), emit `MailSent`.
  - Transient failure (4xx/timeout/TLS) ⇒ `retry_count++`,
    `next_attempt_at = now + backoff(retry_count)`, emit `OutboxRetry`.
  - Permanent failure (5xx reject, bad recipient) ⇒ emit
    `MailFailed { permanent: true }`; row kept as draft.
- Backoff table: 30 s, 2 m, 8 m, 30 m, 2 h, then every 6 h (±20 % jitter),
  capped `retry_count` from config (default 12) ⇒ `RetryExhausted` (draft
  preserved).
- Offline mutations journal: flag/move/delete commands while offline are
    recorded in a `pending_ops` queue inside `data.db` and replayed
    FIFO on reconnect; conflicts resolved server-wins, re-synced, surfaced
    via `FlagsChanged`/`MessagesChanged`.

## 7. SMTP submission

- `lettre` async transport, rustls (TLS 1.3 default/1.2 min), submission
  port 465 (implicit TLS) or 587 (STARTTLS); AUTH via `kestrel-crypto`
  SASL callbacks (PLAIN/LOGIN/XOAUTH2).
- Bounce/`MAIL FROM` size limits honored; message built with generated
  Message-ID, Date from the injected clock, and DKIM-neutral headers
  (signing is Phase 5+ territory).

## 8. JMAP mapping (Phase 5)

- JMAP (`RFC 8620/8621`) shares the SyncService trait seam:
  `Email/state` + `Mailbox/state` tokens replace `(uidvalidity, modseq)`
  cursors; `Email/changes` replaces UID FETCH deltas; `Email/set` replaces
  STORE/APPEND.
- Account record `protocol = 'jmap'` selects the JMAP engine; IMAP-only
  features (IDLE) map to push (EventSource) with PollLoop fallback.

## 9. Test matrix

| # | Scenario | Fixture | Asserts |
|---|----------|---------|---------|
| 1 | Cold initial sync | Dovecot seeded 50 msgs | metadata + envelopes present; cursors set; `IndexProgress` to total |
| 2 | New mail while IDLE | inject via LMTP mid-IDLE | `MailArrived` within 5 s; envelope ingested |
| 3 | Flag change (CONDSTORE) | `doveadm flags add` | `FlagsChanged`; `highest_modseq` advances |
| 4 | Vanished (QRESYNC) | expunge + resync | `MessagesChanged { removed }`; rows purged |
| 5 | UIDVALIDITY bump | scripted mailbox recreate | purge + refetch; no duplicates; events emitted |
| 6 | Offline compose → online flush | Greenmail down/up | outbox survives restart; `MailSent`; Sent APPEND visible |
| 7 | Outbox permanent failure | Greenmail 5xx | `MailFailed { permanent }`; draft preserved |
| 8 | Offline flag ops replay | mutate while offline | journal replayed FIFO; server-wins conflict surfaced |
| 9 | Restart mid-sync (crash cut) | kill -9 between batches | resume from last committed cursor; no dup/partial rows |
| 10 | Capability fallback (no CONDSTORE) | Dovecot config without QRESYNC | windowed scans; correctness equal |
| 11 | Token refresh (OAuth2) | expiry clock advance | unattended refresh; no credential material in logs |
