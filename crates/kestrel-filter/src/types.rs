//! Filter rule types: conditions, operators, actions, and the composite rule
//! structure. These types are domain-only (no storage/async dependencies)
//! so they can be evaluated synchronously against a `MessageSummary`.

use kestrel_core::{
    ids::FolderId,
    protocol::{Address, Flag, MessageSummary},
};

/// A complete filter rule: conditions + actions + metadata.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FilterRule {
    /// Stable identifier (UUID).
    pub id: String,
    /// Owning account.
    pub account_id: kestrel_core::ids::AccountId,
    /// Human-readable name.
    pub name: String,
    /// Whether the rule is active.
    pub enabled: bool,
    /// Lower = evaluated first.
    pub priority: i32,
    /// Conditions to evaluate.
    pub conditions: Vec<Condition>,
    /// How conditions are combined.
    pub condition_logic: LogicOp,
    /// Actions to execute when the rule matches.
    pub actions: Vec<Action>,
}

/// Boolean combinator for conditions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LogicOp {
    /// All conditions must match.
    And,
    /// Any condition may match.
    Or,
}

/// A single condition clause.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Condition {
    /// Which message field to test.
    pub field: ConditionField,
    /// How to test it.
    pub operator: Operator,
    /// The comparison value (interpreted per operator).
    pub value: String,
    /// When `true`, invert the result.
    pub negate: bool,
}

/// Message fields that conditions can target.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConditionField {
    /// First From address email.
    From,
    /// First To address email.
    To,
    /// Any Cc address email.
    Cc,
    /// Subject header.
    Subject,
    /// Message body text (plain or HTML, stripped of tags).
    Body,
    /// A specific header by name.
    Header(String),
    /// Whether the message has attachments.
    HasAttachment,
}

/// Comparison operators.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Operator {
    /// Substring match (case-insensitive).
    Contains,
    /// Exact match (case-insensitive).
    Equals,
    /// Glob-style match (`*` and `?`).
    Matches,
    /// Regular expression (bounded: 100 ms timeout, complexity limit).
    Regex,
    /// Field exists and is non-empty.
    Exists,
}

/// An action to perform when a rule matches.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Action {
    /// Move the message to a folder.
    MoveTo(FolderId),
    /// Copy the message to a folder (original stays).
    CopyTo(FolderId),
    /// Apply flags.
    Flag(Vec<Flag>),
    /// Mark as read (adds `\Seen`).
    MarkRead,
    /// Delete the message (move to trash).
    Delete,
    /// Forward to an email address.
    Forward(String),
}

/// Extracted field values from a message for condition evaluation.
#[derive(Clone, Debug)]
pub struct FieldValues<'a> {
    /// From address email (first).
    pub from: &'a str,
    /// To address email (first).
    pub to: &'a str,
    /// Cc address email (first, if any).
    pub cc: &'a str,
    /// Subject.
    pub subject: &'a str,
    /// Body text.
    pub body: &'a str,
    /// Whether the message has attachments.
    pub has_attachment: bool,
    /// Raw headers (name -> value) for Header conditions.
    pub headers: Vec<(&'a str, &'a str)>,
}

impl<'a> FieldValues<'a> {
    /// Extract field values from a `MessageSummary`.
    ///
    /// `body` is passed separately since it is not part of the summary.
    #[must_use]
    pub fn from_message(msg: &'a MessageSummary, body: &'a str) -> Self {
        Self {
            from: msg.from.first().map(first_email).unwrap_or_default(),
            to: msg.to.first().map(first_email).unwrap_or_default(),
            cc: msg.cc.first().map(first_email).unwrap_or_default(),
            subject: msg.subject.as_deref().unwrap_or_default(),
            body,
            has_attachment: msg.has_attachments,
            headers: Vec::new(),
        }
    }

    /// Get the value for a condition field.
    #[must_use]
    pub fn get_field(&self, field: &ConditionField) -> &str {
        match field {
            ConditionField::From => self.from,
            ConditionField::To => self.to,
            ConditionField::Cc => self.cc,
            ConditionField::Subject => self.subject,
            ConditionField::Body => self.body,
            ConditionField::Header(name) => self
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| *v)
                .unwrap_or_default(),
            ConditionField::HasAttachment => {
                if self.has_attachment {
                    "true"
                } else {
                    "false"
                }
            }
        }
    }
}

fn first_email(addr: &Address) -> &str {
    &addr.email
}
