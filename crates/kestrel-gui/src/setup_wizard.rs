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
}

// ────────────────────── helpers ──────────────────────

/// Fetch the full folder list for all accounts and populate the UI.
async fn fetch_all_folders(state: &GuiState) -> Option<Vec<FolderId>> {
    let h = &state.handle;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = h
        .commands
        .send(Command {
            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
            origin: FrontendKind::Gui,
            payload: CommandPayload::ListAccounts { reply: tx },
        })
        .await;
    let Reply::Accounts(accounts) = rx.await.ok()? else {
        return None;
    };

    let mut all_folder_names: Vec<String> = Vec::new();
    let mut all_folder_ids: Vec<FolderId> = Vec::new();
    let mut all_folder_unreads: Vec<i32> = Vec::new();
    let acct_names: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
    let acct_emails: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();

    // Unified Inbox as the first virtual folder.
    all_folder_names.push("Unified Inbox".into());
    all_folder_ids.push(FolderId::from_uuid(uuid::Uuid::nil()));
    all_folder_unreads.push(0);

    {
        if let Ok(mut cached) = state.account_ids_cache.lock() {
            *cached = accounts.iter().map(|a| a.id).collect();
        }
    }
    {
        if let Ok(mut cached) = state.account_emails_cache.lock() {
            (*cached).clone_from(&acct_emails);
        }
    }

    for acct in &accounts {
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

    let count = accounts.len();
    let names_clone = all_folder_names.clone();
    let unreads_clone = all_folder_unreads.clone();
    let acct_colors_clone = acct_emails.clone();

    let weak = state.app_weak.clone();
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
    let fids = Arc::clone(&state.folder_ids);
    let state_clone = GuiState {
        app_weak: state.app_weak.clone(),
        handle: state.handle.clone(),
        config: Arc::clone(&state.config),
        paths: Arc::clone(&state.paths),
        vp_state: crate::state::SharedViewportState::clone(&state.vp_state),
        folder_ids: Arc::clone(&state.folder_ids),
        message_ids: Arc::clone(&state.message_ids),
        account_ids_cache: Arc::clone(&state.account_ids_cache),
        account_emails_cache: Arc::clone(&state.account_emails_cache),
        current_attachment_keys: Arc::clone(&state.current_attachment_keys),
        current_message_for_attachments: Arc::clone(&state.current_message_for_attachments),
        current_message_html: Arc::clone(&state.current_message_html),
        reply_in_reply_to: Arc::clone(&state.reply_in_reply_to),
        reply_references: Arc::clone(&state.reply_references),
        pending_compose_attachments: Arc::clone(&state.pending_compose_attachments),
        unread_count: Arc::clone(&state.unread_count),
    };
    std::thread::spawn(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            if let Some(ids) = fetch_all_folders(&state_clone).await
                && let Ok(mut f) = fids.lock()
            {
                *f = ids;
            }
        });
    });
}

// ────────────────────── on_select_account ──────────────────────

fn wire_select_account(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let fids = Arc::clone(&state.folder_ids);
    let aids = Arc::clone(&state.account_ids_cache);
    app.on_select_account(move |idx| {
        let idx = usize::try_from(idx).unwrap_or(0);
        let account_id = {
            let ids = aids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match ids.get(idx) {
                Some(id) => *id,
                None => return,
            }
        };
        let h = h.clone();
        let w = w.clone();
        let fids2 = fids.clone();
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
                    if let Ok(mut f) = fids2.lock() {
                        *f = all_folder_ids;
                    }
                }
            });
        });
    });
}

// ────────────────────── on_add_account ──────────────────────

fn wire_add_account(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let fids = Arc::clone(&state.folder_ids);
    let aids = Arc::clone(&state.account_ids_cache);
    let emails_add = Arc::clone(&state.account_emails_cache);

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
        let fids2 = fids.clone();
        let aids2 = aids.clone();
        let emails2 = emails_add.clone();
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
                        {
                            if let Ok(mut cached) = aids2.lock() {
                                *cached = accts.iter().map(|a| a.id).collect();
                            }
                        }
                        {
                            if let Ok(mut cached) = emails2.lock() {
                                (*cached).clone_from(&acct_emails);
                            }
                        }
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
                        if let Ok(mut f) = fids2.lock() {
                            *f = all_folder_ids;
                        }
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
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_setup_testing_status(
                                    "Waiting for browser authentication...".into(),
                                );
                                app.set_setup_busy(false);
                            }
                        })
                        .ok();
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
