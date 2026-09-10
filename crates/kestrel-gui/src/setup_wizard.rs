//! Account setup wizard callbacks and folder-loading logic.

use std::sync::Arc;

use kestrel_core::{
    ids::FolderId,
    protocol::{Command, CommandPayload, FrontendKind, Reply},
    provider::{
        detect_provider, provider_display_name, provider_help, provider_oauth2_button_label,
        provider_preset, provider_supports_oauth2, validate_account_config,
    },
    secrets::SecretString,
};
use slint::ComponentHandle as _;

use crate::{
    state::GuiState,
    util::{account_color_for_index, hex_to_slint_color, show_toast},
};

/// Wire all setup-wizard callbacks and perform the initial account check.
pub(crate) fn install(state: &GuiState) {
    let Some(app) = state.app_weak.upgrade() else {
        return;
    };

    initial_account_check(state);
    wire_select_account(state, &app);
    wire_add_account(state, &app);
    wire_email_changed(state, &app);
    wire_step_navigation(state, &app);
    wire_oauth2_flow(state, &app);
    wire_reauth_account(state, &app);
}

// ────────────────────── re-auth affordance (#27) ──────────────────────

/// Re-auth affordance (#27): an account whose `OAuth2` refresh token was
/// rejected lands in `ConnectionState::NeedsReauth`; the sidebar shows a
/// `re-auth` badge for it (event-driven via `ForwardedEvent`) and clicking
/// the badge restarts the browser flow with the account's own email —
/// the same wizard path as first-time setup, minus the typing.
fn wire_reauth_account(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let accounts_cache = Arc::clone(&state.accounts);
    app.on_reauth_account(move |idx| {
        let idx = usize::try_from(idx).unwrap_or(0);
        let Some(email) = accounts_cache.email_at(idx) else {
            return;
        };
        let provider = detect_provider(&email);
        if !provider_supports_oauth2(&provider) {
            let w2 = w.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(app) = w2.upgrade() {
                    show_toast(
                        &app,
                        "re-authentication requires an OAuth2 provider; edit the account instead",
                        "error",
                    );
                }
            })
            .ok();
            return;
        }
        // Owned clones only: the `FnMut` closure can fire repeatedly.
        start_oauth2_flow_for(h.clone(), w.clone(), &email, provider);
    });
}

/// Starts an `OAuth2` browser flow for `email` outside the setup wizard UI:
/// opens the browser, then reuses the wizard's completion watcher so the
/// exchanged tokens land via `AddAccount` (the upsert-by-email path that
/// repairs a `NeedsReauth` account).
fn start_oauth2_flow_for(
    handle: kestrel_engine::EngineHandle,
    w: slint::Weak<crate::AppWindow>,
    email: &str,
    provider: kestrel_core::protocol::Provider,
) {
    let h = handle;
    let email = email.to_owned();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = h
                .commands
                .send(Command {
                    id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                    origin: FrontendKind::Gui,
                    payload: CommandPayload::StartOAuth2Flow {
                        provider: provider.clone(),
                        reply: tx,
                    },
                })
                .await;
            match rx.await {
                Ok(Reply::OAuthUrl(url)) => {
                    if let Err(e) = open::that(&url) {
                        tracing::warn!("failed to open browser: {e}");
                    }
                    let w_toast = w.clone();
                    spawn_oauth2_completion_watcher(h, w, provider, email);
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w_toast.upgrade() {
                            show_toast(
                                &app,
                                "Re-authentication: complete sign-in in your browser",
                                "info",
                            );
                        }
                    })
                    .ok();
                }
                Ok(Reply::Err(e)) => {
                    let msg = e.user_message();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w.upgrade() {
                            show_toast(&app, &format!("re-auth failed: {msg}"), "error");
                        }
                    })
                    .ok();
                }
                _ => {}
            }
        });
    });
}

// ────────────────────── helpers ──────────────────────

/// Fetch the full folder list for all accounts and populate the UI.
async fn fetch_all_folders(
    handle: &kestrel_engine::EngineHandle,
    accounts: &crate::state::AccountCache,
    weak: slint::Weak<crate::AppWindow>,
) -> Option<Vec<FolderId>> {
    let h = handle;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = h
        .commands
        .send(Command {
            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
            origin: FrontendKind::Gui,
            payload: CommandPayload::ListAccounts { reply: tx },
        })
        .await;
    let Reply::Accounts(accts) = rx.await.ok()? else {
        return None;
    };

    let mut all_folder_names: Vec<String> = Vec::new();
    let mut all_folder_ids: Vec<FolderId> = Vec::new();
    let mut all_folder_unreads: Vec<i32> = Vec::new();
    let acct_names: Vec<String> = accts.iter().map(|a| a.name.clone()).collect();
    let acct_emails: Vec<String> = accts.iter().map(|a| a.email.clone()).collect();

    // Unified Inbox as the first virtual folder.
    all_folder_names.push("Unified Inbox".into());
    all_folder_ids.push(FolderId::from_uuid(uuid::Uuid::nil()));
    all_folder_unreads.push(0);

    accounts.replace(accts.iter().map(|a| a.id).collect(), acct_emails.clone());

    for acct in &accts {
        let (ftx, frx) = tokio::sync::oneshot::channel();
        let _ = h
            .commands
            .send(Command {
                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                origin: FrontendKind::Gui,
                payload: CommandPayload::ListFolders {
                    account: acct.id,
                    reply: ftx,
                },
            })
            .await;
        if let Ok(Reply::Folders(folders)) = frx.await {
            for folder in &folders {
                all_folder_names.push(format!("{}/{}", acct.name, folder.remote_name));
                all_folder_ids.push(folder.id);
                all_folder_unreads.push(i32::try_from(folder.unread).unwrap_or(0));
            }
        }
    }

    let count = accts.len();
    let names_clone = all_folder_names.clone();
    let unreads_clone = all_folder_unreads.clone();
    let acct_colors_clone = acct_emails.clone();
    let acct_states_clone: Vec<kestrel_core::protocol::ConnectionState> =
        accts.iter().map(|a| a.state).collect();

    slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            app.set_account_count(i32::try_from(count).unwrap_or(0));
            if count > 0 {
                app.set_show_setup(false);
                app.set_status_text(format!("{count} account(s) syncing...").into());
            }
            let acct_strs: Vec<slint::SharedString> = acct_names
                .iter()
                .map(|s| slint::SharedString::from(s.as_str()))
                .collect();
            app.set_account_names(acct_strs.as_slice().into());
            // Reset re-auth badges to the authoritative account states.
            let reauth: Vec<bool> = acct_states_clone
                .iter()
                .map(|s| *s == kestrel_core::protocol::ConnectionState::NeedsReauth)
                .collect();
            app.set_account_needs_reauth(reauth.as_slice().into());
            let colors: Vec<slint::Color> = acct_colors_clone
                .iter()
                .enumerate()
                .map(|(i, _)| hex_to_slint_color(account_color_for_index(i)))
                .collect();
            app.set_account_colors(colors.as_slice().into());
            let strs: Vec<slint::SharedString> = names_clone
                .iter()
                .map(|s| slint::SharedString::from(s.as_str()))
                .collect();
            app.set_folder_names(strs.as_slice().into());
            app.set_folder_unreads(unreads_clone.as_slice().into());
        }
    })
    .ok();

    Some(all_folder_ids)
}

// ────────────────────── initial account check ──────────────────────

fn initial_account_check(state: &GuiState) {
    let handle = state.handle.clone();
    let accounts = Arc::clone(&state.accounts);
    let lists = Arc::clone(&state.lists);
    let weak = state.app_weak.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let Some(ids) = fetch_all_folders(&handle, &accounts, weak).await else {
                return;
            };
            lists.set_folders(ids);
        });
    });
}

// ────────────────────── on_select_account ──────────────────────

fn wire_select_account(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let lists = Arc::clone(&state.lists);
    let accounts_cache = Arc::clone(&state.accounts);
    app.on_select_account(move |idx| {
        let idx = usize::try_from(idx).unwrap_or(0);
        let Some(account_id) = accounts_cache.id_at(idx) else {
            return;
        };
        let h = h.clone();
        let w = w.clone();
        let lists2 = Arc::clone(&lists);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let (ftx, frx) = tokio::sync::oneshot::channel();
                let _ = h
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::ListFolders {
                            account: account_id,
                            reply: ftx,
                        },
                    })
                    .await;
                if let Ok(Reply::Folders(folders)) = frx.await {
                    let mut all_folder_names: Vec<String> = Vec::new();
                    let mut all_folder_ids: Vec<FolderId> = Vec::new();
                    let mut all_folder_unreads: Vec<i32> = Vec::new();
                    all_folder_names.push("Unified Inbox".into());
                    all_folder_ids.push(FolderId::from_uuid(uuid::Uuid::nil()));
                    all_folder_unreads.push(0);
                    for folder in &folders {
                        all_folder_names.push(folder.remote_name.clone());
                        all_folder_ids.push(folder.id);
                        all_folder_unreads.push(i32::try_from(folder.unread).unwrap_or(0));
                    }
                    let names_clone = all_folder_names.clone();
                    let unreads_clone = all_folder_unreads.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w.upgrade() {
                            app.set_selected_account_idx(i32::try_from(idx).unwrap_or(0));
                            let strs: Vec<slint::SharedString> = names_clone
                                .iter()
                                .map(|s| slint::SharedString::from(s.as_str()))
                                .collect();
                            app.set_folder_names(strs.as_slice().into());
                            app.set_folder_unreads(unreads_clone.as_slice().into());
                            app.set_selected_folder_idx(-1);
                            app.set_message_subjects(vec![].as_slice().into());
                            app.set_message_froms(vec![].as_slice().into());
                            app.set_message_dates(vec![].as_slice().into());
                            app.set_thread_depths(vec![].as_slice().into());
                            app.set_total_messages(0);
                        }
                    })
                    .ok();
                    lists2.set_folders(all_folder_ids);
                }
            });
        });
    });
}

// ────────────────────── on_add_account ──────────────────────

fn wire_add_account(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let lists = Arc::clone(&state.lists);
    let accounts_cache = Arc::clone(&state.accounts);

    app.on_add_account(move |display_name, email, password, imap_host, smtp_host| {
        let Some(app) = w.upgrade() else { return };
        let is_editing = app.get_editing_account();
        app.set_setup_busy(true);
        app.set_setup_error(slint::SharedString::default());

        let provider = detect_provider(&email);
        let mut config = provider_preset(&provider, &email);
        if !display_name.is_empty() {
            config.display_name = display_name.to_string();
        }
        config.email = email.to_string();
        if !imap_host.is_empty() {
            match imap_host.split_once(':') {
                Some((h, p)) => {
                    config.imap_host = h.to_string();
                    config.imap_port = p.parse().unwrap_or(config.imap_port);
                }
                None => config.imap_host = imap_host.to_string(),
            }
        }
        if !smtp_host.is_empty() {
            match smtp_host.split_once(':') {
                Some((h, p)) => {
                    config.smtp_host = h.to_string();
                    config.smtp_port = p.parse().unwrap_or(config.smtp_port);
                }
                None => config.smtp_host = smtp_host.to_string(),
            }
        }
        let errors = validate_account_config(&config);
        if !errors.is_empty() {
            app.set_setup_error(errors.join("; ").into());
            app.set_setup_busy(false);
            return;
        }

        app.set_setup_step(4);
        app.set_setup_testing_status("Testing connection...".into());

        let h2 = h.clone();
        let w2 = w.clone();
        let lists2 = Arc::clone(&lists);
        let accounts2 = Arc::clone(&accounts_cache);
        let is_editing_clone = is_editing;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                // Step 1: Test connection
                let (test_tx, test_rx) = tokio::sync::oneshot::channel();
                let _ = h2
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::TestConnection {
                            config: config.clone(),
                            password: SecretString::new(password.to_string()),
                            reply: test_tx,
                        },
                    })
                    .await;
                match test_rx.await {
                    Ok(Reply::Accepted) => {
                        let w2c = w2.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2c.upgrade() {
                                app.set_setup_testing_status(
                                    "Connection successful! Adding account...".into(),
                                );
                            }
                        })
                        .ok();
                    }
                    Ok(Reply::Err(e)) => {
                        let msg = e.user_message();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_setup_error(msg.into());
                                app.set_setup_testing_status(slint::SharedString::default());
                                app.set_setup_busy(false);
                                app.set_setup_step(3);
                            }
                        })
                        .ok();
                        return;
                    }
                    _ => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_setup_error("unexpected reply from connection test".into());
                                app.set_setup_testing_status(slint::SharedString::default());
                                app.set_setup_busy(false);
                                app.set_setup_step(3);
                            }
                        })
                        .ok();
                        return;
                    }
                }

                // Step 2: Add or update account
                let (tx, rx) = tokio::sync::oneshot::channel();
                let payload = if is_editing_clone {
                    CommandPayload::UpdateAccount {
                        config,
                        password: SecretString::new(password.to_string()),
                        reply: tx,
                    }
                } else {
                    CommandPayload::AddAccount {
                        config,
                        password: SecretString::new(password.to_string()),
                        reply: tx,
                    }
                };
                let _ = h2
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload,
                    })
                    .await;
                match rx.await {
                    Ok(Reply::Accounts(accts)) => {
                        let n = accts.len();
                        let mut all_folder_names: Vec<String> = Vec::new();
                        let mut all_folder_ids: Vec<FolderId> = Vec::new();
                        let mut all_folder_unreads: Vec<i32> = Vec::new();
                        all_folder_names.push("Unified Inbox".into());
                        all_folder_ids.push(FolderId::from_uuid(uuid::Uuid::nil()));
                        all_folder_unreads.push(0);
                        let acct_names: Vec<String> =
                            accts.iter().map(|a| a.name.clone()).collect();
                        let acct_emails: Vec<String> =
                            accts.iter().map(|a| a.email.clone()).collect();
                        accounts2
                            .replace(accts.iter().map(|a| a.id).collect(), acct_emails.clone());
                        for acct in &accts {
                            let (ftx, frx) = tokio::sync::oneshot::channel();
                            let _ = h2
                                .commands
                                .send(Command {
                                    id: kestrel_core::ids::RequestId::from_uuid(
                                        uuid::Uuid::now_v7(),
                                    ),
                                    origin: FrontendKind::Gui,
                                    payload: CommandPayload::ListFolders {
                                        account: acct.id,
                                        reply: ftx,
                                    },
                                })
                                .await;
                            if let Ok(Reply::Folders(folders)) = frx.await {
                                for folder in &folders {
                                    all_folder_names
                                        .push(format!("{}/{}", acct.name, folder.remote_name));
                                    all_folder_ids.push(folder.id);
                                    all_folder_unreads
                                        .push(i32::try_from(folder.unread).unwrap_or(0));
                                }
                            }
                        }
                        let names_clone = all_folder_names.clone();
                        let unreads_clone = all_folder_unreads.clone();
                        let acct_emails_clone = acct_emails.clone();
                        let was_editing = is_editing_clone;
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_account_count(i32::try_from(n).unwrap_or(0));
                                app.set_show_setup(false);
                                app.set_editing_account(false);
                                app.set_editing_account_email(slint::SharedString::default());
                                if was_editing {
                                    show_toast(&app, "Account updated", "success");
                                }
                                app.set_status_text(format!("{n} account(s) syncing").into());
                                let acct_strs: Vec<slint::SharedString> = acct_names
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                app.set_account_names(acct_strs.as_slice().into());
                                // Reset re-auth badges to the authoritative account states.
                                let reauth: Vec<bool> = accts
                                    .iter()
                                    .map(|a| {
                                        a.state
                                            == kestrel_core::protocol::ConnectionState::NeedsReauth
                                    })
                                    .collect();
                                app.set_account_needs_reauth(reauth.as_slice().into());
                                let colors: Vec<slint::Color> = acct_emails_clone
                                    .iter()
                                    .enumerate()
                                    .map(|(i, _)| hex_to_slint_color(account_color_for_index(i)))
                                    .collect();
                                app.set_account_colors(colors.as_slice().into());
                                let strs: Vec<slint::SharedString> = names_clone
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                app.set_folder_names(strs.as_slice().into());
                                app.set_folder_unreads(unreads_clone.as_slice().into());
                                app.set_setup_busy(false);
                            }
                        })
                        .ok();
                        lists2.set_folders(all_folder_ids);
                    }
                    Ok(Reply::Err(e)) => {
                        let msg = e.user_message();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_setup_error(msg.into());
                                app.set_setup_testing_status(slint::SharedString::default());
                                app.set_setup_busy(false);
                            }
                        })
                        .ok();
                    }
                    _ => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_setup_error("unexpected reply".into());
                                app.set_setup_testing_status(slint::SharedString::default());
                                app.set_setup_busy(false);
                            }
                        })
                        .ok();
                    }
                }
            });
        });
    });
}

// ────────────────────── on_email_changed ──────────────────────

fn wire_email_changed(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_email_changed(move |email| {
        let Some(app) = w.upgrade() else { return };
        let email_str = email.to_string();
        if !email_str.contains('@') || email_str.is_empty() {
            app.set_provider_name(slint::SharedString::default());
            app.set_provider_help(slint::SharedString::default());
            app.set_setup_provider_name(slint::SharedString::default());
            app.set_setup_detected_hosts(slint::SharedString::default());
            app.set_setup_is_oauth2(false);
            app.set_setup_oauth2_button_label(slint::SharedString::default());
            app.set_setup_email_valid(false);
            return;
        }
        let provider = detect_provider(&email_str);
        let name = provider_display_name(&provider);
        let help = provider_help(&provider).unwrap_or_default();
        let config = provider_preset(&provider, &email_str);
        let hosts = format!(
            "IMAP: {}:{} | SMTP: {}:{}",
            config.imap_host, config.imap_port, config.smtp_host, config.smtp_port
        );
        let is_oauth2 = provider_supports_oauth2(&provider);
        let button_label = if is_oauth2 {
            provider_oauth2_button_label(&provider)
        } else {
            ""
        };
        app.set_provider_name(name.into());
        app.set_provider_help(help.into());
        app.set_setup_provider_name(name.into());
        app.set_setup_detected_hosts(hosts.into());
        app.set_setup_is_oauth2(is_oauth2);
        app.set_setup_oauth2_button_label(button_label.into());
        app.set_setup_email_valid(true);
    });
}

// ────────────────────── step navigation ──────────────────────

fn wire_step_navigation(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_next_step(move || {
        let Some(app) = w.upgrade() else { return };
        let step = app.get_setup_step();
        if step < 4 {
            app.set_setup_step(step + 1);
            app.set_setup_error(slint::SharedString::default());
        }
    });

    let w = app.as_weak();
    app.on_prev_step(move || {
        let Some(app) = w.upgrade() else { return };
        let step = app.get_setup_step();
        if step > 1 {
            app.set_setup_step(step - 1);
            app.set_setup_error(slint::SharedString::default());
        }
    });
}

// ────────────────────── OAuth2 flow ──────────────────────

fn wire_oauth2_flow(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    app.on_start_oauth2_flow(move || {
        let Some(app) = w.upgrade() else { return };
        let email_str = app.get_setup_email().to_string();
        if email_str.is_empty() {
            return;
        }
        let provider = detect_provider(&email_str);
        if !provider_supports_oauth2(&provider) {
            app.set_setup_error("provider does not support OAuth2".into());
            return;
        }
        app.set_setup_busy(true);
        app.set_setup_error(slint::SharedString::default());

        let h2 = h.clone();
        let w2 = w.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = h2
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::StartOAuth2Flow {
                            provider: provider.clone(),
                            reply: tx,
                        },
                    })
                    .await;
                match rx.await {
                    Ok(Reply::OAuthUrl(url)) => {
                        if let Err(e) = open::that(&url) {
                            tracing::warn!("failed to open browser: {e}");
                        }
                        {
                            let w3 = w2.clone();
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w3.upgrade() {
                                    app.set_setup_testing_status(
                                        "Waiting for browser authentication...".into(),
                                    );
                                    app.set_setup_busy(false);
                                }
                            })
                            .ok();
                        }
                        // Completion half (#28): the engine captures the
                        // redirect and exchanges the code autonomously;
                        // this watcher picks up the resulting event and
                        // links the exchanged credential set to the
                        // account through `AddAccount` (the keyring path
                        // that also seeds the refresh worker's slot).
                        spawn_oauth2_completion_watcher(h2, w2, provider, email_str);
                    }
                    Ok(Reply::Err(e)) => {
                        let msg = e.user_message();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_setup_error(msg.into());
                                app.set_setup_busy(false);
                            }
                        })
                        .ok();
                    }
                    _ => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_setup_error("unexpected reply from OAuth2 flow".into());
                                app.set_setup_busy(false);
                            }
                        })
                        .ok();
                    }
                }
            });
        });
    });
}

/// Watches for `OAuth2FlowCompleted` after a wizard-initiated flow and
/// finishes account linking: retrieve the token set (single-use) via
/// `CompleteOAuth2Flow`, then `AddAccount` with `auth_kind = "oauth2"`.
/// Only this thread completes wizard flows — the event itself never
/// carries token material, and the retrieval is single-use.
fn spawn_oauth2_completion_watcher(
    handle: kestrel_engine::EngineHandle,
    w: slint::Weak<crate::AppWindow>,
    provider: kestrel_core::protocol::Provider,
    email: String,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mut events = handle.events();
            // One flow at a time from the wizard; wait up to 10 minutes
            // (the capture server's own timeout is 5 minutes).
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_mins(10);
            let mut state = String::new();
            let mut finished = false;
            while tokio::time::Instant::now() < deadline {
                let Ok(Ok(ev)) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await
                else {
                    continue;
                };
                if let kestrel_core::protocol::EngineEvent::OAuth2FlowCompleted {
                    state: flow_state,
                    result,
                } = ev
                {
                    state = flow_state;
                    finished = result.is_ok();
                    break;
                }
            }
            if !finished {
                let msg = if state.is_empty() {
                    "sign-in timed out".to_string()
                } else {
                    "sign-in failed".to_string()
                };
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        app.set_setup_error(msg.into());
                    }
                })
                .ok();
                return;
            }
            // Retrieve the exchanged credential set (single-use).
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = handle
                .commands
                .send(Command {
                    id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                    origin: FrontendKind::Gui,
                    payload: CommandPayload::CompleteOAuth2Flow { state, reply: tx },
                })
                .await;
            let Ok(Reply::OAuthTokens(tokens)) = rx.await else {
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        app.set_setup_error("sign-in could not be completed".into());
                    }
                })
                .ok();
                return;
            };
            // Link the account (upserts by email — this is also the
            // re-auth path for an existing account in NeedsReauth).
            let mut config = provider_preset(&provider, &email);
            config.auth_kind = "oauth2".into();
            config.username = Some(config.email.clone());
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = handle
                .commands
                .send(Command {
                    id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                    origin: FrontendKind::Gui,
                    payload: CommandPayload::AddAccount {
                        config,
                        password: tokens,
                        reply: tx,
                    },
                })
                .await;
            let linked = matches!(rx.await, Ok(Reply::Accounts(_)));
            slint::invoke_from_event_loop(move || {
                if let Some(app) = w.upgrade() {
                    if linked {
                        app.set_setup_testing_status("Account connected".into());
                        app.set_show_setup(false);
                        app.set_status_text("OAuth2 account connected".into());
                    } else {
                        app.set_setup_error("could not save the account".into());
                    }
                    app.set_setup_busy(false);
                }
            })
            .ok();
        });
    });
}
