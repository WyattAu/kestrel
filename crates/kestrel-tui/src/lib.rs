//! `kestrel-tui` — the terminal frontend of Kestrel (requirements §5).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]

pub mod app;
pub mod editor;
pub mod event;
pub mod html;
pub mod ui;
