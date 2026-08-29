//! TUI App state machine: pure state transitions testable without a
//! terminal (architecture §3.2 rule 4: windowed, pre-materialized models).

use kestrel_core::{
    ids::{FolderId, MessageId},
    protocol::{AccountSummary, FolderSummary, MessagePage, MessageSummary, MessageView},
};

/// Which pane has focus (requirements §5: 3-pane + focus mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// Folder/account list.
    Folders,
    /// Message list.
    List,
    /// Preview/reader.
    Preview,
}

/// UI mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Normal navigation.
    Normal,
    /// Search input.
    Search,
    /// Command-line status (quit confirm etc.).
    Confirm,
    /// Account setup form.
    Setup,
}

/// Pre-materialized windowed model (frame pacing SLA).
pub struct AppState {
    /// Accounts.
    pub accounts: Vec<AccountSummary>,
    /// Folders of the selected account.
    pub folders: Vec<FolderSummary>,
    /// Current message page.
    pub page: MessagePage,
    /// Preview of the selected message (if loaded).
    pub preview: Option<MessageView>,
    /// Selected account index.
    pub selected_account: usize,
    /// Selected folder index.
    pub selected_folder: usize,
    /// Selected message index within the page.
    pub selected_message: usize,
    /// Focus.
    pub focus: Focus,
    /// Mode.
    pub mode: Mode,
    /// Search input buffer.
    pub search_input: String,
    /// Setup: email field.
    pub setup_email: String,
    /// Setup: password field.
    pub setup_password: String,
    /// Setup: IMAP host field.
    pub setup_imap_host: String,
    /// Setup: active field index (0=email, 1=password, 2=host).
    pub setup_field: usize,
    /// Status line.
    pub status: String,
    /// Current page offset (windowing).
    pub page_offset: u64,
    /// Window size.
    pub window_limit: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            folders: Vec::new(),
            page: MessagePage::default(),
            preview: None,
            selected_account: 0,
            selected_folder: 0,
            selected_message: 0,
            focus: Focus::Folders,
            mode: Mode::Normal,
            search_input: String::new(),
            setup_email: String::new(),
            setup_password: String::new(),
            setup_imap_host: String::new(),
            setup_field: 0,
            status: String::new(),
            page_offset: 0,
            window_limit: 50,
        }
    }
}

impl AppState {
    /// Selected account.
    #[must_use]
    pub fn account(&self) -> Option<&AccountSummary> {
        self.accounts.get(self.selected_account)
    }

    /// Selected folder.
    #[must_use]
    pub fn folder(&self) -> Option<&FolderSummary> {
        self.folders.get(self.selected_folder)
    }

    /// Selected folder id.
    #[must_use]
    pub fn folder_id(&self) -> Option<FolderId> {
        self.folder().map(|f| f.id)
    }

    /// Selected message.
    #[must_use]
    pub fn message(&self) -> Option<&MessageSummary> {
        self.page.items.get(self.selected_message)
    }

    /// Selected message id.
    #[must_use]
    pub fn message_id(&self) -> Option<MessageId> {
        self.message().map(|m| m.id)
    }

    /// Vi-down: context-dependent (threat model §4.6: bounded work).
    pub fn move_down(&mut self) {
        match self.focus {
            Focus::Folders => {
                if self.selected_folder + 1 < self.folders.len() {
                    self.selected_folder += 1;
                }
            }
            Focus::List | Focus::Preview => {
                if self.selected_message + 1 < self.page.items.len() {
                    self.selected_message += 1;
                }
            }
        }
    }

    /// Vi-up.
    pub fn move_up(&mut self) {
        match self.focus {
            Focus::Folders => self.selected_folder = self.selected_folder.saturating_sub(1),
            Focus::List | Focus::Preview => {
                self.selected_message = self.selected_message.saturating_sub(1);
            }
        }
    }

    /// Half-page down (Ctrl-D style).
    pub fn page_down(&mut self) {
        let half = (self.window_limit / 2).max(1);
        for _ in 0..half {
            self.move_down();
        }
    }

    /// Half-page up.
    pub fn page_up(&mut self) {
        let half = (self.window_limit / 2).max(1);
        for _ in 0..half {
            self.move_up();
        }
    }

    /// Focus cycle (Tab).
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Folders => Focus::List,
            Focus::List => Focus::Preview,
            Focus::Preview => Focus::Folders,
        };
    }

    /// Focus the next pane (J/K in list).
    pub fn enter(&mut self) {
        if self.focus == Focus::Folders {
            self.focus = Focus::List;
        } else if self.focus == Focus::List {
            self.focus = Focus::Preview;
        }
    }

    /// Go to parent pane (H).
    pub fn back(&mut self) {
        if self.focus == Focus::Preview {
            self.focus = Focus::List;
        } else if self.focus == Focus::List {
            self.focus = Focus::Folders;
        }
    }

    /// Selecting a folder resets the message selection.
    pub fn set_folders(&mut self, folders: Vec<FolderSummary>) {
        self.folders = folders;
        if self.selected_folder >= self.folders.len() {
            self.selected_folder = self.folders.len().saturating_sub(1);
        }
    }

    /// Replace the page, clamping the selection.
    pub fn set_page(&mut self, page: MessagePage) {
        let keep = std::cmp::min(self.selected_message, page.items.len().saturating_sub(1));
        self.selected_message = keep;
        self.page = page;
        self.preview = None;
    }

    /// Search input push.
    pub fn push_search(&mut self, ch: char) {
        self.search_input.push(ch);
    }

    /// Search input pop.
    pub fn pop_search(&mut self) {
        self.search_input.pop();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use kestrel_core::protocol::ThreadIdLite;

    use super::*;

    fn summary(n: usize) -> MessageSummary {
        MessageSummary {
            id: MessageId::from_uuid(uuid::Uuid::now_v7()),
            folder: FolderId::from_uuid(uuid::Uuid::now_v7()),
            uid: u32::try_from(n).unwrap_or(0),
            internal_date: 1_700_000_000_000 + i64::try_from(n).unwrap_or(0),
            flags: vec![],
            message_id: None,
            in_reply_to: None,
            subject: Some(format!("msg {n}")),
            from: vec![],
            to: vec![],
            cc: vec![],
            size: 100,
            is_read: false,
            is_flagged: false,
            is_answered: false,
            has_attachments: false,
            thread: ThreadIdLite {
                key: format!("t{n}"),
            },
        }
    }

    #[test]
    fn navigation_bounds_respected() {
        let mut s = AppState::default();
        s.set_page(MessagePage {
            items: (0..5).map(summary).collect(),
            total: 5,
        });
        s.focus = Focus::List;
        for _ in 0..10 {
            s.move_down();
        }
        assert_eq!(s.selected_message, 4, "clamps at last message");
        for _ in 0..10 {
            s.move_up();
        }
        assert_eq!(s.selected_message, 0, "clamps at first message");
    }

    #[test]
    fn focus_cycles_through_panes() {
        let mut s = AppState::default();
        assert_eq!(s.focus, Focus::Folders);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::List);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Preview);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Folders);
        s.enter();
        assert_eq!(s.focus, Focus::List);
        s.enter();
        assert_eq!(s.focus, Focus::Preview);
        s.back();
        assert_eq!(s.focus, Focus::List);
        s.back();
        assert_eq!(s.focus, Focus::Folders);
    }

    #[test]
    fn page_replace_clamps_selection() {
        let mut s = AppState::default();
        s.set_page(MessagePage {
            items: (0..10).map(summary).collect(),
            total: 10,
        });
        s.selected_message = 9;
        s.set_page(MessagePage {
            items: (0..3).map(summary).collect(),
            total: 3,
        });
        assert_eq!(s.selected_message, 2, "clamped into smaller page");
        assert!(s.preview.is_none(), "preview invalidated on page change");
    }

    #[test]
    fn search_input_editing() {
        let mut s = AppState {
            mode: Mode::Search,
            ..AppState::default()
        };
        s.push_search('a');
        s.push_search('b');
        assert_eq!(s.search_input, "ab");
        s.pop_search();
        assert_eq!(s.search_input, "a");
    }
}
