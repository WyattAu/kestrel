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

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32},
};

use kestrel_core::{
    clock::Clock as _,
    config::Config,
    ids::{AccountId, FolderId, MessageId},
    paths::Paths,
    protocol::{
        Address, BodyPreference, Command, CommandPayload, Draft, EngineEvent, FrontendKind,
        PartIdView, Reply, SearchQuery, SortSpec, Window,
    },
    provider::{
        detect_provider, provider_display_name, provider_help, provider_oauth2_button_label,
        provider_preset, provider_supports_oauth2,
    },
    secrets::SecretString,
};
use kestrel_gui::{AppWindow, SharedViewportState, ViewportState};
use slint::{ComponentHandle, Model as _};

fn update_contact_suggestions_gui(
    w: &slint::Weak<AppWindow>,
    cache: &Arc<std::sync::Mutex<Vec<kestrel_core::protocol::ContactSummary>>>,
    query: &str,
) {
    let q = query.to_string();
    let suggestions: Vec<slint::SharedString> = {
        let Ok(contacts) = cache.lock() else {
            return;
        };
        contacts
            .iter()
            .filter(|c| {
                let q_lower = q.to_lowercase();
                c.display_name.to_lowercase().contains(&q_lower)
                    || c.email.to_lowercase().contains(&q_lower)
            })
            .map(|c| {
                if c.display_name.is_empty() {
                    slint::SharedString::from(c.email.as_str())
                } else {
                    slint::SharedString::from(format!("{} <{}>", c.display_name, c.email))
                }
            })
            .collect()
    };
    let show = !suggestions.is_empty();
    if let Some(app) = w.upgrade() {
        app.set_contact_suggestions(suggestions.as_slice().into());
        app.set_show_contact_suggestions(show);
    }
}

fn show_toast(app: &AppWindow, message: &str, toast_type: &str) {
    app.set_show_toast(true);
    app.set_toast_message(slint::SharedString::from(message));
    app.set_toast_type(slint::SharedString::from(toast_type));
    let w = app.as_weak();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        slint::invoke_from_event_loop(move || {
            if let Some(app) = w.upgrade() {
                app.set_show_toast(false);
            }
        })
        .ok();
    });
}

const ACCOUNT_COLOR_PALETTE: [&str; 8] = [
    "#f38ba8", "#a6e3a1", "#89b4fa", "#f9e2af", "#cba6f7", "#94e2d5", "#fab387", "#74c7ec",
];

fn hex_to_slint_color(hex: &str) -> slint::Color {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    slint::Color::from_rgb_u8(r, g, b)
}

fn account_color_for_index(idx: usize) -> &'static str {
    ACCOUNT_COLOR_PALETTE[idx % ACCOUNT_COLOR_PALETTE.len()]
}

fn main() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    let _subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

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

    let vp_state: SharedViewportState = Arc::new(std::sync::Mutex::new(ViewportState::default()));

    // Shared state for folder and message ID tracking (index matches UI list position).
    let folder_ids: Arc<std::sync::Mutex<Vec<FolderId>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let message_ids: Arc<std::sync::Mutex<Vec<MessageId>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Cached account IDs for folder listing after setup.
    let account_ids_cache: Arc<std::sync::Mutex<Vec<AccountId>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Cached account emails for compose from-address lookup.
    let account_emails_cache: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Current message's attachment part keys (for save operations).
    let current_attachment_keys: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Current message ID (for save attachment operations).
    let current_message_for_attachments: Arc<std::sync::Mutex<Option<MessageId>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Current message's raw HTML (for remote content toggle).
    let current_message_html: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Reply context: threading info from the message being replied to.
    let reply_in_reply_to: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let reply_references: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Pending attachments for compose (shared between file-drop and paste handlers).
    let pending_compose_attachments: Arc<
        std::sync::Mutex<Vec<kestrel_core::protocol::DraftAttachment>>,
    > = Arc::new(std::sync::Mutex::new(Vec::new()));

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

    // Shared unread counter for tray tooltip (updated from the event loop).
    let unread_count: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let unread_for_thread = Arc::clone(&unread_count);

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
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        let vp = Arc::clone(&vp2);
                        let unread = Arc::clone(&unread_for_thread);
                        let fwd = ForwardedEvent(ev);
                        gui_weak
                            .upgrade_in_event_loop(move |app| {
                                fwd.apply(&app, &vp, &unread);
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

    // ─── Initial account check → show wizard or start sync ───
    {
        let h = handle.clone();
        let w = app.as_weak();
        let fids = Arc::clone(&folder_ids);
        let aids = Arc::clone(&account_ids_cache);
        let account_emails_cache_ref = Arc::clone(&account_emails_cache);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = h
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::ListAccounts { reply: tx },
                    })
                    .await;
                if let Ok(Reply::Accounts(accounts)) = rx.await {
                    let count = accounts.len();
                    // Cache account IDs and fetch folders for each account.
                    let mut all_folder_names: Vec<String> = Vec::new();
                    let mut all_folder_ids: Vec<FolderId> = Vec::new();
                    let mut all_folder_unreads: Vec<i32> = Vec::new();
                    let acct_names: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
                    let acct_emails: Vec<String> =
                        accounts.iter().map(|a| a.email.clone()).collect();
                    // Prepend unified inbox as the first virtual folder.
                    all_folder_names.push("Unified Inbox".into());
                    all_folder_ids.push(FolderId::from_uuid(uuid::Uuid::nil()));
                    all_folder_unreads.push(0);
                    {
                        if let Ok(mut cached) = aids.lock() {
                            *cached = accounts.iter().map(|a| a.id).collect();
                        }
                    }
                    {
                        if let Ok(mut cached) = account_emails_cache_ref.lock() {
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
                                all_folder_names
                                    .push(format!("{}/{}", acct.name, folder.remote_name));
                                all_folder_ids.push(folder.id);
                                all_folder_unreads.push(i32::try_from(folder.unread).unwrap_or(0));
                            }
                        }
                    }
                    let names_clone = all_folder_names.clone();
                    let unreads_clone = all_folder_unreads.clone();
                    let acct_colors_clone = acct_emails.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w.upgrade() {
                            app.set_account_count(i32::try_from(count).unwrap_or(0));
                            if count > 0 {
                                app.set_show_setup(false);
                                app.set_status_text(
                                    format!("{count} account(s) syncing...").into(),
                                );
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
                    if let Ok(mut f) = fids.lock() {
                        *f = all_folder_ids;
                    }
                }
            });
        });
    }

    // ─── Select account ───
    app.on_select_account({
        let h = handle.clone();
        let w = app.as_weak();
        let fids = Arc::clone(&folder_ids);
        let aids = Arc::clone(&account_ids_cache);
        move |idx| {
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
                        // Prepend unified inbox.
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
        }
    });

    // ─── Setup wizard callback ───
    app.on_add_account({
        let h = handle.clone();
        let w = app.as_weak();
        let fids = Arc::clone(&folder_ids);
        let aids = Arc::clone(&account_ids_cache);
        let emails_add = Arc::clone(&account_emails_cache);
        move |display_name, email, password, imap_host, smtp_host| {
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
            let errors = kestrel_core::provider::validate_account_config(&config);
            if !errors.is_empty() {
                app.set_setup_error(errors.join("; ").into());
                app.set_setup_busy(false);
                return;
            }

            // Move to step 4 (testing) and send TestConnection
            app.set_setup_step(4);
            app.set_setup_testing_status("Testing connection...".into());

            let h2 = h.clone();
            let w2 = w.clone();
            let fids2 = Arc::clone(&fids);
            let aids2 = Arc::clone(&aids);
            let emails2 = Arc::clone(&emails_add);
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
                            // Connection successful, now add the account
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
                                    app.set_setup_step(3); // Go back to review step
                                }
                            })
                            .ok();
                            return;
                        }
                        _ => {
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w2.upgrade() {
                                    app.set_setup_error(
                                        "unexpected reply from connection test".into(),
                                    );
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
                                        .map(|(i, _)| {
                                            hex_to_slint_color(account_color_for_index(i))
                                        })
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
        }
    });

    // ─── Email-changed: detect provider and update UI ───
    app.on_email_changed({
        let w = app.as_weak();
        move |email| {
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
        }
    });

    // ─── Wizard step navigation ───
    app.on_next_step({
        let w = app.as_weak();
        move || {
            let Some(app) = w.upgrade() else { return };
            let step = app.get_setup_step();
            if step < 4 {
                app.set_setup_step(step + 1);
                app.set_setup_error(slint::SharedString::default());
            }
        }
    });

    app.on_prev_step({
        let w = app.as_weak();
        move || {
            let Some(app) = w.upgrade() else { return };
            let step = app.get_setup_step();
            if step > 1 {
                app.set_setup_step(step - 1);
                app.set_setup_error(slint::SharedString::default());
            }
        }
    });

    // ─── OAuth2 browser flow ───
    app.on_start_oauth2_flow({
        let h = handle.clone();
        let w = app.as_weak();
        move || {
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
                            // Open the browser with the authorization URL
                            if let Err(e) = open::that(&url) {
                                tracing::warn!("failed to open browser: {e}");
                            }
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w2.upgrade() {
                                    app.set_setup_testing_status(
                                        "Waiting for browser authentication...".into(),
                                    );
                                    // For now, move to testing step; in a full implementation
                                    // the engine would listen for the callback and exchange tokens.
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
        }
    });

    // ─── Search ───
    app.on_search({
        let h = handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&message_ids);
        move |query| {
            let h = h.clone();
            let w = w.clone();
            let mids = Arc::clone(&mids);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::Search {
                                query: SearchQuery {
                                    text: if query.is_empty() {
                                        None
                                    } else {
                                        Some(query.to_string())
                                    },
                                    ..SearchQuery::default()
                                },
                                reply: tx,
                            },
                        })
                        .await;
                    if let Ok(Reply::SearchResults(hits)) = rx.await {
                        let subjects: Vec<String> = hits
                            .iter()
                            .map(|h| {
                                h.message
                                    .subject
                                    .clone()
                                    .unwrap_or_else(|| "(no subject)".into())
                            })
                            .collect();
                        let froms: Vec<String> = hits
                            .iter()
                            .map(|h| {
                                h.message
                                    .from
                                    .first()
                                    .and_then(|a| a.name.as_deref().or(Some(&*a.email)))
                                    .unwrap_or("(unknown)")
                                    .to_string()
                            })
                            .collect();
                        let dates: Vec<String> = hits
                            .iter()
                            .map(|h| kestrel_core::time::format_datetime(h.message.internal_date))
                            .collect();
                        let ids: Vec<MessageId> = hits.iter().map(|h| h.message.id).collect();
                        let thread_depths: Vec<i32> = hits
                            .iter()
                            .map(|h| i32::from(h.message.in_reply_to.is_some()))
                            .collect();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                let count = i32::try_from(subjects.len()).unwrap_or(i32::MAX);
                                let ss: Vec<slint::SharedString> = subjects
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                let fs: Vec<slint::SharedString> = froms
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                let ds: Vec<slint::SharedString> = dates
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                let td: Vec<i32> = thread_depths;
                                app.set_message_subjects(ss.as_slice().into());
                                app.set_message_froms(fs.as_slice().into());
                                app.set_message_dates(ds.as_slice().into());
                                app.set_thread_depths(td.as_slice().into());
                                app.set_total_messages(count);
                                app.set_selected_msg_idx(-1);
                                app.set_status_text(format!("{count} results").into());
                            }
                            if let Ok(mut mid) = mids.lock() {
                                *mid = ids;
                            }
                        })
                        .ok();
                    }
                });
            });
        }
    });

    // ─── Select folder ───
    app.on_select_folder({
        let h = handle.clone();
        let w = app.as_weak();
        let fids = Arc::clone(&folder_ids);
        let mids = Arc::clone(&message_ids);
        move |idx| {
            let idx = usize::try_from(idx).unwrap_or(0);
            let folder_id = {
                let ids = fids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let is_unified = folder_id == FolderId::from_uuid(uuid::Uuid::nil());
            let h = h.clone();
            let w = w.clone();
            let mids = Arc::clone(&mids);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let w2 = w.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w2.upgrade() {
                            app.set_loading_messages(true);
                        }
                    })
                    .ok();
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let payload = if is_unified {
                        CommandPayload::ListUnifiedInbox {
                            window: Window {
                                offset: 0,
                                limit: 50,
                            },
                            sort: SortSpec::default(),
                            reply: tx,
                        }
                    } else {
                        CommandPayload::ListMessages {
                            folder: folder_id,
                            window: Window {
                                offset: 0,
                                limit: 50,
                            },
                            sort: SortSpec::default(),
                            reply: tx,
                        }
                    };
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload,
                        })
                        .await;
                    if let Ok(Reply::Messages(page)) = rx.await {
                        let subjects: Vec<String> = page
                            .items
                            .iter()
                            .map(|m| m.subject.clone().unwrap_or_else(|| "(no subject)".into()))
                            .collect();
                        let froms: Vec<String> = page
                            .items
                            .iter()
                            .map(|m| {
                                m.from
                                    .first()
                                    .and_then(|a| a.name.as_deref().or(Some(&*a.email)))
                                    .unwrap_or("(unknown)")
                                    .to_string()
                            })
                            .collect();
                        let dates: Vec<String> = page
                            .items
                            .iter()
                            .map(|m| kestrel_core::time::format_datetime(m.internal_date))
                            .collect();
                        let ids: Vec<MessageId> = page.items.iter().map(|m| m.id).collect();
                        let total = i32::try_from(page.total).unwrap_or(0);
                        let thread_depths: Vec<i32> = page
                            .items
                            .iter()
                            .map(|m| i32::from(m.in_reply_to.is_some()))
                            .collect();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                let ss: Vec<slint::SharedString> = subjects
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                let fs: Vec<slint::SharedString> = froms
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                let ds: Vec<slint::SharedString> = dates
                                    .iter()
                                    .map(|s| slint::SharedString::from(s.as_str()))
                                    .collect();
                                let td: Vec<i32> = thread_depths;
                                app.set_message_subjects(ss.as_slice().into());
                                app.set_message_froms(fs.as_slice().into());
                                app.set_message_dates(ds.as_slice().into());
                                app.set_thread_depths(td.as_slice().into());
                                app.set_total_messages(total);
                                app.set_selected_msg_idx(-1);
                                app.set_preview_from(slint::SharedString::default());
                                app.set_preview_subject(slint::SharedString::default());
                                app.set_preview_body(slint::SharedString::default());
                                app.set_loading_messages(false);
                                app.set_status_text(format!("{total} messages").into());
                            }
                            if let Ok(mut mid) = mids.lock() {
                                *mid = ids;
                            }
                        })
                        .ok();
                    }
                });
            });
        }
    });

    // ─── Select message ───
    app.on_select_message({
        let h = handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&message_ids);
        let att_keys_outer = Arc::clone(&current_attachment_keys);
        let att_msg_outer = Arc::clone(&current_message_for_attachments);
        let html_cache = Arc::clone(&current_message_html);
        move |idx| {
            let idx = usize::try_from(idx).unwrap_or(0);
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            let att_keys = Arc::clone(&att_keys_outer);
            let att_msg = Arc::clone(&att_msg_outer);
            let html_cache2 = Arc::clone(&html_cache);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let w2 = w.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w2.upgrade() {
                            app.set_loading_preview(true);
                        }
                    })
                    .ok();
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::GetMessage {
                                message: message_id,
                                body: BodyPreference::Full,
                                reply: tx,
                            },
                        })
                        .await;
                    if let Ok(Reply::Message(view)) = rx.await {
                        let from = view
                            .summary
                            .from
                            .first()
                            .and_then(|a| a.name.as_deref().or(Some(&*a.email)))
                            .unwrap_or("(unknown)")
                            .to_string();
                        let subject = view.summary.subject.unwrap_or_default();
                        let is_html = view.body_html.is_some() && view.body_plain.is_none();
                        let body = view.body_plain.unwrap_or_default();
                        // Track raw HTML for remote content toggle
                        let raw_html = view.body_html.clone();
                        let remote_blocked = raw_html
                            .as_deref()
                            .map_or(0, kestrel_core::sanitizer::count_remote_refs);
                        {
                            if let Ok(mut cache) = html_cache2.lock() {
                                *cache = raw_html;
                            }
                        }
                        // Extract attachment info from parts
                        let attachments: Vec<(String, String, String)> = view
                            .parts
                            .iter()
                            .filter(|p| {
                                p.disposition.as_deref() == Some("attachment")
                                    || p.filename.is_some()
                            })
                            .map(|p| {
                                let name = p.filename.clone().unwrap_or_else(|| "unnamed".into());
                                #[allow(clippy::cast_precision_loss)]
                                let size = if p.byte_size >= 1_048_576 {
                                    format!("{:.1} MB", p.byte_size as f64 / 1_048_576.0)
                                } else if p.byte_size >= 1024 {
                                    format!("{:.1} KB", p.byte_size as f64 / 1024.0)
                                } else {
                                    format!("{} B", p.byte_size)
                                };
                                (name, size, p.id.key.clone())
                            })
                            .collect();
                        let att_names: Vec<slint::SharedString> = attachments
                            .iter()
                            .map(|(n, _, _)| slint::SharedString::from(n.as_str()))
                            .collect();
                        let att_sizes: Vec<slint::SharedString> = attachments
                            .iter()
                            .map(|(_, s, _)| slint::SharedString::from(s.as_str()))
                            .collect();
                        let att_keys_clone: Vec<String> =
                            attachments.iter().map(|(_, _, k)| k.clone()).collect();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                app.set_preview_from(from.into());
                                app.set_preview_subject(subject.into());
                                app.set_preview_body(body.into());
                                app.set_preview_is_html(is_html);
                                app.set_loading_preview(false);
                                app.set_attachment_names(att_names.as_slice().into());
                                app.set_attachment_sizes(att_sizes.as_slice().into());
                                app.set_remote_blocked_count(
                                    i32::try_from(remote_blocked).unwrap_or(0),
                                );
                                app.set_show_remote_content(false);
                                // Reset find bar
                                app.set_show_find_bar(false);
                                app.set_find_query(slint::SharedString::default());
                                app.set_find_results(0);
                            }
                        })
                        .ok();
                        // Store attachment part keys for save operations
                        {
                            if let Ok(mut keys) = att_keys.lock() {
                                *keys = att_keys_clone;
                            }
                            if let Ok(mut msg) = att_msg.lock() {
                                *msg = Some(message_id);
                            }
                        }
                        // Mark message as read
                        let (tx_read, _rx_read) = tokio::sync::oneshot::channel();
                        let _ = h
                            .commands
                            .send(Command {
                                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                                origin: FrontendKind::Gui,
                                payload: CommandPayload::SetFlags {
                                    messages: vec![message_id],
                                    flags: kestrel_core::protocol::FlagOp::Add(vec![
                                        kestrel_core::protocol::Flag::Seen,
                                    ]),
                                    reply: tx_read,
                                },
                            })
                            .await;
                    }
                });
            });
        }
    });

    app.on_compose({
        let h = handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&message_ids);
        let rirt = Arc::clone(&reply_in_reply_to);
        let rrefs = Arc::clone(&reply_references);
        move || {
            let selected_idx = {
                let Some(app_ref) = w.upgrade() else { return };
                let idx = app_ref.get_selected_msg_idx();
                if idx < 0 {
                    let Some(app) = w.upgrade() else { return };
                    // New message: clear reply context.
                    *rirt
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    rrefs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clear();
                    app.set_show_compose(true);
                    app.set_compose_error(slint::SharedString::default());
                    return;
                }
                usize::try_from(idx).unwrap_or(0)
            };
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(id) = ids.get(selected_idx) {
                    *id
                } else {
                    let Some(app) = w.upgrade() else { return };
                    // No message selected: clear reply context.
                    *rirt
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    rrefs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clear();
                    app.set_show_compose(true);
                    app.set_compose_error(slint::SharedString::default());
                    return;
                }
            };
            let h = h.clone();
            let w = w.clone();
            let rirt2 = rirt.clone();
            let rrefs2 = rrefs.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::GetMessage {
                                message: message_id,
                                body: BodyPreference::Full,
                                reply: tx,
                            },
                        })
                        .await;
                    match rx.await {
                        Ok(Reply::Message(view)) => {
                            let sender = view
                                .summary
                                .from
                                .first()
                                .map(|a| a.name.as_deref().unwrap_or(&a.email).to_string())
                                .unwrap_or_default();
                            let sender_email = view
                                .summary
                                .from
                                .first()
                                .map(|a| a.email.clone())
                                .unwrap_or_default();
                            let subject = view.summary.subject.unwrap_or_default();
                            let date =
                                kestrel_core::time::format_datetime(view.summary.internal_date);
                            let plain = view.body_plain.unwrap_or_default();
                            let quoted: String = plain
                                .lines()
                                .map(|line| format!("> {line}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            let compose_body = format!("\n\nOn {date}, {sender} wrote:\n{quoted}");
                            let compose_to = if sender_email.is_empty() {
                                String::new()
                            } else {
                                format!("{sender} <{sender_email}>")
                            };
                            let compose_subject = format!("Re: {subject}");
                            // Store threading info for reply submission.
                            {
                                let mut irt = rirt2
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                *irt = view
                                    .summary
                                    .in_reply_to
                                    .clone()
                                    .or_else(|| view.summary.message_id.clone());
                            }
                            {
                                let mut refs_vec = rrefs2
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                refs_vec.clear();
                                if let Some(ref irt) = view.summary.in_reply_to {
                                    refs_vec.push(irt.clone());
                                }
                                if let Some(ref mid) = view.summary.message_id
                                    && (refs_vec.is_empty() || refs_vec.last() != Some(mid))
                                {
                                    refs_vec.push(mid.clone());
                                }
                            }
                            // Extract CC from original message for reply-all.
                            let compose_cc: String = view
                                .summary
                                .cc
                                .iter()
                                .map(|a| {
                                    if let Some(ref name) = a.name {
                                        format!("{} <{}>", name, a.email)
                                    } else {
                                        a.email.clone()
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w.upgrade() {
                                    app.set_compose_to(compose_to.into());
                                    app.set_compose_subject(compose_subject.into());
                                    app.set_compose_body(compose_body.into());
                                    app.set_compose_cc(compose_cc.into());
                                    app.set_show_compose(true);
                                    app.set_compose_error(slint::SharedString::default());
                                }
                            })
                            .ok();
                        }
                        Ok(Reply::Err(e)) => {
                            let msg = e.user_message();
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w.upgrade() {
                                    app.set_compose_error(msg.into());
                                    app.set_show_compose(true);
                                }
                            })
                            .ok();
                        }
                        _ => {
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w.upgrade() {
                                    app.set_show_compose(true);
                                    app.set_compose_error(slint::SharedString::default());
                                }
                            })
                            .ok();
                        }
                    }
                });
            });
        }
    });

    app.on_compose_cancel({
        let w = app.as_weak();
        move || {
            if let Some(app) = w.upgrade() {
                app.set_show_compose(false);
                app.set_show_bcc(false);
                app.set_show_schedule_input(false);
                app.set_compose_send_after(0);
            }
        }
    });

    // ─── Compose body paste handler (image paste) ───
    {
        let w = app.as_weak();
        let atts = Arc::clone(&pending_compose_attachments);
        app.on_compose_body_pasted({
            let w = w.clone();
            move |mime_hint| {
                let Some(app) = w.upgrade() else { return };
                let _ = &atts;
                #[cfg(feature = "tray")]
                {
                    let mime_str = mime_hint.to_string();
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => {
                            if let Some(img) = clipboard.get_image().ok() {
                                let width = img.width() as u32;
                                let height = img.height() as u32;
                                let bytes = img.as_bytes();
                                let mime = if mime_str.contains("jpeg") || mime_str.contains("jpg")
                                {
                                    "image/jpeg"
                                } else {
                                    "image/png"
                                };
                                let ext = if mime == "image/jpeg" { "jpg" } else { "png" };
                                let name = format!("pasted-image-{width}x{height}.{ext}");
                                let attachment = kestrel_core::protocol::DraftAttachment {
                                    name: name.clone(),
                                    mime_type: mime.to_string(),
                                    data: bytes.to_vec(),
                                };
                                if let Ok(mut list) = atts.lock() {
                                    list.push(attachment);
                                }
                                // Update attachment names display
                                let mut names: String =
                                    app.get_compose_attachment_names().to_string();
                                if !names.is_empty() {
                                    names.push_str(", ");
                                }
                                names.push_str(&name);
                                app.set_compose_attachment_names(names.into());
                                app.set_status_text(format!("Pasted image: {name}").into());
                            } else {
                                app.set_compose_error("No image found in clipboard".into());
                            }
                        }
                        Err(e) => {
                            tracing::warn!("clipboard access failed: {e}");
                            app.set_compose_error(format!("Clipboard error: {e}").into());
                        }
                    }
                }
                #[cfg(not(feature = "tray"))]
                {
                    let _ = app;
                    let _ = mime_hint;
                }
            }
        });
    }

    // ─── Toggle BCC field visibility ───
    app.on_toggle_bcc({
        let w = app.as_weak();
        move || {
            if let Some(app) = w.upgrade() {
                let current = app.get_show_bcc();
                app.set_show_bcc(!current);
            }
        }
    });

    // ─── Schedule send (hours from now) ───
    app.on_schedule_send({
        let w = app.as_weak();
        move |hours_str| {
            let Some(app) = w.upgrade() else { return };
            let hours_str = hours_str.to_string();
            match hours_str.parse::<f64>() {
                Ok(hours) if hours > 0.0 => {
                    let now_ms = kestrel_core::clock::SystemClock.now_unix_ms();
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let delay_ms = (hours * 3600.0 * 1000.0) as i64;
                    let combined = now_ms.saturating_add(delay_ms);
                    app.set_compose_send_after(i32::try_from(combined).unwrap_or(i32::MAX));
                    app.set_show_schedule_input(false);
                    app.set_status_text(format!("Scheduled: send in {hours} hours").into());
                }
                _ => {
                    app.set_compose_error("invalid hours value".into());
                }
            }
        }
    });

    // ─── Toggle compose preview mode ───
    app.on_toggle_compose_preview({
        let w = app.as_weak();
        let vp = Arc::clone(&vp_state);
        move || {
            let Some(app) = w.upgrade() else { return };
            let current = app.get_compose_preview_mode();
            app.set_compose_preview_mode(!current);
            if !current {
                // Entering preview mode: render markdown as HTML in a viewport
                let body = app.get_compose_body();
                if !body.is_empty() {
                    let html = kestrel_core::compose::markdown_to_html(&body);
                    let wrapped = kestrel_gui::wrap_html_with_csp(&html);
                    let parts = {
                        let state = vp.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.parts_for_display()
                    };
                    if let Err(e) = kestrel_gui::viewport::spawn_wry_viewport(&wrapped, parts) {
                        tracing::warn!("compose preview viewport: {e}");
                    }
                }
            }
        }
    });

    // ─── Contact autocomplete: search on To field edit ───
    {
        let contacts_cache: Arc<std::sync::Mutex<Vec<kestrel_core::protocol::ContactSummary>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let h_contacts = handle.clone();
        let w_contacts = app.as_weak();

        // Pre-load contacts when compose is opened
        app.on_compose_to_edited({
            let h = h_contacts.clone();
            let w = w_contacts.clone();
            let cache = Arc::clone(&contacts_cache);
            move |text| {
                let text_str = text.to_string();
                if text_str.is_empty() {
                    if let Some(app) = w.upgrade() {
                        app.set_show_contact_suggestions(false);
                        app.set_contact_suggestions(vec![].as_slice().into());
                    }
                    return;
                }
                // If cache is empty, fetch contacts first
                let cache_empty = { cache.lock().map_or(true, |c| c.is_empty()) };
                if cache_empty {
                    let h2 = h.clone();
                    let w2 = w.clone();
                    let cache2 = Arc::clone(&cache);
                    let query = text_str.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Handle::current();
                        rt.block_on(async move {
                            // Get first account ID
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
                            let account_id = if let Ok(Reply::Accounts(accts)) = rx.await {
                                accts.first().map(|a| a.id)
                            } else {
                                None
                            };
                            let Some(account_id) = account_id else { return };
                            let (tx2, rx2) = tokio::sync::oneshot::channel();
                            let _ = h2
                                .commands
                                .send(Command {
                                    id: kestrel_core::ids::RequestId::from_uuid(
                                        uuid::Uuid::now_v7(),
                                    ),
                                    origin: FrontendKind::Gui,
                                    payload: CommandPayload::ListContacts {
                                        account: account_id,
                                        reply: tx2,
                                    },
                                })
                                .await;
                            if let Ok(Reply::Contacts(contacts)) = rx2.await {
                                if let Ok(mut c) = cache2.lock() {
                                    *c = contacts;
                                }
                                // Now filter and update UI
                                update_contact_suggestions_gui(&w2, &cache2, &query);
                            }
                        });
                    });
                } else {
                    // Filter cached contacts
                    update_contact_suggestions_gui(&w, &cache, &text_str);
                }
            }
        });

        // Select a contact suggestion
        app.on_select_contact_suggestion({
            let w = w_contacts.clone();
            let cache = Arc::clone(&contacts_cache);
            move |idx| {
                let idx = usize::try_from(idx).unwrap_or(0);
                let entry = { cache.lock().ok().and_then(|c| c.get(idx).cloned()) };
                if let Some(contact) = entry {
                    let email_entry = if contact.display_name.is_empty() {
                        contact.email.clone()
                    } else {
                        format!("{} <{}>", contact.display_name, contact.email)
                    };
                    if let Some(app) = w.upgrade() {
                        let current_to = app.get_compose_to().to_string();
                        let new_to = if current_to.is_empty() {
                            email_entry
                        } else {
                            format!("{current_to}, {email_entry}")
                        };
                        app.set_compose_to(new_to.into());
                        app.set_show_contact_suggestions(false);
                        app.set_contact_suggestions(vec![].as_slice().into());
                    }
                }
            }
        });
    }

    // ─── Apply template ───
    {
        let cfg = Arc::clone(&config);
        app.on_apply_template({
            let w = app.as_weak();
            move |name| {
                let name_str = name.to_string();
                let body = cfg.templates.get(&name_str).cloned().unwrap_or_default();
                if let Some(app) = w.upgrade() {
                    let current = app.get_compose_body().to_string();
                    let new_body = if current.is_empty() {
                        body
                    } else {
                        format!("{current}\n\n{body}")
                    };
                    app.set_compose_body(new_body.into());
                }
            }
        });
    }

    // ─── Set priority ───
    app.on_set_priority({
        let w = app.as_weak();
        move |idx| {
            if let Some(app) = w.upgrade() {
                app.set_compose_priority_idx(idx);
            }
        }
    });

    app.on_compose_submit({
        let h = handle.clone();
        let w = app.as_weak();
        let aids_compose = Arc::clone(&account_ids_cache);
        let emails_compose = Arc::clone(&account_emails_cache);
        let cfg_compose = Arc::clone(&config);
        let rirt_compose = Arc::clone(&reply_in_reply_to);
        let rrefs_compose = Arc::clone(&reply_references);
        let atts_compose = Arc::clone(&pending_compose_attachments);
        move |to, cc, bcc, subject, body| {
            let Some(app) = w.upgrade() else { return };
            app.set_compose_busy(true);
            app.set_compose_error(slint::SharedString::default());

            let to_addrs: Vec<Address> = to
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Address::bare(s.to_string()))
                .collect();
            let cc_addrs: Vec<Address> = if cc.is_empty() {
                vec![]
            } else {
                cc.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| Address::bare(s.to_string()))
                    .collect()
            };
            let bcc_addrs: Vec<Address> = if bcc.is_empty() {
                vec![]
            } else {
                bcc.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| Address::bare(s.to_string()))
                    .collect()
            };
            if to_addrs.is_empty() {
                app.set_compose_error("no recipients".into());
                app.set_compose_busy(false);
                return;
            }

            let pgp_sign = app.get_compose_pgp_sign();
            let pgp_encrypt = app.get_compose_pgp_encrypt();

            // Read schedule value.
            let send_after_val = app.get_compose_send_after();
            let send_after: Option<i64> = if send_after_val > 0 {
                Some(i64::from(send_after_val))
            } else {
                None
            };

            // Read compose-from override.
            let compose_from = app.get_compose_from().to_string();
            let compose_priority_idx = app.get_compose_priority_idx();

            // Map priority index to enum.
            let priority = match compose_priority_idx {
                0 => kestrel_core::protocol::Priority::High,
                2 => kestrel_core::protocol::Priority::Low,
                _ => kestrel_core::protocol::Priority::Normal,
            };

            // Get the currently selected account from the GUI state
            let (account_id, account_email) = {
                let aids = aids_compose
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let emails = emails_compose
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let idx = usize::try_from(app.get_selected_folder_idx()).unwrap_or(0);
                let account_id = aids.get(idx).copied().unwrap_or_else(|| {
                    aids.first()
                        .copied()
                        .unwrap_or_else(|| AccountId::from_uuid(uuid::Uuid::now_v7()))
                });
                let account_email = emails
                    .get(idx)
                    .cloned()
                    .or_else(|| emails.first().cloned())
                    .unwrap_or_default();
                (account_id, account_email)
            };

            let from_email = if compose_from.is_empty() {
                account_email.clone()
            } else {
                compose_from
            };

            // Look up per-account signature and append to body
            let body_with_sig = {
                let sig = cfg_compose
                    .account_signatures
                    .get(&account_email)
                    .cloned()
                    .filter(|s| !s.is_empty());
                if let Some(sig) = sig {
                    format!("{body}\n\n{sig}")
                } else {
                    body.to_string()
                }
            };

            let draft = Draft {
                account: account_id,
                from: Address::bare(from_email),
                to: to_addrs,
                cc: cc_addrs,
                bcc: bcc_addrs,
                subject: subject.to_string(),
                in_reply_to: rirt_compose
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                references: rrefs_compose
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                body_markdown: body_with_sig,
                attachments: atts_compose
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .drain(..)
                    .collect(),
                pgp_sign,
                pgp_encrypt,
                smime_sign: false,
                smime_encrypt: false,
                send_after,
                priority,
            };

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
                            payload: CommandPayload::ComposeSubmit { draft, reply: tx },
                        })
                        .await;
                    match rx.await {
                        Ok(Reply::Accepted) => {
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w2.upgrade() {
                                    app.set_show_compose(false);
                                    app.set_compose_busy(false);
                                    show_toast(&app, "Message queued for sending", "success");
                                }
                            })
                            .ok();
                        }
                        Ok(Reply::Err(e)) => {
                            let msg = e.user_message();
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w2.upgrade() {
                                    show_toast(&app, &msg, "error");
                                    app.set_compose_busy(false);
                                }
                            })
                            .ok();
                        }
                        _ => {
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w2.upgrade() {
                                    show_toast(&app, "Unexpected error sending message", "error");
                                    app.set_compose_busy(false);
                                }
                            })
                            .ok();
                        }
                    }
                });
            });
        }
    });

    // ─── Delete message ───
    app.on_delete_message({
        let h = handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&message_ids);
        move || {
            let selected_idx = {
                let Some(app_ref) = w.upgrade() else { return };
                let idx = app_ref.get_selected_msg_idx();
                if idx < 0 {
                    return;
                }
                usize::try_from(idx).unwrap_or(0)
            };
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(selected_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::DeleteMessages {
                                messages: vec![message_id],
                                expunge: false,
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message deleted", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        }
    });

    // ─── Archive message ───
    app.on_archive_message({
        let h = handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&message_ids);
        let fids = Arc::clone(&folder_ids);
        move || {
            let selected_msg_idx = {
                let Some(app_ref) = w.upgrade() else { return };
                let idx = app_ref.get_selected_msg_idx();
                if idx < 0 {
                    return;
                }
                usize::try_from(idx).unwrap_or(0)
            };
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(selected_msg_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            // Find the Archive folder (first one found)
            let archive_folder_id = {
                let ids = fids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // We don't have role info here, so we'll use a simple heuristic
                // In a real implementation, we'd need to track folder roles
                ids.iter()
                    .find(|id| **id != FolderId::from_uuid(uuid::Uuid::nil()))
                    .copied()
            };
            let Some(dest) = archive_folder_id else {
                let w_err = w.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w_err.upgrade() {
                        show_toast(&app, "No archive folder available", "error");
                    }
                })
                .ok();
                return;
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::MoveMessages {
                                messages: vec![message_id],
                                to: dest,
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message archived", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        }
    });

    // ─── Flag message ───
    app.on_flag_message({
        let h = handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&message_ids);
        move || {
            let selected_idx = {
                let Some(app_ref) = w.upgrade() else { return };
                let idx = app_ref.get_selected_msg_idx();
                if idx < 0 {
                    return;
                }
                usize::try_from(idx).unwrap_or(0)
            };
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(selected_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::SetFlags {
                                messages: vec![message_id],
                                flags: kestrel_core::protocol::FlagOp::Add(vec![
                                    kestrel_core::protocol::Flag::Flagged,
                                ]),
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message flagged", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        }
    });

    // ─── Select next message (j key) ───
    app.on_select_next_message({
        let w = app.as_weak();
        move || {
            let Some(app) = w.upgrade() else { return };
            let total = app.get_total_messages();
            let current = app.get_selected_msg_idx();
            let next = if current < 0 {
                0
            } else if current + 1 < total {
                current + 1
            } else {
                return;
            };
            app.set_selected_msg_idx(next);
        }
    });

    // ─── Select previous message (k key) ───
    app.on_select_prev_message({
        let w = app.as_weak();
        move || {
            let Some(app) = w.upgrade() else { return };
            let current = app.get_selected_msg_idx();
            if current <= 0 {
                return;
            }
            app.set_selected_msg_idx(current - 1);
        }
    });

    // ─── Command palette ───
    {
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

        app.on_show_command_palette({
            let w = app.as_weak();
            let cmds = Arc::clone(&commands);
            move || {
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
            }
        });

        app.on_search_commands({
            let w = app.as_weak();
            let cmds = Arc::clone(&commands);
            move |query| {
                let Some(app) = w.upgrade() else { return };
                let q = query.to_lowercase();
                let filtered: Vec<slint::SharedString> = cmds
                    .iter()
                    .filter(|(label, _)| q.is_empty() || label.to_lowercase().contains(&q))
                    .map(|(label, _)| slint::SharedString::from(*label))
                    .collect();
                app.set_command_palette_results(filtered.as_slice().into());
            }
        });

        app.on_execute_command({
            let w = app.as_weak();
            let h_exec = handle.clone();
            let cfg_exec = Arc::clone(&config);
            move |command_label| {
                let Some(app) = w.upgrade() else { return };
                app.set_show_command_palette_active(false);
                app.set_command_palette_input(slint::SharedString::default());
                app.set_command_palette_results(vec![].as_slice().into());

                let label = command_label.to_string();
                let action = commands
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
                        // Populate settings from config
                        let cfg = cfg_exec.clone();
                        app.set_settings_idle_timeout(slint::SharedString::from(
                            cfg.sync.idle_timeout_mins.to_string().as_str(),
                        ));
                        app.set_settings_poll_interval(slint::SharedString::from(
                            cfg.sync.poll_interval_secs.to_string().as_str(),
                        ));
                        app.set_settings_notifications_enabled(cfg.notifications.enabled);
                        app.set_settings_notifications_show_subject(cfg.notifications.show_subject);
                        // Set theme index
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
                        // Set signature from templates
                        let sig = cfg.templates.get("signature").cloned().unwrap_or_default();
                        app.set_settings_signature(slint::SharedString::from(sig.as_str()));
                        // Set template names
                        let tmpl_names: Vec<slint::SharedString> = cfg
                            .templates
                            .keys()
                            .map(|k| slint::SharedString::from(k.as_str()))
                            .collect();
                        app.set_settings_template_names(tmpl_names.as_slice().into());
                        // Populate account info
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
                                        // Build per-account notification settings arrays
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
                                                app.set_settings_account_names(
                                                    names.as_slice().into(),
                                                );
                                                app.set_settings_account_emails(
                                                    emails.as_slice().into(),
                                                );
                                                app.set_settings_account_hosts(
                                                    hosts.as_slice().into(),
                                                );
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
            }
        });
    }

    // ─── Settings callbacks ───
    {
        let w = app.as_weak();
        let cfg = Arc::clone(&config);
        let paths_clone = Arc::clone(&paths);

        app.on_select_settings_theme({
            let w = w.clone();
            move |idx| {
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
            }
        });

        app.on_save_settings({
            let w = w.clone();
            let cfg = Arc::clone(&cfg);
            let paths = Arc::clone(&paths_clone);
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
                let file = paths.config_file();
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

        app.on_close_settings({
            let w = w.clone();
            move || {
                if let Some(app) = w.upgrade() {
                    app.set_show_settings(false);
                }
            }
        });

        // ─── Per-account notification toggle callbacks ───
        {
            let w = w.clone();
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
                            app.set_settings_account_notifications_show_subject(
                                vals.as_slice().into(),
                            );
                        }
                    }
                }
            });
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

        app.on_edit_account({
            let w = w.clone();
            let _h = handle.clone();
            move |idx| {
                let Some(app) = w.upgrade() else { return };
                let idx = usize::try_from(idx).unwrap_or(0);
                // Get account email from settings list
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
                // Get account host from settings list
                let hosts = app.get_settings_account_hosts();
                let host = hosts
                    .iter()
                    .nth(idx)
                    .map(|h| h.to_string())
                    .unwrap_or_default();
                // Enter edit mode and pre-fill setup wizard
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
                // Trigger provider detection for the pre-filled email
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
                // Pre-fill SMTP host from preset if not already set
                if host.is_empty() {
                    app.set_setup_imap_host(
                        format!("{}:{}", config.imap_host, config.imap_port).into(),
                    );
                    app.set_setup_smtp_host(
                        format!("{}:{}", config.smtp_host, config.smtp_port).into(),
                    );
                }
            }
        });

        app.on_remove_account({
            let w = w.clone();
            let h = handle.clone();
            let aids = Arc::clone(&account_ids_cache);
            let emails_remove = Arc::clone(&account_emails_cache);
            move |idx| {
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
                                        show_toast(
                                            &app,
                                            &format!("Removed account: {name}"),
                                            "success",
                                        );
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
                                                        payload: CommandPayload::ListAccounts {
                                                            reply: tx,
                                                        },
                                                    })
                                                    .await;
                                                if let Ok(Reply::Accounts(accts)) = rx.await {
                                                    let acct_names: Vec<slint::SharedString> =
                                                        accts
                                                            .iter()
                                                            .map(|a| {
                                                                slint::SharedString::from(
                                                                    a.name.as_str(),
                                                                )
                                                            })
                                                            .collect();
                                                    let acct_email_strs: Vec<String> = accts
                                                        .iter()
                                                        .map(|a| a.email.clone())
                                                        .collect();
                                                    let acct_emails: Vec<slint::SharedString> =
                                                        acct_email_strs
                                                            .iter()
                                                            .map(|s| {
                                                                slint::SharedString::from(
                                                                    s.as_str(),
                                                                )
                                                            })
                                                            .collect();
                                                    let acct_hosts: Vec<slint::SharedString> =
                                                        accts
                                                            .iter()
                                                            .map(|a| {
                                                                slint::SharedString::from(
                                                                    a.host.as_str(),
                                                                )
                                                            })
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
                                                                i32::try_from(accts.len())
                                                                    .unwrap_or(0),
                                                            );
                                                            app.set_account_names(
                                                                acct_names.as_slice().into(),
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
            }
        });
    }

    // ─── Wry HTML viewport wiring ───
    app.on_open_html_view({
        let w = app.as_weak();
        let vp = Arc::clone(&vp_state);
        move || {
            if let Some(app) = w.upgrade() {
                let body = app.get_preview_body();
                if !body.is_empty() {
                    let parts = {
                        let state = vp.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.parts_for_display()
                    };
                    if let Err(e) = kestrel_gui::viewport::spawn_wry_viewport(&body, parts) {
                        tracing::warn!("html viewport: {e}");
                    }
                }
            }
        }
    });

    // ─── Toggle remote content (show images) ───
    app.on_toggle_remote_content({
        let w = app.as_weak();
        let html_cache = Arc::clone(&current_message_html);
        move || {
            let Some(app) = w.upgrade() else { return };
            // Get the cached raw HTML
            let raw_html = {
                let Ok(cache) = html_cache.lock() else { return };
                cache.clone()
            };
            let Some(html) = raw_html else { return };
            // Re-sanitize with remote content allowed
            let sanitized = kestrel_core::sanitizer::sanitize_html_body_with_remote(&html, true);
            let wrapped = kestrel_gui::viewport::wrap_html_with_csp(&sanitized.html);
            // Update the preview body with the re-rendered HTML
            app.set_preview_body(slint::SharedString::from(wrapped.as_str()));
            app.set_show_remote_content(true);
            app.set_remote_blocked_count(0);
            app.set_status_text("Remote content loaded".into());
        }
    });

    // ─── Toggle find bar (Ctrl-F) ───
    app.on_toggle_find_bar({
        let w = app.as_weak();
        move || {
            if let Some(app) = w.upgrade() {
                let visible = app.get_show_find_bar();
                app.set_show_find_bar(!visible);
                if visible {
                    app.set_find_query(slint::SharedString::default());
                    app.set_find_results(0);
                }
            }
        }
    });

    // ─── Find in message ───
    app.on_find_in_message({
        let w = app.as_weak();
        move |query| {
            let Some(app) = w.upgrade() else { return };
            let query_str = query.to_string();
            if query_str.is_empty() {
                app.set_find_results(0);
                return;
            }
            let body = app.get_preview_body().to_string();
            let query_lower = query_str.to_lowercase();
            let body_lower = body.to_lowercase();
            let count = body_lower.matches(&query_lower).count();
            app.set_find_results(i32::try_from(count).unwrap_or(i32::MAX));
        }
    });

    // ─── Move message to folder (drag-and-drop) ───
    app.on_move_message_to_folder({
        let h = handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&message_ids);
        let fids = Arc::clone(&folder_ids);
        move |msg_idx, folder_idx| {
            let msg_idx = usize::try_from(msg_idx).unwrap_or(0);
            let dest_folder_idx = usize::try_from(folder_idx).unwrap_or(0);
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(msg_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let dest_folder_id = {
                let ids = fids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(dest_folder_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::MoveMessages {
                                messages: vec![message_id],
                                to: dest_folder_id,
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message moved", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        }
    });

    // ─── Save attachment ───
    app.on_save_attachment({
        let h = handle.clone();
        let w = app.as_weak();
        let att_keys = Arc::clone(&current_attachment_keys);
        let att_msg = Arc::clone(&current_message_for_attachments);
        move |idx| {
            let idx = usize::try_from(idx).unwrap_or(0);
            let (part_key, message_id) = {
                let keys = att_keys
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let msg = att_msg
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match (keys.get(idx), *msg) {
                    (Some(k), Some(m)) => (k.clone(), m),
                    _ => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::GetAttachment {
                                message: message_id,
                                part: PartIdView {
                                    key: part_key.clone(),
                                },
                                reply: tx,
                            },
                        })
                        .await;
                    match rx.await {
                        Ok(Reply::AttachmentData(data)) => {
                            // Use rfd (native file dialog) to pick save location
                            let filename = format!("attachment-{part_key}");
                            let w2 = w.clone();
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w2.upgrade() {
                                    // For now, save to ~/Downloads/ with the part key as name
                                    let home =
                                        std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                                    let path = std::path::PathBuf::from(&home)
                                        .join("Downloads")
                                        .join(&filename);
                                    if let Err(e) = std::fs::write(&path, &data) {
                                        show_toast(&app, &format!("Save failed: {e}"), "error");
                                    } else {
                                        show_toast(
                                            &app,
                                            &format!("Saved to {}", path.display()),
                                            "success",
                                        );
                                    }
                                }
                            })
                            .ok();
                        }
                        Ok(Reply::Err(e)) => {
                            let msg = e.user_message();
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w.upgrade() {
                                    show_toast(&app, &msg, "error");
                                }
                            })
                            .ok();
                        }
                        _ => {
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = w.upgrade() {
                                    show_toast(&app, "Failed to save attachment", "error");
                                }
                            })
                            .ok();
                        }
                    }
                });
            });
        }
    });

    // ─── File-drop handler for compose attachments (C12) ───
    #[cfg(feature = "tray")]
    {
        use kestrel_core::protocol::DraftAttachment;
        let attachments = Arc::clone(&pending_compose_attachments);
        app.on_file_dropped({
            let w = app.as_weak();
            move |path_str| {
                let path = std::path::PathBuf::from(path_str.to_string());
                let file_name = path
                    .file_name()
                    .map_or_else(|| "attachment".into(), |n| n.to_string_lossy().into_owned());
                let mime = mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .to_string();
                match std::fs::read(&path) {
                    Ok(data) => {
                        let attachment = DraftAttachment {
                            name: file_name.clone(),
                            mime_type: mime,
                            data,
                        };
                        if let Ok(mut atts) = attachments.lock() {
                            atts.push(attachment);
                        }
                        if let Some(app) = w.upgrade() {
                            let mut names: String = app.get_compose_attachment_names().to_string();
                            if !names.is_empty() {
                                names.push_str(", ");
                            }
                            names.push_str(&file_name);
                            app.set_compose_attachment_names(names.into());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to read dropped file {path_str}: {e}");
                        if let Some(app) = w.upgrade() {
                            app.set_compose_error(
                                format!("failed to read {file_name}: {e}").into(),
                            );
                        }
                    }
                }
            }
        });

        // ─── DropApi global: wire can-drop / transfer-to-string callbacks ───
        {
            use slint::private_unstable_api::re_exports as sp;

            let drop_api = app.global::<kestrel_gui::DropApi<'_>>();
            drop_api.on_can_drop(|data: sp::DataTransfer| -> sp::DragAction {
                if data.has_plain_text() {
                    sp::DragAction::Copy
                } else {
                    sp::DragAction::None
                }
            });
            drop_api.on_transfer_to_string(|data: sp::DataTransfer| -> sp::SharedString {
                let text = data.plain_text().unwrap_or_default();
                // External file drops provide file URIs; take the first one.
                let uri = text.lines().next().unwrap_or("");
                let path = uri.strip_prefix("file://").unwrap_or(uri);
                sp::SharedString::from(path)
            });
        }
    } // end #[cfg(feature = "tray")]

    // ─── System tray setup ───
    #[cfg(feature = "tray")]
    {
        setup_tray(&app, &handle, &unread_count);
    }

    // ─── Slint event loop (runs until window close) ───
    app.run().unwrap_or_else(|e| {
        eprintln!("kestrel-gui: {e}");
        std::process::exit(1);
    });
}

/// Set up the system tray icon with a context menu.
///
/// Returns the `TrayIcon` so it is not dropped while the event loop runs.
/// On platforms where the underlying toolkit is unavailable (e.g. Linux
/// without GTK), creation fails gracefully and returns `None`.
#[cfg(feature = "tray")]
#[allow(dead_code, unused_variables)]
fn setup_tray(
    app: &AppWindow,
    handle: &kestrel_engine::EngineHandle,
    unread_count: &Arc<AtomicU32>,
) {
    use tray_icon::{
        Icon, TrayIconBuilder,
        menu::{Menu, MenuItem},
    };

    // Minimal 2x2 icon (catppuccin base colour).
    let icon = match Icon::from_rgba(
        vec![
            0x1e, 0x1e, 0x2e, 0xff, 0x31, 0x32, 0x44, 0xff, 0x45, 0x47, 0x5a, 0xff, 0xc0, 0xc0,
            0xc0, 0xff,
        ],
        2,
        2,
    ) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("tray-icon: failed to create icon: {e}");
            return;
        }
    };

    let menu = Menu::new();
    let compose_item = MenuItem::new("Compose", true, None);
    let sync_item = MenuItem::new("Sync Now", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    let _ = menu.append(&compose_item);
    let _ = menu.append(&sync_item);
    let _ = menu.append(&quit_item);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Kestrel — 0 unread")
        .with_icon(icon)
        .with_menu_on_left_click(false)
        .build();

    let tray = match tray {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("tray-icon: build failed (platform may not support tray): {e}");
            return;
        }
    };

    // On Linux, tray-icon requires GTK which is not available with the
    // winit backend. Menu/tray events are forwarded only on macOS/Windows.
    #[cfg(not(target_os = "linux"))]
    {
        use tray_icon::{TrayIconEvent, menu::MenuEvent};

        let gui_weak_ev = app.as_weak();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                let w = gui_weak_ev.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        let visible = app.window().is_visible().unwrap_or(true);
                        let _ = app.window().set_visible(!visible);
                    }
                })
                .ok();
            }
        }));

        let gui_weak_menu = app.as_weak();
        let h_menu = handle.clone();
        let unread = Arc::clone(unread_count);
        MenuEvent::set_event_handler(Some(move |event| {
            if *event.id() == *compose_item.id() {
                let w = gui_weak_menu.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        app.set_show_compose(true);
                        app.set_compose_error(slint::SharedString::default());
                    }
                })
                .ok();
            } else if *event.id() == *sync_item.id() {
                let h = h_menu.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(async move {
                        let (tx, _rx) = tokio::sync::oneshot::channel();
                        let _ = h
                            .commands
                            .send(Command {
                                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                                origin: FrontendKind::Gui,
                                payload: CommandPayload::ResyncState { reply: tx },
                            })
                            .await;
                    });
                });
            } else if *event.id() == *quit_item.id() {
                slint::invoke_from_event_loop(|| {
                    slint::quit_event_loop().ok();
                })
                .ok();
            }
        }));
    }

    #[cfg(target_os = "linux")]
    {
        tracing::info!("tray-icon: menu events not forwarded on Linux (no GTK event loop)");
    }

    // Keep tray alive by leaking it for the process lifetime.
    // The OS reclaims when the process exits.
    Box::leak(Box::new(tray));
}

/// Wrapper that is Send-safe for the event-forwarding thread.
struct ForwardedEvent(EngineEvent);

impl ForwardedEvent {
    fn apply(self, app: &AppWindow, _vp: &SharedViewportState, unread: &Arc<AtomicU32>) {
        match self.0 {
            EngineEvent::EngineStarted { version, .. } => {
                app.set_status_text(format!("Kestrel v{version} ready").into());
            }
            EngineEvent::AccountConnection { state, .. } => {
                app.set_connection_state(format!("{state:?}").into());
            }
            EngineEvent::MailArrived { summary, .. } => {
                app.set_status_text(format!("{} new", summary.new).into());
                app.set_total_messages(
                    app.get_total_messages() + i32::try_from(summary.new).unwrap_or(0),
                );
                // Track unread count for tray tooltip.
                unread.store(
                    u32::try_from(summary.unread).unwrap_or(u32::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
                // OS notification for new mail — respects per-account mute.
                if summary.new > 0 {
                    // Check if notifications are globally enabled via settings.
                    let notifications_enabled = app.get_settings_notifications_enabled();
                    if notifications_enabled
                        && let Err(e) = notify_rust::Notification::new()
                            .summary("Kestrel")
                            .body(&format!("{} new message(s)", summary.new))
                            .appname("Kestrel")
                            .show()
                    {
                        tracing::warn!("notification: {e}");
                    }
                }
            }
            EngineEvent::FolderTreeChanged { .. } => {
                app.set_status_text("folders synced".into());
            }
            EngineEvent::ServiceDegraded { service, error, .. } => {
                app.set_status_text(format!("degraded: {service}: {error}").into());
            }
            EngineEvent::RemoteContentBlocked { count, .. } => {
                app.set_status_text(format!("{count} remote items blocked").into());
            }
            EngineEvent::EventStreamLagged { missed } => {
                app.set_status_text(format!("missed {missed} events").into());
            }
            EngineEvent::OutboxEnqueued { .. } => app.set_status_text("queued".into()),
            EngineEvent::MailSent { .. } => show_toast(app, "Message sent", "success"),
            EngineEvent::MailFailed { error, .. } => {
                show_toast(app, &format!("Send failed: {error}"), "error");
            }
            _ => {}
        }
    }
}
