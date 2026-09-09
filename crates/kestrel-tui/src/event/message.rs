#![allow(clippy::wildcard_imports)] // sibling-module glue (issue #4)
//! Per-message and bulk actions: delete, flag, read-state, archive, snooze.

use kestrel_core::{
    ids::{AccountId, MessageId},
    protocol::{Command, CommandPayload, FolderRole, FrontendKind, Reply},
};
use kestrel_engine::EngineHandle;

use crate::{
    app::{AppState, Mode},
    event::{engine::*, next_request_id},
};

pub(crate) async fn delete_selected(handle: &EngineHandle, state: &mut AppState) {
    if let Some(id) = state.message_id() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = handle
            .commands
            .send(Command {
                id: next_request_id(),
                origin: FrontendKind::Tui,
                payload: CommandPayload::DeleteMessages {
                    messages: vec![id],
                    expunge: false,
                    reply: tx,
                },
            })
            .await;
        if matches!(rx.await, Ok(Reply::Accepted)) {
            state.status = "deleted".into();
            refresh_messages(handle, state).await;
        }
    }
}

pub(crate) async fn toggle_flagged(handle: &EngineHandle, state: &mut AppState) {
    let Some(msg) = state.message() else {
        return;
    };
    let id = msg.id;
    let op = if msg.is_flagged {
        kestrel_core::protocol::FlagOp::Remove(vec![kestrel_core::protocol::Flag::Flagged])
    } else {
        kestrel_core::protocol::FlagOp::Add(vec![kestrel_core::protocol::Flag::Flagged])
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: vec![id],
                flags: op,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let label = if state.message().is_some_and(|m| m.is_flagged) {
            "flagged"
        } else {
            "unflagged"
        };
        state.status = label.into();
        refresh_messages(handle, state).await;
    }
}

pub(crate) async fn mark_all_read(handle: &EngineHandle, state: &mut AppState) {
    let ids: Vec<_> = state.page.items.iter().map(|m| m.id).collect();
    if ids.is_empty() {
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: ids,
                flags: kestrel_core::protocol::FlagOp::Add(vec![
                    kestrel_core::protocol::Flag::Seen,
                ]),
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        state.status = "all read".into();
        refresh_messages(handle, state).await;
    }
}

pub(crate) async fn toggle_mark_unread(handle: &EngineHandle, state: &mut AppState) {
    let Some(msg) = state.message() else {
        return;
    };
    let id = msg.id;
    let op = if msg.is_read {
        kestrel_core::protocol::FlagOp::Remove(vec![kestrel_core::protocol::Flag::Seen])
    } else {
        kestrel_core::protocol::FlagOp::Add(vec![kestrel_core::protocol::Flag::Seen])
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: vec![id],
                flags: op,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let label = if state.page.items[state.selected_message].is_read {
            "marked unread"
        } else {
            "marked read"
        };
        state.status = label.into();
        refresh_messages(handle, state).await;
    }
}

/// Move `ids` to the account's Archive folder (via role), or fall back to
/// a folder literally named "Archive". Returns the count actually moved.
///
/// Resolves the target from the cached folder list: the engine's
/// `ListFolders` reply is authoritative (special-use `\Archive` beats name
/// heuristics), so no second round-trip is needed.
async fn move_to_archive(
    handle: &EngineHandle,
    state: &AppState,
    account: AccountId,
    ids: Vec<MessageId>,
) -> Option<Reply> {
    let archive = state
        .folders
        .iter()
        .find(|f| f.account == account && matches!(f.role, Some(FolderRole::Archive)))
        .or_else(|| {
            state
                .folders
                .iter()
                .find(|f| f.account == account && f.remote_name.eq_ignore_ascii_case("Archive"))
        })?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sent = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::MoveMessages {
                messages: ids,
                to: archive.id,
                reply: tx,
            },
        })
        .await;
    sent.ok()?;
    rx.await.ok()
}

pub(crate) async fn archive_selected(handle: &EngineHandle, state: &mut AppState) {
    let Some(id) = state.message_id() else {
        return;
    };
    let Some(account) = state.account().map(|a| a.id) else {
        return;
    };
    match move_to_archive(handle, state, account, vec![id]).await {
        Some(Reply::Accepted) => {
            state.status = "archived".into();
            refresh_messages(handle, state).await;
        }
        Some(_) => state.status = "archive failed".into(),
        None => state.status = "no archive folder".into(),
    }
}

pub(crate) async fn execute_snooze(handle: &EngineHandle, state: &mut AppState) {
    use kestrel_core::clock::Clock as _;
    let Some(msg) = state.message() else {
        state.mode = Mode::Normal;
        return;
    };
    let Some(account_id) = state.account().map(|a| a.id) else {
        state.mode = Mode::Normal;
        state.status = "no account selected".into();
        return;
    };
    let now = kestrel_core::clock::SystemClock.now_unix_ms();
    let hour_ms = 3_600_000;
    let day_ms = 86_400_000;
    let default_ms = 12 * hour_ms;
    let until = match state.snooze_selection {
        0 => now + default_ms,
        1 => now + 7 * day_ms,
        _ => {
            let hours: i64 = state.snooze_hours.parse().unwrap_or(1);
            now + hours * hour_ms
        }
    };
    let msg_id = msg.id;
    let folder_id = msg.folder;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SnoozeMessage {
                message: msg_id,
                account: account_id,
                folder: folder_id,
                until,
                reply: tx,
            },
        })
        .await;
    state.mode = Mode::Normal;
    state.snooze_hours.clear();
    state.snooze_selection = 0;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        state.status = "message snoozed".into();
        refresh_messages(handle, state).await;
    } else {
        state.status = "snooze failed".into();
    }
}

/// Delete all messages selected in multi-select mode.
pub(crate) async fn bulk_delete_selected(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    let mut folders = Vec::new();
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
            folders.push((msg.id, msg.folder));
        }
    }
    if ids.is_empty() {
        state.mode = Mode::Normal;
        state.status.clear();
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::DeleteMessages {
                messages: ids,
                expunge: false,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let count = state.selected_messages.len();
        state.toggle_multi_select();
        state.status = format!("deleted {count} message(s)");
        refresh_messages(handle, state).await;
    }
    state.mode = Mode::Normal;
}

/// Archive all messages selected in multi-select mode.
pub(crate) async fn bulk_archive(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
        }
    }
    if ids.is_empty() {
        return;
    }
    let count = ids.len();
    let Some(account) = state.account().map(|a| a.id) else {
        state.mode = Mode::Normal;
        return;
    };
    match move_to_archive(handle, state, account, ids).await {
        Some(Reply::Accepted) => {
            state.toggle_multi_select();
            state.status = format!("archived {count} message(s)");
            refresh_messages(handle, state).await;
        }
        Some(_) => state.status = "archive failed".into(),
        None => state.status = "no archive folder".into(),
    }
    state.mode = Mode::Normal;
}

/// Toggle read/unread for all messages selected in multi-select mode.
pub(crate) async fn bulk_toggle_unread(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    let mut mark_unread = false;
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
            if msg.is_read {
                mark_unread = true;
            }
        }
    }
    if ids.is_empty() {
        return;
    }
    let op = if mark_unread {
        kestrel_core::protocol::FlagOp::Add(vec![kestrel_core::protocol::Flag::Seen])
    } else {
        kestrel_core::protocol::FlagOp::Remove(vec![kestrel_core::protocol::Flag::Seen])
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: ids,
                flags: op,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let count = state.selected_messages.len();
        state.status = format!("toggled read status for {count} message(s)");
        refresh_messages(handle, state).await;
    }
}

/// Flag (star) all messages selected in multi-select mode.
pub(crate) async fn bulk_flag_selected(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
        }
    }
    if ids.is_empty() {
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: ids,
                flags: kestrel_core::protocol::FlagOp::Add(vec![
                    kestrel_core::protocol::Flag::Flagged,
                ]),
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let count = state.selected_messages.len();
        state.toggle_multi_select();
        state.status = format!("flagged {count} message(s)");
        refresh_messages(handle, state).await;
    }
}
