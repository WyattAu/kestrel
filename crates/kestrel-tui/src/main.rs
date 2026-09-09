//! `kestrel-tui` binary: load config, spawn engine, run the TUI loop.
// Binary top-level: eprintln for startup failures before the subscriber
// is up (ADR 0007 permits anyhow-style reporting at binary top level).
// `unsafe`: exactly one FFI call — the SIGTTOU disposition in `main`
// (documented there); everything else is safe.
#![allow(clippy::print_stderr, clippy::print_stdout, unsafe_code)]

use std::sync::Arc;

use kestrel_core::{config::Config, paths::Paths};

fn main() {
    // Cold-start SLA probe (phase-3 gate 1): exec → first frame.
    let mut sla_probe = kestrel_tui::sla::ColdStartProbe::start();
    // The TUI must be able to set up and read the terminal even when its
    // process group is not the pty's foreground group — e.g. under the
    // CI startup harness (`script`/xvfb launchers leave it backgrounded).
    // Without this, the kernel stops the whole process at terminal setup
    // (SIGTTOU from tcsetattr) or at the first input read (SIGTTIN), and
    // the app hangs before its first frame — invisible to a mere
    // liveness check, which is exactly what the old harness measured. In
    // a normal foreground terminal neither signal is generated, so
    // ignoring them is a no-op there.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::signal(libc::SIGTTIN, libc::SIG_IGN);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("kestrel-tui: runtime startup failed: {e}");
            std::process::exit(1);
        });
    let code = runtime.block_on(async_main(&mut sla_probe));
    std::process::exit(code);
}

async fn async_main(sla_probe: &mut kestrel_tui::sla::ColdStartProbe) -> i32 {
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

    let ui_result = kestrel_tui::event::run(handle.clone(), config, Some(sla_probe)).await;
    // Ordered engine shutdown (architecture §3.3): stop supervised services,
    // bounded outbox flush, storage checkpoint — even when the UI failed.
    handle.shutdown(true).await;
    match ui_result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("kestrel-tui: {e}");
            1
        }
    }
}
