//! `kestrel-gui` binary: Slint event loop on the main thread, tokio
//! runtime for the engine, sandboxed wry viewport for HTML bodies.
//! First run shows an account setup wizard; after account creation the
//! main 3-pane UI appears and syncs.

#![allow(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::let_unit_value,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

mod compose;
mod events;
mod navigation;
mod settings;
mod setup_wizard;
mod state;
mod util;

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32},
};

use events::ForwardedEvent;
use kestrel_core::{
    config::Config,
    ids::{AccountId, FolderId, MessageId},
    paths::Paths,
    protocol::{Command, CommandPayload, FrontendKind, Reply},
};
use kestrel_gui::{AppWindow, SharedViewportState, ViewportState};
use slint::{ComponentHandle, Model as _};
use state::GuiState;
use util::show_toast;

fn main() {
    // ── Logging ──
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    let _subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // ── Paths + config ──
    let paths = match Paths::from_xdg() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("kestrel-gui: paths failed: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = paths.ensure() {
        eprintln!("kestrel-gui: dirs failed: {e}");
        std::process::exit(1);
    }
    let loaded = match Config::load(&paths) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("kestrel-gui: config: {e}");
            std::process::exit(1);
        }
    };
    let config = loaded.config;

    // ── App window ──
    let app = AppWindow::new().unwrap_or_else(|e| {
        eprintln!("kestrel-gui: UI: {e}");
        std::process::exit(1);
    });

    // Populate template names in the compose UI
    {
        let template_names: Vec<slint::SharedString> = config
            .templates
            .keys()
            .map(|k| slint::SharedString::from(k.as_str()))
            .collect();
        app.set_template_names(template_names.as_slice().into());
    }

    // ── Shared state ──
    let vp_state: SharedViewportState = Arc::new(std::sync::Mutex::new(ViewportState::default()));
    let folder_ids: Arc<std::sync::Mutex<Vec<FolderId>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let message_ids: Arc<std::sync::Mutex<Vec<MessageId>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let account_ids_cache: Arc<std::sync::Mutex<Vec<AccountId>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let account_emails_cache: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let current_attachment_keys: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let current_message_for_attachments: Arc<std::sync::Mutex<Option<MessageId>>> =
        Arc::new(std::sync::Mutex::new(None));
    let current_message_html: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let reply_in_reply_to: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let reply_references: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let pending_compose_attachments: Arc<
        std::sync::Mutex<Vec<kestrel_core::protocol::DraftAttachment>>,
    > = Arc::new(std::sync::Mutex::new(Vec::new()));
    let unread_count: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));

    // ── Tokio runtime + engine spawn ──
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let (engine_tx, engine_rx) = std::sync::mpsc::channel();
    let gui_weak = app.as_weak();
    let vp2 = Arc::clone(&vp_state);
    let ec = Arc::clone(&config);
    let ep = Arc::clone(&paths);
    let _engine_ready = Arc::new(AtomicBool::new(false));
    let unread_for_thread = Arc::clone(&unread_count);
    let account_ids_for_events = Arc::clone(&account_ids_cache);

    std::thread::spawn(move || {
        rt.block_on(async move {
            let handle = match kestrel_engine::Engine::spawn(ec, ep).await {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("kestrel-gui: engine: {e}");
                    return;
                }
            };
            let _ = engine_tx.send(handle.clone());
            let mut events = handle.events();
            let ids_for_events = Arc::clone(&account_ids_for_events);
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        let vp = Arc::clone(&vp2);
                        let unread = Arc::clone(&unread_for_thread);
                        let ids = Arc::clone(&ids_for_events);
                        let fwd = ForwardedEvent(ev);
                        gui_weak
                            .upgrade_in_event_loop(move |app| {
                                fwd.apply(&app, &vp, &unread, &ids);
                            })
                            .ok();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    });

    let handle = engine_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| {
            eprintln!("kestrel-gui: engine timeout");
            std::process::exit(1);
        });

    // ── Build GuiState ──
    let state = GuiState::new(
        app.as_weak(),
        handle.clone(),
        Arc::clone(&config),
        Arc::clone(&paths),
        Arc::clone(&vp_state),
        Arc::clone(&folder_ids),
        Arc::clone(&message_ids),
        Arc::clone(&account_ids_cache),
        Arc::clone(&account_emails_cache),
        Arc::clone(&current_attachment_keys),
        Arc::clone(&current_message_for_attachments),
        Arc::clone(&current_message_html),
        Arc::clone(&reply_in_reply_to),
        Arc::clone(&reply_references),
        Arc::clone(&pending_compose_attachments),
        Arc::clone(&unread_count),
    );

    // ── Install callback modules ──
    setup_wizard::install(&state);
    navigation::install(&state);
    compose::install(&state);
    settings::install(&state);

    // ── Command palette ──
    wire_command_palette(&state, &app);

    // ── System tray ──
    #[cfg(feature = "tray")]
    {
        events::setup_tray(&app, &handle, &unread_count);
    }

    // ── Slint event loop ──
    app.run().unwrap_or_else(|e| {
        eprintln!("kestrel-gui: {e}");
        std::process::exit(1);
    });

    // Ordered engine shutdown (architecture §3.3)
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        rt.block_on(handle.shutdown(true));
    }
}

// ────────────────────── command palette ──────────────────────

fn wire_command_palette(state: &GuiState, app: &AppWindow) {
    let commands: Arc<Vec<(&str, &str)>> = Arc::new(vec![
        ("Compose New Message", "compose"),
        ("Reply to Message", "reply"),
        ("Reply All to Message", "reply-all"),
        ("Forward Message", "forward"),
        ("Delete Message", "delete"),
        ("Archive Message", "archive"),
        ("Flag Message", "flag"),
        ("Search Messages", "search"),
        ("Select Next Message", "next"),
        ("Select Previous Message", "prev"),
        ("Open HTML View", "html"),
        ("Show Calendar", "show-calendar"),
        ("Show Contacts", "show-contacts"),
        ("Close Compose", "close-compose"),
        ("Close Calendar", "close-calendar"),
        ("Close Contacts", "close-contacts"),
        ("Sync Now", "sync"),
        ("Add Account", "add-account"),
        ("Settings", "settings"),
    ]);

    // on_show_command_palette
    {
        let w = app.as_weak();
        let cmds = Arc::clone(&commands);
        app.on_show_command_palette(move || {
            let Some(app) = w.upgrade() else { return };
            let visible = app.get_show_command_palette_active();
            if visible {
                app.set_show_command_palette_active(false);
                app.set_command_palette_input(slint::SharedString::default());
                app.set_command_palette_results(vec![].as_slice().into());
            } else {
                let all: Vec<slint::SharedString> = cmds
                    .iter()
                    .map(|(label, _)| slint::SharedString::from(*label))
                    .collect();
                app.set_command_palette_results(all.as_slice().into());
                app.set_command_palette_input(slint::SharedString::default());
                app.set_show_command_palette_active(true);
            }
        });
    }

    // on_search_commands
    {
        let w = app.as_weak();
        let cmds = Arc::clone(&commands);
        app.on_search_commands(move |query| {
            let Some(app) = w.upgrade() else { return };
            let q = query.to_lowercase();
            let filtered: Vec<slint::SharedString> = cmds
                .iter()
                .filter(|(label, _)| q.is_empty() || label.to_lowercase().contains(&q))
                .map(|(label, _)| slint::SharedString::from(*label))
                .collect();
            app.set_command_palette_results(filtered.as_slice().into());
        });
    }

    // on_execute_command
    {
        let w = app.as_weak();
        let h_exec = state.handle.clone();
        let cfg_exec = Arc::clone(&state.config);
        let cmds = Arc::clone(&commands);
        app.on_execute_command(move |command_label| {
            let Some(app) = w.upgrade() else { return };
            app.set_show_command_palette_active(false);
            app.set_command_palette_input(slint::SharedString::default());
            app.set_command_palette_results(vec![].as_slice().into());

            let label = command_label.to_string();
            let action = cmds
                .iter()
                .find(|(l, _)| *l == label)
                .map_or("", |(_, a)| *a);
            match action {
                "compose" => {
                    app.set_show_compose(true);
                    app.set_compose_error(slint::SharedString::default());
                }
                "reply" | "reply-all" | "forward" => app.invoke_compose(),
                "delete" => app.invoke_delete_message(),
                "archive" => app.invoke_archive_message(),
                "flag" => app.invoke_flag_message(),
                "search" => {
                    app.set_search_query(slint::SharedString::default());
                }
                "next" => app.invoke_select_next_message(),
                "prev" => app.invoke_select_prev_message(),
                "html" => app.invoke_open_html_view(),
                "show-calendar" => app.invoke_open_calendar(),
                "show-contacts" => app.invoke_open_contacts(),
                "close-compose" => {
                    app.set_show_compose(false);
                }
                "close-calendar" => app.invoke_close_calendar(),
                "close-contacts" => app.invoke_close_contacts(),
                "sync" => show_toast(&app, "Sync triggered", "info"),
                "add-account" => {
                    app.set_show_setup(true);
                    app.set_setup_error(slint::SharedString::default());
                }
                "settings" => {
                    app.set_show_settings(true);
                    let cfg = cfg_exec.clone();
                    app.set_settings_idle_timeout(slint::SharedString::from(
                        cfg.sync.idle_timeout_mins.to_string().as_str(),
                    ));
                    app.set_settings_poll_interval(slint::SharedString::from(
                        cfg.sync.poll_interval_secs.to_string().as_str(),
                    ));
                    app.set_settings_notifications_enabled(cfg.notifications.enabled);
                    app.set_settings_notifications_show_subject(cfg.notifications.show_subject);
                    let theme_idx = match cfg.general.theme.as_str() {
                        "catppuccin-mocha" | "catppuccin_mocha" => 1,
                        "catppuccin-latte" | "catppuccin_latte" => 2,
                        "catppuccin-macchiato" | "catppuccin_macchiato" => 3,
                        "catppuccin-frappe" | "catppuccin_frappe" => 4,
                        "dracula" => 5,
                        "gruvbox-dark" | "gruvbox_dark" => 6,
                        "solarized-dark" | "solarized_dark" => 7,
                        "solarized-light" | "solarized_light" => 8,
                        _ => 0,
                    };
                    app.set_settings_theme_idx(theme_idx);
                    let sig = cfg.templates.get("signature").cloned().unwrap_or_default();
                    app.set_settings_signature(slint::SharedString::from(sig.as_str()));
                    let tmpl_names: Vec<slint::SharedString> = cfg
                        .templates
                        .keys()
                        .map(|k| slint::SharedString::from(k.as_str()))
                        .collect();
                    app.set_settings_template_names(tmpl_names.as_slice().into());
                    {
                        let names = app.get_account_names();
                        let acct_names: Vec<slint::SharedString> = names.iter().collect();
                        app.set_settings_account_names(acct_names.as_slice().into());
                    }
                    // Fetch full account details for settings
                    {
                        let h2 = h_exec.clone();
                        let w2 = app.as_weak();
                        let cfg2 = cfg_exec.clone();
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Handle::current();
                            rt.block_on(async move {
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                let _ = h2
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
                                    let names: Vec<slint::SharedString> = accts
                                        .iter()
                                        .map(|a| slint::SharedString::from(a.name.as_str()))
                                        .collect();
                                    let emails: Vec<slint::SharedString> = accts
                                        .iter()
                                        .map(|a| slint::SharedString::from(a.email.as_str()))
                                        .collect();
                                    let hosts: Vec<slint::SharedString> = accts
                                        .iter()
                                        .map(|a| slint::SharedString::from(a.host.as_str()))
                                        .collect();
                                    let notif_enabled: Vec<bool> = accts
                                        .iter()
                                        .map(|a| {
                                            cfg2.account_notifications
                                                .get(&a.email)
                                                .is_none_or(|n| n.enabled)
                                        })
                                        .collect();
                                    let notif_subject: Vec<bool> = accts
                                        .iter()
                                        .map(|a| {
                                            cfg2.account_notifications
                                                .get(&a.email)
                                                .is_none_or(|n| n.show_subject)
                                        })
                                        .collect();
                                    let notif_mute: Vec<bool> = accts
                                        .iter()
                                        .map(|a| {
                                            cfg2.account_notifications
                                                .get(&a.email)
                                                .is_some_and(|n| n.mute)
                                        })
                                        .collect();
                                    slint::invoke_from_event_loop(move || {
                                        if let Some(app) = w2.upgrade() {
                                            app.set_settings_account_names(names.as_slice().into());
                                            app.set_settings_account_emails(
                                                emails.as_slice().into(),
                                            );
                                            app.set_settings_account_hosts(hosts.as_slice().into());
                                            app.set_settings_account_notifications_enabled(
                                                notif_enabled.as_slice().into(),
                                            );
                                            app.set_settings_account_notifications_show_subject(
                                                notif_subject.as_slice().into(),
                                            );
                                            app.set_settings_account_notifications_mute(
                                                notif_mute.as_slice().into(),
                                            );
                                        }
                                    })
                                    .ok();
                                }
                            });
                        });
                    }
                }
                _ => {}
            }
        });
    }
}
