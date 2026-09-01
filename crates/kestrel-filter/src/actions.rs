//! Action definitions and execution helpers. Actions are value types that
//! describe what to do; the engine translates them into storage commands
//! via `StorageHandle` (ADR 0004: no direct storage calls from evaluation
//! logic).

use kestrel_core::{
    ids::{FolderId, MessageId},
    protocol::Flag,
};

pub use crate::types::Action;

/// The result of evaluating a rule: which message and what actions to apply.
#[derive(Clone, Debug)]
pub struct FilterMatch {
    /// The matched message.
    pub message: MessageId,
    /// Actions to execute, in rule order.
    pub actions: Vec<Action>,
}

/// Translate a list of actions into a plan that the engine can execute.
///
/// This is a pure function: it collects the actions without performing I/O.
/// The engine's filter service will iterate the plan and call the appropriate
/// `StorageHandle` methods.
#[must_use]
pub fn build_action_plan(actions: &[Action]) -> Vec<PlannedAction> {
    actions.iter().map(PlannedAction::from).collect()
}

/// A single planned action ready for execution by the engine.
#[derive(Clone, Debug)]
pub enum PlannedAction {
    /// Move to folder.
    Move {
        /// Destination folder.
        to: FolderId,
    },
    /// Copy to folder.
    Copy {
        /// Destination folder.
        to: FolderId,
    },
    /// Apply flags.
    AddFlags {
        /// Flags to add.
        flags: Vec<Flag>,
    },
    /// Mark as read.
    MarkRead,
    /// Delete (move to trash).
    Delete,
    /// Forward to address.
    Forward {
        /// Recipient email.
        to: String,
    },
}

impl<'a> From<&'a Action> for PlannedAction {
    fn from(action: &'a Action) -> Self {
        match action {
            Action::MoveTo(to) => Self::Move { to: *to },
            Action::CopyTo(to) => Self::Copy { to: *to },
            Action::Flag(flags) => Self::AddFlags {
                flags: flags.clone(),
            },
            Action::MarkRead => Self::MarkRead,
            Action::Delete => Self::Delete,
            Action::Forward(addr) => Self::Forward { to: addr.clone() },
        }
    }
}

/// Collect all matches from evaluating a set of rules against a message.
///
/// Returns actions from the first matching rule only (priority order).
/// If no rule matches, returns an empty vec.
#[must_use]
pub fn collect_matches(
    rules: &[crate::types::FilterRule],
    field_values: &crate::types::FieldValues<'_>,
    regex_cache: &crate::eval::RegexCache,
) -> Vec<Action> {
    use crate::eval::evaluate_rule;

    for rule in rules {
        if evaluate_rule(rule, field_values, regex_cache) {
            return rule.actions.clone();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use kestrel_core::{
        ids::AccountId,
        protocol::{Address, MessageSummary, ThreadIdLite},
    };

    use super::*;
    use crate::types::{Condition, FilterRule, LogicOp, Operator};

    fn test_rule(actions: Vec<Action>) -> FilterRule {
        FilterRule {
            id: "test".to_string(),
            account_id: AccountId::from_uuid(uuid::Uuid::now_v7()),
            name: "Test".to_string(),
            enabled: true,
            priority: 0,
            conditions: vec![Condition {
                field: crate::types::ConditionField::Subject,
                operator: Operator::Contains,
                value: "hello".to_string(),
                negate: false,
            }],
            condition_logic: LogicOp::And,
            actions,
        }
    }

    fn make_summary() -> MessageSummary {
        MessageSummary {
            id: kestrel_core::ids::MessageId::from_uuid(uuid::Uuid::now_v7()),
            folder: kestrel_core::ids::FolderId::from_uuid(uuid::Uuid::now_v7()),
            uid: 1,
            internal_date: 0,
            flags: vec![],
            message_id: None,
            in_reply_to: None,
            subject: Some("Hello World".to_string()),
            from: vec![Address {
                name: Some("Alice".to_string()),
                email: "alice@example.com".to_string(),
            }],
            to: vec![Address {
                name: Some("Bob".to_string()),
                email: "bob@example.com".to_string(),
            }],
            cc: vec![],
            size: 100,
            is_read: false,
            is_flagged: false,
            is_answered: false,
            has_attachments: false,
            thread: ThreadIdLite {
                key: "test-thread".to_string(),
            },
        }
    }

    #[test]
    fn collect_matches_returns_first_rule_actions() {
        let rules = vec![
            test_rule(vec![Action::MarkRead]),
            test_rule(vec![Action::Delete]),
        ];
        let msg = make_summary();
        let fv = crate::types::FieldValues::from_message(&msg, "");
        let cache = crate::eval::RegexCache::default();
        let matches = collect_matches(&rules, &fv, &cache);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], Action::MarkRead);
    }

    #[test]
    fn collect_matches_returns_empty_when_no_match() {
        let rules = vec![test_rule(vec![Action::MarkRead])];
        let mut msg = make_summary();
        msg.subject = Some("no match here".to_string());
        let fv = crate::types::FieldValues::from_message(&msg, "");
        let cache = crate::eval::RegexCache::default();
        let matches = collect_matches(&rules, &fv, &cache);
        assert!(matches.is_empty());
    }
}
