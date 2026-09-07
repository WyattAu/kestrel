#![allow(clippy::wildcard_imports)] // sibling-module glue (issue #4)
//! Engine-event fan-out and refresh: folders, messages, preview, search.

use kestrel_core::protocol::{
    Command, CommandPayload, EngineEvent, FrontendKind, Reply, SearchQuery, Window,
};
use kestrel_engine::EngineHandle;

use crate::{app::AppState, event::next_request_id};

pub(crate) async fn handle_engine_event(
    handle: &EngineHandle,
    state: &mut AppState,
    ev: EngineEvent,
) {
    match ev {
        EngineEvent::MailArrived { folder, .. } | EngineEvent::MessagesChanged { folder, .. } => {
            if Some(folder) == state.folder_id() {
                refresh_messages(handle, state).await;
            }
        }
        EngineEvent::FlagsChanged { .. } => {
            refresh_messages(handle, state).await;
        }
        EngineEvent::FolderTreeChanged { .. } => {
            refresh_folders(handle, state).await;
        }
        EngineEvent::AccountConnection { state: conn, .. } => {
            state.status = format!("{conn:?}");
        }
        EngineEvent::ServiceDegraded { service, error, .. } => {
            state.status = format!("⚠ {service} degraded: {error}");
        }
        EngineEvent::OutboxEnqueued { .. } => {
            state.status = "queued for sending".into();
        }
        EngineEvent::MailSent { .. } => {
            state.status = "sent".into();
        }
        EngineEvent::MailFailed { error, .. } => {
            state.status = format!("send failed: {error}");
        }
        EngineEvent::RemoteContentBlocked { count, .. } => {
            state.status = format!("{count} remote item(s) blocked");
        }
        EngineEvent::EventStreamLagged { missed } => {
            state.status = format!("⚠ missed {missed} events; resyncing");
            refresh_all(handle, state).await;
        }
        _ => {}
    }
}

pub(crate) async fn refresh_all(handle: &EngineHandle, state: &mut AppState) {
    refresh_accounts(handle, state).await;
    if state.account().is_some() {
        refresh_folders(handle, state).await;
    }
    if state.folder_id().is_some() {
        refresh_messages(handle, state).await;
    }
}

pub(crate) async fn refresh_accounts(handle: &EngineHandle, state: &mut AppState) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ListAccounts { reply: tx },
        })
        .await;
    if let Ok(Reply::Accounts(accounts)) = rx.await {
        state.accounts = accounts;
    }
}

pub(crate) async fn refresh_folders(handle: &EngineHandle, state: &mut AppState) {
    let Some(account) = state.account() else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ListFolders {
                account: account.id,
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::Folders(folders)) = rx.await {
        state.set_folders(folders);
    }
}

pub(crate) async fn refresh_messages(handle: &EngineHandle, state: &mut AppState) {
    let Some(folder) = state.folder_id() else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ListMessages {
                folder,
                window: Window {
                    offset: state.page_offset,
                    limit: state.window_limit,
                },
                sort: kestrel_core::protocol::SortSpec::default(),
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::Messages(page)) = rx.await {
        state.set_page(page);
    }
}

pub(crate) async fn load_preview(
    handle: &EngineHandle,
    state: &mut AppState,
    id: kestrel_core::ids::MessageId,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::GetMessage {
                message: id,
                body: kestrel_core::protocol::BodyPreference::Full,
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::Message(view)) = rx.await {
        state.preview = Some(view);
    }
}

pub(crate) async fn search(handle: &EngineHandle, state: &mut AppState, query: &str) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::Search {
                query: SearchQuery {
                    text: Some(query.to_owned()),
                    ..SearchQuery::default()
                },
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::SearchResults(hits)) = rx.await {
        state.status = format!("{} hit(s)", hits.len());
    }
}
