//! `kestrel-mobile` — Mobile frontend for the Kestrel email client.
//!
//! Reuses `kestrel-engine` in-process (ADR 0011) and provides mobile-specific
//! configuration, background task management, and push notification stubs.
//! The UI is built with Slint mobile (ADR 0001 extended for mobile).

#![allow(unsafe_code)]

pub mod background;
pub mod engine_adapter;
pub mod ffi;
pub mod platform;
pub mod push;

pub use engine_adapter::{EngineHandle, MobileEngineConfig};
