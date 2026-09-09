//! Settings panel callbacks: theme selection, save, account edit/remove,
//! per-account notification toggles.

use std::sync::Arc;

use kestrel_core::{
    ids::AccountId,
    protocol::{Command, CommandPayload, FrontendKind, Reply},
};
use slint::{ComponentHandle as _, Model as _};

use crate::{
    state::GuiState,
    util::{account_color_for_index, hex_to_slint_color, show_toast},
};

/// Wire all settings callbacks.
pub(crate) fn install(state: &GuiState) {
    let Some(app) = state.app_weak.upgrade() else {
        return;
    };

    wire_theme_select(state, &app);
    wire_save_settings(state, &app);
    wire_close_settings(state, &app);
    wire_account_notification_toggles(state, &app);
    wire_edit_account(state, &app);
    wire_remove_account(state, &app);
}

// ────────────────────── theme select ──────────────────────

fn wire_theme_select(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_select_settings_theme(move |idx| {
        let theme = match idx {
            1 => "catppuccin-mocha",
            2 => "catppuccin-latte",
            3 => "catppuccin-macchiato",
            4 => "catppuccin-frappe",
            5 => "dracula",
            6 => "gruvbox-dark",
            7 => "solarized-dark",
            8 => "solarized-light",
            _ => "auto",
        };
        if let Some(app) = w.upgrade() {
            app.set_status_text(format!("Theme: {theme}").into());
        }
    });
}

// ────────────────────── save settings ──────────────────────

fn wire_save_settings(state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    let cfg = Arc::clone(&state.config);
    let paths_clone = Arc::clone(&state.paths);

    app.on_save_settings({
        let w = w.clone();
        move || {
            let Some(app) = w.upgrade() else { return };
            let mut new_cfg = (*cfg).clone();
            // Theme
            new_cfg.general.theme = match app.get_settings_theme_idx() {
                1 => "catppuccin-mocha".to_string(),
                2 => "catppuccin-latte".to_string(),
                3 => "catppuccin-macchiato".to_string(),
                4 => "catppuccin-frappe".to_string(),
                5 => "dracula".to_string(),
                6 => "gruvbox-dark".to_string(),
                7 => "solarized-dark".to_string(),
                8 => "solarized-light".to_string(),
                _ => "auto".to_string(),
            };
            // Notifications
            new_cfg.notifications.enabled = app.get_settings_notifications_enabled();
            new_cfg.notifications.show_subject = app.get_settings_notifications_show_subject();
            // Per-account notification settings
            let acct_emails = app.get_settings_account_emails();
            let acct_notif_enabled: Vec<bool> = app
                .get_settings_account_notifications_enabled()
                .iter()
                .collect();
            let acct_notif_subject: Vec<bool> = app
                .get_settings_account_notifications_show_subject()
                .iter()
                .collect();
            let acct_notif_mute: Vec<bool> = app
                .get_settings_account_notifications_mute()
                .iter()
                .collect();
            for (i, email_val) in acct_emails.iter().enumerate() {
                let email_str = email_val.to_string();
                if email_str.is_empty() {
                    continue;
                }
                let mut notif_cfg = kestrel_core::config::AccountNotificationConfig::default();
                if i < acct_notif_enabled.len() {
                    notif_cfg.enabled = acct_notif_enabled[i];
                }
                if i < acct_notif_subject.len() {
                    notif_cfg.show_subject = acct_notif_subject[i];
                }
                if i < acct_notif_mute.len() {
                    notif_cfg.mute = acct_notif_mute[i];
                }
                new_cfg.account_notifications.insert(email_str, notif_cfg);
            }
            // Sync
            let idle_str = app.get_settings_idle_timeout().to_string();
            if let Ok(idle) = idle_str.parse::<u64>() {
                new_cfg.sync.idle_timeout_mins = idle;
            }
            let poll_str = app.get_settings_poll_interval().to_string();
            if let Ok(poll) = poll_str.parse::<u64>() {
                new_cfg.sync.poll_interval_secs = poll;
            }
            // Signature template
            let sig = app.get_settings_signature().to_string();
            if !sig.is_empty() {
                new_cfg.templates.insert("signature".to_string(), sig);
            }
            // Persist
            let file = paths_clone.config_file();
            if let Some(parent) = file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match toml::to_string_pretty(&new_cfg) {
                Ok(text) => {
                    if let Err(e) = std::fs::write(&file, text) {
                        show_toast(&app, &format!("Failed to save: {e}"), "error");
                    } else {
                        show_toast(&app, "Settings saved", "success");
                    }
                }
                Err(e) => {
                    show_toast(&app, &format!("Serialization error: {e}"), "error");
                }
            }
        }
    });
}

fn wire_close_settings(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_close_settings(move || {
        if let Some(app) = w.upgrade() {
            app.set_show_settings(false);
        }
    });
}

// ────────────────────── per-account notification toggles ──────────────────────

fn wire_account_notification_toggles(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_toggle_account_notif_enabled({
        let w = w.clone();
        move |idx| {
            let idx = usize::try_from(idx).unwrap_or(0);
            if let Some(app) = w.upgrade() {
                let mut vals: Vec<bool> = app
                    .get_settings_account_notifications_enabled()
                    .iter()
                    .collect();
                if idx < vals.len() {
                    vals[idx] = !vals[idx];
                    app.set_settings_account_notifications_enabled(vals.as_slice().into());
                }
            }
        }
    });
    let w = app.as_weak();
    app.on_toggle_account_notif_subject({
        let w = w.clone();
        move |idx| {
            let idx = usize::try_from(idx).unwrap_or(0);
            if let Some(app) = w.upgrade() {
                let mut vals: Vec<bool> = app
                    .get_settings_account_notifications_show_subject()
                    .iter()
                    .collect();
                if idx < vals.len() {
                    vals[idx] = !vals[idx];
                    app.set_settings_account_notifications_show_subject(vals.as_slice().into());
                }
            }
        }
    });
    let w = app.as_weak();
    app.on_toggle_account_notif_mute({
        let w = w.clone();
        move |idx| {
            let idx = usize::try_from(idx).unwrap_or(0);
            if let Some(app) = w.upgrade() {
                let mut vals: Vec<bool> = app
                    .get_settings_account_notifications_mute()
                    .iter()
                    .collect();
                if idx < vals.len() {
                    vals[idx] = !vals[idx];
                    app.set_settings_account_notifications_mute(vals.as_slice().into());
                }
            }
        }
    });
}

// ────────────────────── edit account ──────────────────────

fn wire_edit_account(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_edit_account(move |idx| {
        let Some(app) = w.upgrade() else { return };
        let idx = usize::try_from(idx).unwrap_or(0);
        let emails = app.get_settings_account_emails();
        let email = emails
            .iter()
            .nth(idx)
            .map(|e| e.to_string())
            .unwrap_or_default();
        if email.is_empty() {
            show_toast(&app, "Account not found", "error");
            return;
        }
        let hosts = app.get_settings_account_hosts();
        let host = hosts
            .iter()
            .nth(idx)
            .map(|h| h.to_string())
            .unwrap_or_default();
        app.set_editing_account(true);
        app.set_editing_account_email(slint::SharedString::from(email.as_str()));
        app.set_setup_email(slint::SharedString::from(email.as_str()));
        app.set_setup_password(slint::SharedString::default());
        app.set_setup_imap_host(slint::SharedString::from(host.as_str()));
        app.set_setup_smtp_host(slint::SharedString::default());
        app.set_setup_step(1);
        app.set_setup_error(slint::SharedString::default());
        app.set_setup_busy(false);
        app.set_show_settings(false);
        app.set_show_setup(true);
        let provider = kestrel_core::provider::detect_provider(&email);
        let name = kestrel_core::provider::provider_display_name(&provider);
        let config = kestrel_core::provider::provider_preset(&provider, &email);
        let hosts_str = format!(
            "IMAP: {}:{} | SMTP: {}:{}",
            config.imap_host, config.imap_port, config.smtp_host, config.smtp_port
        );
        let is_oauth2 = kestrel_core::provider::provider_supports_oauth2(&provider);
        let button_label = if is_oauth2 {
            kestrel_core::provider::provider_oauth2_button_label(&provider)
        } else {
            ""
        };
        app.set_provider_name(name.into());
        app.set_setup_provider_name(name.into());
        app.set_setup_detected_hosts(hosts_str.clone().into());
        app.set_setup_is_oauth2(is_oauth2);
        app.set_setup_oauth2_button_label(button_label.into());
        app.set_setup_email_valid(true);
        if host.is_empty() {
            app.set_setup_imap_host(format!("{}:{}", config.imap_host, config.imap_port).into());
            app.set_setup_smtp_host(format!("{}:{}", config.smtp_host, config.smtp_port).into());
        }
    });
}

// ────────────────────── remove account ──────────────────────

fn wire_remove_account(state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    let h = state.handle.clone();
    let aids = Arc::clone(&state.account_ids_cache);
    let emails_remove = Arc::clone(&state.account_emails_cache);

    app.on_remove_account(move |idx| {
        let Some(app) = w.upgrade() else { return };
        let idx = usize::try_from(idx).unwrap_or(0);
        let account_id = {
            let ids = aids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ids.get(idx).copied()
        };
        let Some(account_id) = account_id else {
            show_toast(&app, "Account not found", "error");
            return;
        };
        let names = app.get_settings_account_names();
        let name = names
            .iter()
            .nth(idx)
            .map(|n| n.to_string())
            .unwrap_or_default();
        let h2 = h.clone();
        let w2 = w.clone();
        let aids2 = Arc::clone(&aids);
        let emails2 = Arc::clone(&emails_remove);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = h2
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::RemoveAccount {
                            account: account_id,
                            reply: tx,
                        },
                    })
                    .await;
                match rx.await {
                    Ok(Reply::Accepted) => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                show_toast(&app, &format!("Removed account: {name}"), "success");
                                // Refresh account list
                                let h3 = h2.clone();
                                let w3 = w2.clone();
                                let aids3 = Arc::clone(&aids2);
                                std::thread::spawn(move || {
                                    let rt = tokio::runtime::Handle::current();
                                    rt.block_on(async move {
                                        let (tx, rx) = tokio::sync::oneshot::channel();
                                        let _ = h3
                                            .commands
                                            .send(Command {
                                                id: kestrel_core::ids::RequestId::from_uuid(
                                                    uuid::Uuid::now_v7(),
                                                ),
                                                origin: FrontendKind::Gui,
                                                payload: CommandPayload::ListAccounts { reply: tx },
                                            })
                                            .await;
                                        if let Ok(Reply::Accounts(accts)) = rx.await {
                                            let acct_names: Vec<slint::SharedString> = accts
                                                .iter()
                                                .map(|a| slint::SharedString::from(a.name.as_str()))
                                                .collect();
                                            let acct_email_strs: Vec<String> =
                                                accts.iter().map(|a| a.email.clone()).collect();
                                            let acct_emails: Vec<slint::SharedString> =
                                                acct_email_strs
                                                    .iter()
                                                    .map(|s| slint::SharedString::from(s.as_str()))
                                                    .collect();
                                            let acct_hosts: Vec<slint::SharedString> = accts
                                                .iter()
                                                .map(|a| slint::SharedString::from(a.host.as_str()))
                                                .collect();
                                            let ids: Vec<AccountId> =
                                                accts.iter().map(|a| a.id).collect();
                                            {
                                                if let Ok(mut cached) = emails2.lock() {
                                                    *cached = acct_email_strs;
                                                }
                                            }
                                            slint::invoke_from_event_loop(move || {
                                                if let Some(app) = w3.upgrade() {
                                                    app.set_settings_account_names(
                                                        acct_names.as_slice().into(),
                                                    );
                                                    app.set_settings_account_emails(
                                                        acct_emails.as_slice().into(),
                                                    );
                                                    app.set_settings_account_hosts(
                                                        acct_hosts.as_slice().into(),
                                                    );
                                                    app.set_account_count(
                                                        i32::try_from(accts.len()).unwrap_or(0),
                                                    );
                                                    app.set_account_names(
                                                        acct_names.as_slice().into(),
                                                    );
                                                    // Reset re-auth badges to the
                                                    // authoritative account states.
                                                    let reauth: Vec<bool> = accts
                                                        .iter()
                                                        .map(|a| {
                                                            a.state
                                                                == kestrel_core::protocol::ConnectionState::NeedsReauth
                                                        })
                                                        .collect();
                                                    app.set_account_needs_reauth(
                                                        reauth.as_slice().into(),
                                                    );
                                                    let colors: Vec<slint::Color> = accts
                                                        .iter()
                                                        .enumerate()
                                                        .map(|(i, _)| {
                                                            hex_to_slint_color(
                                                                account_color_for_index(i),
                                                            )
                                                        })
                                                        .collect();
                                                    app.set_account_colors(
                                                        colors.as_slice().into(),
                                                    );
                                                    if let Ok(mut cached) = aids3.lock() {
                                                        *cached = ids;
                                                    }
                                                }
                                            })
                                            .ok();
                                        }
                                    });
                                });
                            }
                        })
                        .ok();
                    }
                    Ok(Reply::Err(e)) => {
                        let msg = e.user_message();
                        let w2e = w2.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2e.upgrade() {
                                show_toast(&app, &msg, "error");
                            }
                        })
                        .ok();
                    }
                    _ => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                show_toast(&app, "Failed to remove account", "error");
                            }
                        })
                        .ok();
                    }
                }
            });
        });
    });
}
