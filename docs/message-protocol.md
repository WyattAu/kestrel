# Kestrel Core Message Protocol

Status: **v1.0 — binding contract** · Implements `requirements.md` §1.1 ·
Concurrency pattern per ADR 0004.

This document is the frozen interface between the core engine and every
frontend (`kestrel-tui`, `kestrel-gui`). Any change is an ADR-level change
(ADR 0000) and must update this document in the same PR.

---

## 1. Channel Topology

```
Frontend                          Core Engine
────────                          ───────────
CommandSender ─── mpsc(bounded) ──▶ EngineCommandRouter ──▶ target service
Reply (oneshot) ◀────────────────────────────┘
EventReceiver ── broadcast ◀────── EventBus (all services publish)
```

- **Commands:** `tokio::sync::mpsc::Sender<Command>` — bounded (capacity 256,
  configurable). One per frontend.
- **Replies:** request-style commands carry a `oneshot::Sender<Reply>`;
  fire-and-forget commands do not.
- **Events:** `tokio::sync::broadcast::Sender<EngineEvent>` — capacity 1024;
  each frontend holds a cloned receiver. Slow consumers receive
  `EventStreamLagged(n)` and resync via `Command::ResyncState`.

## 2. Command Envelope

```rust
pub struct Command {
    pub id: RequestId,          // uuid v7, monotonic; echoed in events
    pub origin: FrontendKind,   // Tui | Gui
    pub payload: CommandPayload,
}

pub enum CommandPayload {
    // Mailbox navigation & reads
    ListAccounts { reply: oneshot::Sender<Reply> },
    ListFolders { account: AccountId, reply: oneshot::Sender<Reply> },
    ListMessages { folder: FolderId, window: Window, sort: SortSpec,
                   reply: oneshot::Sender<Reply> },
    GetMessage { message: MessageId, body: BodyPreference,
                 reply: oneshot::Sender<Reply> },
    Search { query: SearchQuery, reply: oneshot::Sender<Reply> },

    // State mutations (flag changes flow through sync engine)
    SetFlags { messages: Vec<MessageId>, flags: FlagOp, reply: oneshot::Sender<Reply> },
    MoveMessages { messages: Vec<MessageId>, to: FolderId, reply: oneshot::Sender<Reply> },
    DeleteMessages { messages: Vec<MessageId>, expunge: bool, reply: oneshot::Sender<Reply> },

    // Composition
    ComposeSubmit { draft: Draft, reply: oneshot::Sender<Reply> },
    CancelOutbox { id: OutboxId, reply: oneshot::Sender<Reply> },

    // Sync control
    TriggerSync { account: AccountId, kind: SyncKind },   // fire-and-forget
    GoOffline, GoOnline,
    ResyncState { reply: oneshot::Sender<Reply> },        // after lag

    // Config & lifecycle
    ConfigUpdated { snapshot: Arc<Config> },              // from ConfigWatcher
    Shutdown { drain: bool },
}

pub enum Reply {
    Accounts(Vec<AccountSummary>),
    Folders(Vec<FolderSummary>),
    Messages(MessagePage),          // windowed page + total count
    Message(MessageView),           // metadata + resolved body parts
    SearchResults(Vec<SearchHit>),
    Accepted,                       // queued/applied; follow-up events will follow
    Err(ServiceError),              // ADR 0007 taxonomy payload
}
```

## 3. Event Envelope

```rust
pub enum EngineEvent {
    // Lifecycle
    EngineStarted { accounts: Vec<AccountSummary> },
    ServiceDegraded { service: ServiceId, error: ServiceError, restart_in: Duration },
    EngineShutdownProgress { stage: ShutdownStage },

    // Connectivity
    AccountConnection { account: AccountId, state: ConnectionState },
    // Disconnected | Connecting | Authenticating | Syncing | Idle | OfflineMode

    // Mailbox changes
    MailArrived { account: AccountId, folder: FolderId, summary: FolderDelta },
    MessagesChanged { folder: FolderId, changed: u32, removed: u32 },
    FlagsChanged { messages: Vec<MessageId> },
    FolderTreeChanged { account: AccountId },

    // Composition
    OutboxEnqueued { id: OutboxId },
    OutboxRetry { id: OutboxId, attempt: u32, next_in: Duration, last_error: String },
    MailSent { id: OutboxId, message: MessageId },
    MailFailed { id: OutboxId, error: ServiceError, permanent: bool },

    // Index & search
    IndexProgress { account: AccountId, indexed: u64, total: u64 },

    // Security
    RemoteContentBlocked { message: MessageId, count: u32 },
    SuspiciousLink { message: MessageId, href: String },  // punycode/homograph

    // Protocol upkeep
    ConfigUpdated { snapshot: Arc<Config> },
    EventStreamLagged { missed: u64 },
}
```

## 4. Backpressure & Overflow Policy

| Channel | Capacity | On full | Rationale |
|---------|----------|---------|-----------|
| Command mpsc | 256 | `try_send` fails → engine emits `ServiceDegraded(Busy)`; frontend must not spin-retry | UI threads never block on send |
| per-service inbound mpsc | service-specific (16–128) | Router drops nothing: commands are forwarded with `await` on the router task only; a full service queue backpressures the router, which then applies the Busy policy above | Bounded memory under burst |
| Event broadcast | 1024 | lagged receivers get `EventStreamLagged` → resync via `Command::ResyncState` | Events are lossy by design; state must be re-fetchable |

Rules:

1. Frontends MUST treat events as **hints**, never as the source of truth;
   authoritative state comes from command replies. (Stateless-render pattern.)
2. Reply oneshots MUST be answered exactly once, including on cancellation;
   the default is `Reply::Err(ServiceError::Cancelled)`.
3. Commands are processed per-service in FIFO order; no command may be
   partially applied — services commit atomically per command batch.

## 5. Service Supervision Semantics (ADR 0004 summary)

- Each service: bounded inbox + main loop + `CancellationToken`.
- Panic containment: `tokio::spawn` + `JoinHandle::await` wrapper in the
  supervisor; restart backoff = exponential (250 ms base, ×2, jitter ±20%,
  cap 5 min), reset after 5 min healthy.
- On restart, a service MUST revalidate its persisted state before resuming
  (e.g., SyncService performs `UIDVALIDITY` reconciliation, requirements
  §2.2).
- `ServiceDegraded` events are user-visible in both frontends (status bar /
  notification) — degradation is never silent.

## 6. UI Integration Contracts

- **TUI:** the ratatui loop owns the terminal; commands are sent from input
  handlers; a dedicated task forwards broadcast events into the TUI's
  channel; `$EDITOR` composition suspends the loop (raw mode off), then
  submits `ComposeSubmit`.
- **GUI:** Slint callbacks send commands; an event-forwarder task invokes UI
  updates on the Slint thread via `slint::invoke_from_event_loop`; the wry
  viewport never receives engine objects, only serialized body payloads over
  the custom `cid:` protocol (threat model §4).
- Both frontends MUST implement `EventStreamLagged` recovery and MUST render
  `ServiceDegraded`.

## 7. Versioning

- The protocol version lives in `kestrel-core::protocol::PROTOCOL_VERSION`
  and is emitted in `EngineStarted`.
- Additive changes (new enum variants with a `__NonExhaustive`-style fallback
  in frontends) are minor; anything else is major and requires an ADR.
- Frontends must tolerate unknown event variants (forward compatibility
  within a major version).
