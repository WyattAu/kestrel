//! Draft autosave: persists compose state to a temp file every 30 seconds
//! and offers to resume on compose initiation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Serializable draft state for autosave persistence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutosaveDraft {
    /// Draft subject.
    pub subject: String,
    /// Draft body (Markdown).
    pub body: String,
    /// Recipients (comma-separated).
    pub to: String,
    /// CC recipients (comma-separated).
    pub cc: String,
    /// BCC recipients (comma-separated).
    pub bcc: String,
    /// Account id (serialized as string).
    pub account_id: String,
    /// In-reply-to message id.
    pub in_reply_to: Option<String>,
    /// References.
    pub references: Vec<String>,
}

/// Returns the path to the autosave file.
fn autosave_path() -> PathBuf {
    let dir = std::env::temp_dir();
    dir.join("kestrel-draft-autosave.json")
}

/// Saves the current draft state to the autosave file.
///
/// # Errors
/// Returns an error if serialization or file write fails.
pub fn save_draft(draft: &AutosaveDraft) -> std::io::Result<()> {
    let path = autosave_path();
    let json = serde_json::to_string_pretty(draft)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    std::fs::write(&path, json)
}

/// Loads a previously saved draft from the autosave file.
#[must_use]
pub fn load_draft() -> Option<AutosaveDraft> {
    let path = autosave_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Deletes the autosave file (after successful send or explicit discard).
pub fn delete_autosave() {
    let path = autosave_path();
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_delete_roundtrip() {
        let draft = AutosaveDraft {
            subject: "Test".into(),
            body: "Hello world".into(),
            to: "alice@example.com".into(),
            cc: String::new(),
            bcc: String::new(),
            account_id: "acc-123".into(),
            in_reply_to: Some("msg-456".into()),
            references: vec!["ref-1".into()],
        };
        save_draft(&draft).unwrap();
        let loaded = load_draft().unwrap();
        assert_eq!(loaded.subject, "Test");
        assert_eq!(loaded.body, "Hello world");
        assert_eq!(loaded.to, "alice@example.com");
        assert_eq!(loaded.in_reply_to, Some("msg-456".into()));
        delete_autosave();
        assert!(load_draft().is_none());
    }
}
