//! `kestrel-tui` binary: load config, spawn engine, run the TUI loop.
// Binary top-level: eprintln for startup failures before the subscriber
// is up (ADR 0007 permits anyhow-style reporting at binary top level).
#![allow(clippy::print_stderr, clippy::print_stdout)]

use std::sync::Arc;

use kestrel_core::{config::Config, paths::Paths};

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("kestrel-tui: runtime startup failed: {e}");
            std::process::exit(1);
        });
    let code = runtime.block_on(async_main());
    std::process::exit(code);
}

async fn async_main() -> i32 {
    // Tracing (ADR 0008): default info, JSON via config/env.
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let paths = match Paths::from_xdg() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("kestrel-tui: path resolution failed: {e}");
            return 1;
        }
    };
    if let Err(e) = paths.ensure() {
        eprintln!("kestrel-tui: directory setup failed: {e}");
        return 1;
    }

    let loaded = match Config::load(&paths) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("kestrel-tui: config error: {e}");
            return 1;
        }
    };
    for w in &loaded.warnings {
        eprintln!("kestrel-tui: config warning: {w}");
    }
    let config = loaded.config;

    let handle = match kestrel_engine::Engine::spawn(Arc::clone(&config), Arc::clone(&paths)).await
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("kestrel-tui: engine startup failed: {e}");
            return 1;
        }
    };

    if let Err(e) = kestrel_tui::event::run(handle, config).await {
        eprintln!("kestrel-tui: {e}");
        return 1;
    }
    0
}
