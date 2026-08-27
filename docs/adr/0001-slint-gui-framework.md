# ADR 0001: Use Slint for the Native GUI Shell

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §6 requires a native desktop GUI shell with either **Slint**
or **Iced**, hosting a `wry` webview for HTML email bodies, system tray,
notifications, and a < 200 ms cold-start SLA (hard limit 500 ms). The choice
affects cold start, memory budget (< 120 MB idle), API stability over a
multi-year project, and how easily a small team can build and maintain complex
multi-pane layouts (message lists, sidebars, settings).

## Decision

We use **Slint** (declared in Rust via the `slint` crate, UI in `.slint` files)
for the `kestrel-gui` shell: navigation panels, folder tree, message list,
composer chrome, and settings. The HTML body viewport remains **`wry`** per
requirements §6, embedded as a single native webview widget.

## Consequences

- **Stability:** Slint's language and APIs are versioned with a compatibility
  policy (1.x line), reducing churn risk versus Iced's pre-1.0 API.
- **Performance:** Slint compiles UI to native code with a small runtime; it
  supports the < 200 ms cold-start and < 120 MB idle budgets without pulling a
  retained-mode widget tree that reallocates on every frame.
- **Strong typing:** The `.slint` DSL is strongly typed and generates Rust
  structs/callbacks; UI/model mismatches are compile errors, matching our
  compile-time-correctness stance (see ADR 0003).
- **Tooling:** `slint-viewer` and live-preview speed up UI iteration.
- **Licensing:** Kestrel is Apache-2.0. Slint is available under GPLv3 **or**
  the Slint Royalty-Free Desktop License. Using GPLv3 Slint would force
  relicensing Kestrel — **not acceptable**. We must accept and comply with
  the Slint Royalty-Free Desktop License terms (review before the Phase 4
  milestone; tracked as a `crates/gui` licensing gate in
  `docs/engineering-standards.md` §6 and cargo-deny exceptions). If terms
  become incompatible, Iced becomes the fallback (supersede this ADR).
- **Cost:** The team must learn the Slint DSL; complex custom widgets require
  Rust-side model implementations (fine for our list-heavy UI).
- `wry` integration is a native-window embedding task; we own a thin
  platform-glue layer rather than getting it free from a framework.

## Alternatives Considered

- **Iced** — pure-Rust Elm architecture, pleasant Rust-native API; rejected
  because it is pre-1.0 with recurring breaking changes, historical text
  rendering/frame-pacing issues at 60/120 FPS scroll SLAs, and a heavier
  runtime startup path. Revisit if Iced reaches a stable 1.0 with demonstrated
  list-scrolling performance.
- **Tauri for the entire GUI** — rejected: requirements mandate a native shell
  with only the email body in a webview; a webview-driven shell violates the
  native-GUI requirement and increases attack surface.
- **egui** — immediate mode; excellent for tools, weaker for polished
  multi-pane desktop UX, accessibility, and system integration (tray).
- **GTK/Qt bindings** — non-Rust core or binding-maintenance burden.
