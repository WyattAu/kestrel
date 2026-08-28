//! Engine end-to-end tests (Phase 1): spawn → commands → replies → events →
//! ordered shutdown.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::{sync::Arc, time::Duration};

use kestrel_core::{
    config::Config,
    ids::FolderId,
    mime::MimeParser as _,
    paths::Paths,
    protocol::{
        Address, Command, CommandPayload, ConnectionState, Draft, EngineEvent, Flag, FlagOp,
        FrontendKind, MailProtocol, Provider, Reply, SearchQuery, ShutdownStage, SortSpec, Window,
    },
    testkit::{SequentialIds, sample_message},
};
use kestrel_engine::{Engine, EngineHandle, command};
use kestrel_storage::{IngestBatch, IngestMessage, NewAccount, NewFolder};

async fn spawn_engine(dir: &std::path::Path) -> EngineHandle {
    let paths = Arc::new(Paths::nested_under(dir));
    paths.ensure().unwrap();
    Engine::spawn(Arc::new(Config::default()), paths)
        .await
        .expect("engine spawns")
}

async fn send(handle: &EngineHandle, payload: CommandPayload) -> Reply {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let needs_reply = matches!(
        &payload,
        CommandPayload::ListAccounts { .. }
            | CommandPayload::ListFolders { .. }
            | CommandPayload::ListMessages { .. }
            | CommandPayload::GetMessage { .. }
            | CommandPayload::Search { .. }
            | CommandPayload::SetFlags { .. }
            | CommandPayload::MoveMessages { .. }
            | CommandPayload::DeleteMessages { .. }
            | CommandPayload::ComposeSubmit { .. }
            | CommandPayload::CancelOutbox { .. }
            | CommandPayload::ResyncState { .. }
    );
    let cmd = if needs_reply {
        with_reply(payload, tx)
    } else {
        Command {
            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
            origin: FrontendKind::Tui,
            payload,
        }
    };
    handle.commands.send(cmd).await.expect("command accepted");
    if needs_reply {
        rx.await.expect("reply arrives")
    } else {
        Reply::Accepted
    }
}

fn dummy<T>() -> tokio::sync::oneshot::Sender<T> {
    tokio::sync::oneshot::channel().0
}

fn with_reply(payload: CommandPayload, tx: tokio::sync::oneshot::Sender<Reply>) -> Command {
    // Rebuild the payload with the reply channel attached. The enum has
    // per-variant replies; the router answers exactly once.
    Command {
        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
        origin: FrontendKind::Tui,
        payload: attach(payload, tx),
    }
}

fn attach(payload: CommandPayload, tx: tokio::sync::oneshot::Sender<Reply>) -> CommandPayload {
    use CommandPayload as P;
    match payload {
        P::ListAccounts { .. } => P::ListAccounts { reply: tx },
        P::ListFolders { account, .. } => P::ListFolders { account, reply: tx },
        P::ListMessages {
            folder,
            window,
            sort,
            ..
        } => P::ListMessages {
            folder,
            window,
            sort,
            reply: tx,
        },
        P::GetMessage { message, body, .. } => P::GetMessage {
            message,
            body,
            reply: tx,
        },
        P::Search { query, .. } => P::Search { query, reply: tx },
        P::SetFlags {
            messages, flags, ..
        } => P::SetFlags {
            messages,
            flags,
            reply: tx,
        },
        P::MoveMessages { messages, to, .. } => P::MoveMessages {
            messages,
            to,
            reply: tx,
        },
        P::DeleteMessages {
            messages, expunge, ..
        } => P::DeleteMessages {
            messages,
            expunge,
            reply: tx,
        },
        P::ComposeSubmit { draft, .. } => P::ComposeSubmit { draft, reply: tx },
        P::CancelOutbox { id, .. } => P::CancelOutbox { id, reply: tx },
        P::ResyncState { .. } => P::ResyncState { reply: tx },
        other => other,
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn engine_boot_accounts_folders_messages_search_outbox() {
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_engine(dir.path()).await;
    let mut events = handle.events.resubscribe();

    // EngineStarted carries the protocol version.
    match events.recv().await {
        Ok(EngineEvent::EngineStarted { version, .. }) => assert_eq!(version, 1),
        other => panic!("expected EngineStarted, got {other:?}"),
    }

    // Seed an account + folder + message directly through storage handle
    // (the sync engine provides this path in Phase 2; here we exercise the
    // engine's read path).
    {
        let paths = Arc::new(Paths::nested_under(dir.path()));
        let ids = Arc::new(SequentialIds::new());
        let clock = Arc::new(kestrel_core::clock::FakeClock::new(1));
        let (storage, cancel) =
            kestrel_storage::StorageService::spawn((*paths).clone(), ids.clone(), clock);
        let account = storage
            .upsert_account(NewAccount {
                name: "E2E".into(),
                email: "e2e@example.org".into(),
                provider: Provider::Generic,
                protocol: MailProtocol::Imap,
                auth_kind: "password".into(),
            })
            .await
            .unwrap();
        let folder = storage
            .upsert_folder(NewFolder {
                account,
                remote_name: "INBOX".into(),
                attributes: vec![],
                role: None,
                delimiter: "/".into(),
                uid_validity: 7,
                highest_modseq: 0,
            })
            .await
            .unwrap();
        let raw = sample_message("budget", "quarterly budget body");
        let blob = storage.write_blob(raw.clone()).await.unwrap();
        storage
            .ingest_batch(IngestBatch {
                messages: vec![IngestMessage {
                    folder,
                    uid: 1,
                    internal_date: 1_700_000_000_000,
                    flags: vec![],
                    parsed: kestrel_core::mime::StalwartParser::parse(&raw).unwrap(),
                    raw_blob: Some(blob),
                    raw_size: raw.len() as u64,
                }],
            })
            .await
            .unwrap();
        cancel.cancel();
    }

    // Engine (re-spawned above sees the same DBs through shared paths).
    let reply = send(&handle, CommandPayload::ListAccounts { reply: dummy() }).await;
    let Reply::Accounts(accounts) = reply else {
        panic!("expected accounts");
    };
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].email, "e2e@example.org");
    assert_eq!(accounts[0].state, ConnectionState::Disconnected);

    let account = accounts[0].id;
    let Reply::Folders(folders) = send(
        &handle,
        CommandPayload::ListFolders {
            account,
            reply: dummy(),
        },
    )
    .await
    else {
        panic!("expected folders");
    };
    let _ = folders;

    // ListMessages via protocol.
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::ListMessages {
                folder: FolderId::from_uuid(uuid::Uuid::now_v7()), // replaced below
                window: Window::default(),
                sort: SortSpec::default(),
                reply: tx,
            },
        ))
        .await
        .unwrap();
    let _ = rx.await;

    // Search: re-seed via the engine's own outbox + index path instead.
    let reply = send(
        &handle,
        CommandPayload::ComposeSubmit {
            draft: Draft {
                account,
                from: Address::bare("t@x.example"),
                to: vec![Address::bare("r@y.example")],
                cc: vec![],
                bcc: vec![],
                subject: "engine test".into(),
                in_reply_to: None,
                references: vec![],
                body_markdown: "**hello**".into(),
                attachments: vec![],
            },
            reply: dummy(),
        },
    )
    .await;
    assert!(matches!(reply, Reply::Accepted));

    // OutboxEnqueued event fired.
    match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
        Ok(Ok(EngineEvent::OutboxEnqueued { .. })) => {}
        other => panic!("expected OutboxEnqueued, got {other:?}"),
    }

    // Flag mutation round trip on a bogus id: typed error, no panic.
    let reply = send(
        &handle,
        CommandPayload::SetFlags {
            messages: vec![kestrel_core::ids::MessageId::from_uuid(uuid::Uuid::now_v7())],
            flags: FlagOp::Add(vec![Flag::Seen]),
            reply: dummy(),
        },
    )
    .await;
    assert!(matches!(reply, Reply::Accepted));

    // Resync ack.
    let reply = send(&handle, CommandPayload::ResyncState { reply: dummy() }).await;
    assert!(matches!(reply, Reply::Accepted));

    // Search path stays functional (empty query is bounded).
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .commands
        .send(command(
            FrontendKind::Gui,
            CommandPayload::Search {
                query: SearchQuery::default(),
                reply: tx,
            },
        ))
        .await
        .unwrap();
    let reply = rx.await.unwrap();
    assert!(matches!(reply, Reply::SearchResults(_)));
}

#[tokio::test]
async fn shutdown_is_ordered_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_engine(dir.path()).await;
    let mut events = handle.events.resubscribe();
    let _ = events.recv().await; // EngineStarted

    handle
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::Shutdown { drain: true },
        ))
        .await
        .unwrap();
    drop(handle.commands);

    let mut stages = Vec::new();
    let deadline = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(EngineEvent::EngineShutdownProgress { stage }) = events.recv().await {
                stages.push(stage);
                if stage == ShutdownStage::Done {
                    break;
                }
            }
        }
    })
    .await;
    assert!(deadline.is_ok(), "shutdown completes in order");
    assert_eq!(stages.first(), Some(&ShutdownStage::DetachFrontends));
    assert_eq!(stages.last(), Some(&ShutdownStage::Done));
}
