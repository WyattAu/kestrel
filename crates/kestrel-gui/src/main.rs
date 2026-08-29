//! `kestrel-gui` binary: Slint event loop on the main thread, tokio
//! runtime for the engine, sandboxed wry viewport for HTML bodies.
//! First run shows an account setup wizard; after account creation the
//! main 3-pane UI appears and syncs.

// Binary top-level: eprintln before the tracing subscriber is up.
#![allow(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::let_unit_value,
    clippy::too_many_lines,
    clippy::items_after_statements
)]

use std::sync::Arc;

use kestrel_core::{
    config::Config,
    paths::Paths,
    protocol::{Command, CommandPayload, FrontendKind, Reply, SearchQuery},
    provider::{detect_provider, provider_preset},
    secrets::SecretString,
};
use kestrel_gui::{AppWindow, SharedViewportState, ViewportState};
use slint::ComponentHandle;

fn main() {
    // Tracing (ADR 0008).
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
            eprintln!("kestrel-gui: path resolution failed: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = paths.ensure() {
        eprintln!("kestrel-gui: directory setup failed: {e}");
        std::process::exit(1);
    }
    let loaded = match Config::load(&paths) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("kestrel-gui: config error: {e}");
            std::process::exit(1);
        }
    };
    let config = loaded.config;

    let app = AppWindow::new().unwrap_or_else(|e| {
        eprintln!("kestrel-gui: UI creation failed: {e}");
        std::process::exit(1);
    });

    let viewport_state: SharedViewportState =
        Arc::new(std::sync::Mutex::new(ViewportState::default()));

    // Tokio runtime for the engine (background thread; Slint owns main).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("kestrel-gui: runtime startup failed: {e}");
            std::process::exit(1);
        });

    let (engine_handle_tx, engine_handle_rx) =
        std::sync::mpsc::channel::<kestrel_engine::EngineHandle>();
    let gui_weak = app.as_weak();
    let vp_state = Arc::clone(&viewport_state);
    let engine_config = Arc::clone(&config);
    let engine_paths = Arc::clone(&paths);

    std::thread::spawn(move || {
        rt.block_on(async move {
            let handle = match kestrel_engine::Engine::spawn(engine_config, engine_paths).await {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("kestrel-gui: engine startup failed: {e}");
                    std::process::exit(1);
                }
            };
            let _ = engine_handle_tx.send(handle.clone());

            // Forward engine events → Slint UI updates.
            let mut events = handle.events();
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        let vp = Arc::clone(&vp_state);
                        gui_weak
                            .upgrade_in_event_loop(move |app| {
                                apply_engine_event(&app, &vp, ev);
                            })
                            .ok();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    });

    // Wait for the engine.
    let handle = engine_handle_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|e| {
            eprintln!("kestrel-gui: engine timeout: {e}");
            std::process::exit(1);
        });

    // ─── Account setup wizard ───
    app.on_add_account({
        let handle = handle.clone();
        let app_weak = app.as_weak();
        move |display_name, email, password, imap_host, smtp_host| {
            let app = app_weak.unwrap();
            app.set_setup_busy(true);
            app.set_setup_error("".into());

            // Auto-detect provider from email.
            let provider = detect_provider(&email);
            let mut account_config = provider_preset(&provider, &email);
            account_config.display_name = if display_name.is_empty() {
                account_config.display_name.clone()
            } else {
                display_name.to_string()
            };
            account_config.email = email.to_string();
            // Manual overrides: accept "host" or "host:port".
            if !imap_host.is_empty() {
                if let Some((h, p)) = imap_host.split_once(':') {
                    account_config.imap_host = h.to_string();
                    account_config.imap_port = p.parse().unwrap_or(account_config.imap_port);
                } else {
                    account_config.imap_host = imap_host.to_string();
                }
            }
            if !smtp_host.is_empty() {
                if let Some((h, p)) = smtp_host.split_once(':') {
                    account_config.smtp_host = h.to_string();
                    account_config.smtp_port = p.parse().unwrap_or(account_config.smtp_port);
                } else {
                    account_config.smtp_host = smtp_host.to_string();
                }
            }

            // Validate.
            let errors = kestrel_core::provider::validate_account_config(&account_config);
            if !errors.is_empty() {
                app.set_setup_error(errors.join("; ").into());
                app.set_setup_busy(false);
                return;
            }

            // Submit via protocol.
            let (tx, rx) = tokio::sync::oneshot::channel();
            let cmd = Command {
                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                origin: FrontendKind::Gui,
                payload: CommandPayload::AddAccount {
                    config: account_config,
                    password: SecretString::new(password.to_string()),
                    reply: tx,
                },
            };

            let handle_for_send = handle.clone();
            let app_weak2 = app_weak.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    if handle_for_send.commands.send(cmd).await.is_err() {
                        return;
                    }
                    match rx.await {
                        Ok(Reply::Accounts(accounts)) => {
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_weak2.upgrade() {
                                    app.set_account_count(
                                        i32::try_from(accounts.len()).unwrap_or(0),
                                    );
                                    app.set_setup_busy(false);
                                    app.set_show_setup(false);
                                    app.set_status_text(
                                        format!("{} account(s) — syncing…", accounts.len()).into(),
                                    );
                                }
                            })
                            .ok();
                        }
                        Ok(Reply::Err(e)) => {
                            let msg = e.to_string();
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_weak2.upgrade() {
                                    app.set_setup_error(msg.into());
                                    app.set_setup_busy(false);
                                }
                            })
                            .ok();
                        }
                        _ => {
                            slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_weak2.upgrade() {
                                    app.set_setup_error("unexpected reply".into());
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

    // ─── Auto-fill server settings when email is entered ───
    // (Slint LineEdit doesn't have a `changed` callback for this pattern,
    //  but the user sees the preset values in the placeholder text.)

    // ─── Search ───
    app.on_search({
        let handle = handle.clone();
        move |query| {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let _ = handle.commands.blocking_send(Command {
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
            });
        }
    });

    app.on_compose({
        let _handle = handle.clone();
        move || {}
    });

    app.on_open_body_view({
        let _vp = Arc::clone(&viewport_state);
        move || {}
    });

    // ─── Initial account check ───
    {
        let handle = handle.clone();
        let app_weak = app.as_weak();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = handle
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::ListAccounts { reply: tx },
                    })
                    .await;
                if let Ok(Reply::Accounts(accounts)) = rx.await {
                    let count = accounts.len();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = app_weak.upgrade() {
                            app.set_account_count(i32::try_from(count).unwrap_or(0));
                            if count > 0 {
                                app.set_show_setup(false);
                                app.set_status_text(
                                    format!("{count} account(s) — syncing…").into(),
                                );
                            }
                        }
                    })
                    .ok();
                }
            });
        });
    }

    app.run().unwrap_or_else(|e| {
        eprintln!("kestrel-gui: event loop failed: {e}");
        std::process::exit(1);
    });
}

fn apply_engine_event(
    app: &AppWindow,
    viewport: &SharedViewportState,
    ev: kestrel_core::protocol::EngineEvent,
) {
    use kestrel_core::protocol::EngineEvent as E;
    match ev {
        E::EngineStarted { version, .. } => {
            app.set_status_text(format!("Kestrel v{version} ready").into());
        }
        E::AccountConnection { state, .. } => {
            app.set_connection_state(format!("{state:?}").into());
        }
        E::MailArrived { summary, .. } => {
            app.set_status_text(format!("{} new message(s)", summary.new).into());
        }
        E::FolderTreeChanged { .. } => {
            app.set_status_text("folders synced".into());
        }
        E::ServiceDegraded { service, error, .. } => {
            app.set_status_text(format!("⚠ {service} degraded: {error}").into());
        }
        E::RemoteContentBlocked { count, .. } => {
            app.set_status_text(format!("{count} remote item(s) blocked").into());
        }
        E::EventStreamLagged { missed } => {
            app.set_status_text(format!("⚠ missed {missed} events").into());
        }
        E::EngineShutdownProgress { stage } => {
            app.set_status_text(format!("shutting down: {stage:?}").into());
        }
        _ => {}
    }
    let _ = viewport;
}
