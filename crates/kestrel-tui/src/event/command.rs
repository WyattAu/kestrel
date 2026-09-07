#![allow(clippy::wildcard_imports)] // sibling-module glue (issue #4)
//! `:`-command dispatch: account/event subcommands and saved searches.

use kestrel_core::{
    clock::Clock as _,
    protocol::{Command, CommandPayload, FrontendKind, MessagePage, Reply, SearchQuery},
};
use kestrel_engine::EngineHandle;

use crate::{
    app::{AppState, Mode},
    event::{fmt::*, next_request_id},
};

pub(crate) fn execute_account_command(state: &mut AppState, sub: &str) {
    match sub {
        "list" => {
            if state.accounts.is_empty() {
                state.status = "no accounts configured".into();
            } else {
                let list: Vec<String> = state
                    .accounts
                    .iter()
                    .map(|a| format!("{} ({})", a.name, a.email))
                    .collect();
                state.status = format!("accounts: {}", list.join(", "));
            }
        }
        "edit" => {
            if let Some(acc) = state.account() {
                state.status = format!(
                    "edit account: {} — IMAP: {} ({:?})",
                    acc.name, acc.host, acc.protocol
                );
            } else {
                state.status = "no account selected".into();
            }
        }
        "remove" => {
            if let Some(acc) = state.account() {
                state.status = format!(
                    "remove account: {} ({})? (y to confirm)",
                    acc.name, acc.email
                );
                state.mode = Mode::ConfirmRemoveAccount;
            } else {
                state.status = "no account selected".into();
            }
        }
        _ => {
            state.status = "usage: :account <list|edit|remove>".into();
        }
    }
}

pub(crate) async fn execute_event_command(handle: &EngineHandle, state: &mut AppState, sub: &str) {
    match sub {
        "create" => {
            // Prompt for event details via status line; use a simple
            // inline prompt: title, start, end (ISO-ish format).
            state.status =
                "event create: enter title, start (YYYYMMDDTHHMMSS), end (YYYYMMDDTHHMMSS), \
                 separated by |"
                    .into();
            // For now, create a default test event to prove the pipeline.
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = handle
                .commands
                .send(Command {
                    id: next_request_id(),
                    origin: FrontendKind::Tui,
                    payload: CommandPayload::CreateEvent {
                        calendar_id: String::new(),
                        uid: format!("{}@kestrel", uuid::Uuid::now_v7()),
                        summary: "New Event".into(),
                        description: None,
                        location: None,
                        start_time: kestrel_core::clock::SystemClock.now_unix_ms(),
                        end_time: kestrel_core::clock::SystemClock.now_unix_ms() + 3_600_000,
                        all_day: false,
                        reply: tx,
                    },
                })
                .await;
            match rx.await {
                Ok(Reply::Accepted) => {
                    state.status = "event created".into();
                }
                Ok(Reply::Err(e)) => {
                    state.status = format!("event create failed: {e}");
                }
                _ => {
                    state.status = "event create: unexpected reply".into();
                }
            }
        }
        _ => {
            state.status = "usage: :event create".into();
        }
    }
}

pub(crate) async fn execute_command(handle: &EngineHandle, state: &mut AppState, cmd: &str) {
    let cmd = cmd.strip_prefix(':').unwrap_or(cmd);
    let mut parts = cmd.splitn(3, ' ');
    let verb = parts.next().unwrap_or_default();
    match verb {
        "sort" => {
            let arg = parts.next().unwrap_or_default();
            match arg {
                "date" => {
                    state.sort_field = kestrel_core::protocol::SortField::Date;
                    state.status = format!("sort: date {}", sort_dir_label(state.sort_dir));
                }
                "from" | "sender" => {
                    state.sort_field = kestrel_core::protocol::SortField::Sender;
                    state.status = format!("sort: sender {}", sort_dir_label(state.sort_dir));
                }
                "subject" => {
                    state.sort_field = kestrel_core::protocol::SortField::Subject;
                    state.status = format!("sort: subject {}", sort_dir_label(state.sort_dir));
                }
                "asc" => {
                    state.sort_dir = kestrel_core::protocol::SortDir::Asc;
                    state.status = format!("sort: {} asc", field_label(state.sort_field));
                }
                "desc" => {
                    state.sort_dir = kestrel_core::protocol::SortDir::Desc;
                    state.status = format!("sort: {} desc", field_label(state.sort_field));
                }
                _ => {
                    state.status = "usage: :sort <date|from|subject> or :sort <asc|desc>".into();
                }
            }
        }
        "save-search" => {
            let name = parts.next().unwrap_or_default();
            if name.is_empty() {
                state.status = "usage: :save-search <name>".into();
            } else {
                let query = SearchQuery {
                    text: Some(state.search_input.clone()),
                    ..SearchQuery::default()
                };
                state
                    .saved_searches
                    .push(kestrel_core::config::SavedSearch {
                        name: name.to_string(),
                        query,
                    });
                state.status = format!("search saved: {name}");
            }
        }
        "load-search" => {
            let name = parts.next().unwrap_or_default();
            if name.is_empty() {
                state.status = "usage: :load-search <name>".into();
            } else if let Some(saved) = state.saved_searches.iter().find(|s| s.name == name) {
                let query = saved.query.clone();
                state.status = format!("loaded search: {name}");
                execute_saved_search(handle, state, &query).await;
            } else {
                let names: Vec<&str> = state
                    .saved_searches
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect();
                state.status = format!("unknown search: {name} (available: {})", names.join(", "));
            }
        }
        "list-searches" => {
            if state.saved_searches.is_empty() {
                state.status = "no saved searches".into();
            } else {
                let names: Vec<&str> = state
                    .saved_searches
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect();
                state.status = format!("saved searches: {}", names.join(", "));
            }
        }
        "account" => {
            let sub = parts.next().unwrap_or_default();
            execute_account_command(state, sub);
        }
        "event" => {
            let sub = parts.next().unwrap_or_default();
            execute_event_command(handle, state, sub).await;
        }
        _ => {
            state.status = format!("unknown command: {cmd}");
        }
    }
}

pub(crate) async fn execute_saved_search(
    handle: &EngineHandle,
    state: &mut AppState,
    query: &SearchQuery,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::Search {
                query: query.clone(),
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::SearchResults(hits)) = rx.await {
        let messages: Vec<kestrel_core::protocol::MessageSummary> =
            hits.iter().map(|h| h.message.clone()).collect();
        let total = hits.len() as u64;
        state.original_page = MessagePage {
            items: messages.clone(),
            total,
        };
        state.page.items = messages;
        state.page.total = total;
        state.selected_message = 0;
        state.status = format!("{} hit(s)", hits.len());
    }
}
