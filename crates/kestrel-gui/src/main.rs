//! `kestrel-gui` binary: Slint event loop on the main thread, tokio
//! runtime for the engine, sandboxed wry viewport for HTML bodies.

// Binary top-level: eprintln before the tracing subscriber is up.
#![allow(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::let_unit_value,
    clippy::too_many_lines
)]

use std::sync::Arc;

use kestrel_core::{
    config::Config,
    paths::Paths,
    protocol::{Command, CommandPayload, FrontendKind, SearchQuery},
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
                        if let Some(app) = gui_weak.upgrade() {
                            apply_engine_event(&app, &vp_state, ev);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    });

    // Wait for the engine handle (bounded).
    let handle = engine_handle_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|e| {
            eprintln!("kestrel-gui: engine handle timeout: {e}");
            std::process::exit(1);
        });

    // Wire Slint callbacks → engine commands.
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
        let handle = handle.clone();
        move || {
            // The composer opens via ComposeSubmit (protocol §2); the
            // Slint dialog chrome lands with the GUI polish pass.
            let _ = handle;
        }
    });

    app.on_open_body_view({
        let _vp = Arc::clone(&viewport_state);
        move || {
            // The sandboxed wry viewport opens with the current message;
            // wired when a message is selected in the list.
        }
    });

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
