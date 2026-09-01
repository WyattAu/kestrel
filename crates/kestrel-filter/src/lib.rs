//! `kestrel-filter` — rule-based mail filtering engine.
//!
//! Defines filter rules (conditions + actions), evaluates them against
//! message summaries, and produces action plans for the engine to execute.
//! This crate is synchronous and I/O-free: all rule evaluation is
//! deterministic and testable without storage.
//!
//! # Security
//!
//! Regex evaluation is bounded (100 ms timeout). Invalid patterns are
//! treated as non-matching (never panic). Filter actions execute through
//! the normal service protocol (ADR 0004).

pub mod actions;
pub mod eval;
pub mod types;

pub use actions::{FilterMatch, PlannedAction, collect_matches};
pub use eval::RegexCache;
pub use types::{Action, Condition, ConditionField, FilterRule, LogicOp, Operator};
