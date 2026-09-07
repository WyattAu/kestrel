#![allow(clippy::wildcard_imports)] // sibling-module glue (issue #4)
//! Onboarding: setup-wizard key handling and account add/remove.

use crossterm::event::{KeyCode, KeyEvent};
use kestrel_core::protocol::{Command, CommandPayload, FrontendKind, Reply};
use kestrel_engine::EngineHandle;

use crate::{
    app::{AppState, Mode},
    event::{engine::*, next_request_id},
};

/// Handles keys in setup mode.
pub(crate) async fn handle_setup_key(handle: &EngineHandle, state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            state.mode = Mode::Normal;
            state.status = "setup cancelled".into();
        }
        KeyCode::Enter => {
            state.mode = Mode::Normal;
            state.status = "connecting…".into();
            add_account_from_setup(handle, state).await;
        }
        KeyCode::Tab => {
            state.setup_field = (state.setup_field + 1) % 3;
        }
        KeyCode::Backspace => match state.setup_field {
            0 => {
                state.setup_email.pop();
            }
            1 => {
                state.setup_password.pop();
            }
            _ => {
                state.setup_imap_host.pop();
            }
        },
        KeyCode::Char(c) => match state.setup_field {
            0 => state.setup_email.push(c),
            1 => state.setup_password.push(c),
            _ => state.setup_imap_host.push(c),
        },
        _ => {}
    }
}

pub(crate) async fn remove_account(handle: &EngineHandle, state: &mut AppState) {
    let Some(account) = state.account().cloned() else {
        state.status = "no account selected".into();
        state.mode = Mode::Normal;
        return;
    };
    let account_id = account.id;
    state.mode = Mode::Normal;
    state.status = format!("removing account: {}...", account.name);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::RemoveAccount {
                account: account_id,
                reply: tx,
            },
        })
        .await;
    match rx.await {
        Ok(Reply::Accepted) => {
            state.status = format!("removed account: {}", account.name);
            refresh_accounts(handle, state).await;
            if state.account().is_some() {
                refresh_folders(handle, state).await;
            }
        }
        Ok(Reply::Err(e)) => {
            state.status = format!("remove failed: {e}");
        }
        _ => {
            state.status = "remove: unexpected reply".into();
        }
    }
}

pub(crate) async fn add_account_from_setup(handle: &EngineHandle, state: &mut AppState) {
    use kestrel_core::{
        provider::{detect_provider, provider_preset},
        secrets::SecretString,
    };

    let email = state.setup_email.clone();
    let password = state.setup_password.clone();
    let imap_host = state.setup_imap_host.clone();
    if email.is_empty() || !email.contains('@') {
        state.status = "setup: valid email required".into();
        return;
    }
    let provider = detect_provider(&email);
    let mut config = provider_preset(&provider, &email);
    if !imap_host.is_empty() {
        if let Some((h, p)) = imap_host.split_once(':') {
            config.imap_host = h.to_owned();
            config.imap_port = p.parse().unwrap_or(config.imap_port);
        } else {
            config.imap_host = imap_host;
        }
    }

    // Step 1: Test connection
    state.status = "testing connection...".into();
    let (test_tx, test_rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::TestConnection {
                config: config.clone(),
                password: SecretString::new(password.clone()),
                reply: test_tx,
            },
        })
        .await;
    match test_rx.await {
        Ok(Reply::Accepted) => {
            state.status = "connection OK, adding account...".into();
        }
        Ok(Reply::Err(e)) => {
            state.status = format!("setup failed: {e}");
            return;
        }
        _ => {
            state.status = "setup: unexpected reply from connection test".into();
            return;
        }
    }

    // Step 2: Add account
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::AddAccount {
                config,
                password: SecretString::new(password),
                reply: tx,
            },
        })
        .await;
    match rx.await {
        Ok(Reply::Accounts(accounts)) => {
            state.status = format!("{} account(s) — syncing", accounts.len());
            state.accounts = accounts;
        }
        Ok(Reply::Err(e)) => {
            state.status = format!("setup failed: {e}");
        }
        _ => {
            state.status = "setup: unexpected reply".into();
        }
    }
}
