//! TUI App state machine: pure state transitions testable without a
//! terminal (architecture §3.2 rule 4: windowed, pre-materialized models).

use kestrel_core::{
    clock::Clock as _,
    ids::{FolderId, MessageId},
    protocol::{
        AccountSummary, FolderSummary, MessagePage, MessageSummary, MessageView, SortDir,
        SortField, SortSpec,
    },
    theme::Theme,
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
    /// Confirm delete prompt.
    ConfirmDelete,
    /// Snooze message options overlay.
    Snooze,
    /// Command-line input (`:...` commands).
    Command,
    /// Confirm remove account prompt.
    ConfirmRemoveAccount,
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
    /// Multi-select mode active.
    pub multi_select_mode: bool,
    /// Indices of currently selected messages (in multi-select mode).
    pub selected_messages: Vec<usize>,
    /// Show only unread messages.
    pub show_unread_only: bool,
    /// Which thread keys are expanded (thread key -> expanded).
    pub expanded_threads: std::collections::HashSet<String>,
    /// Vertical scroll offset in the preview pane.
    pub scroll_offset: usize,
    /// Total lines in the current preview body.
    pub preview_total_lines: usize,
    /// Timestamp of the last draft autosave (unix ms) for 30-second interval check.
    pub autosave_timer: i64,
    /// Original unfiltered page (before unread filter is applied).
    pub original_page: MessagePage,
    /// Snooze custom hours input buffer.
    pub snooze_hours: String,
    /// Current sort field for message listings.
    pub sort_field: SortField,
    /// Current sort direction for message listings.
    pub sort_dir: SortDir,
    /// Index of the selected snooze option in the snooze modal.
    pub snooze_selection: usize,
    /// User-saved searches for quick reuse.
    pub saved_searches: Vec<kestrel_core::config::SavedSearch>,
    /// Command-line input buffer (`:...` commands).
    pub command_input: String,
    /// Active color theme.
    pub theme: Theme,
    /// Whether the message list is in thread view mode.
    pub thread_view: bool,
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
            multi_select_mode: false,
            selected_messages: Vec::new(),
            show_unread_only: false,
            expanded_threads: std::collections::HashSet::new(),
            scroll_offset: 0,
            preview_total_lines: 0,
            autosave_timer: 0,
            original_page: MessagePage::default(),
            snooze_hours: String::new(),
            sort_field: SortField::Date,
            sort_dir: SortDir::Desc,
            snooze_selection: 0,
            saved_searches: Vec::new(),
            command_input: String::new(),
            theme: Theme::default(),
            thread_view: false,
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
        self.original_page = page.clone();
        self.page = page;
        self.preview = None;
        self.apply_unread_filter();
    }

    /// Search input push.
    pub fn push_search(&mut self, ch: char) {
        self.search_input.push(ch);
    }

    /// Search input pop.
    pub fn pop_search(&mut self) {
        self.search_input.pop();
    }

    /// Toggle multi-select mode on/off.
    pub fn toggle_multi_select(&mut self) {
        self.multi_select_mode = !self.multi_select_mode;
        if !self.multi_select_mode {
            self.selected_messages.clear();
        }
    }

    /// Add or remove the current message from the selection.
    pub fn select_current(&mut self) {
        let idx = self.selected_message;
        if let Some(pos) = self.selected_messages.iter().position(|&i| i == idx) {
            self.selected_messages.remove(pos);
        } else {
            self.selected_messages.push(idx);
        }
    }

    /// Select a range of messages (inclusive).
    pub fn select_range(&mut self, from: usize, to: usize) {
        let lo = from.min(to);
        let hi = from.max(to);
        for i in lo..=hi {
            if !self.selected_messages.contains(&i) {
                self.selected_messages.push(i);
            }
        }
    }

    /// Select all visible messages.
    pub fn select_all(&mut self) {
        self.selected_messages.clear();
        for i in 0..self.page.items.len() {
            self.selected_messages.push(i);
        }
    }

    /// Toggle unread-only filter.
    pub fn toggle_unread_filter(&mut self) {
        self.show_unread_only = !self.show_unread_only;
        self.apply_unread_filter();
    }

    /// Apply the unread filter to the current page.
    pub fn apply_unread_filter(&mut self) {
        if self.show_unread_only {
            self.page = self.original_page.clone();
            self.page.items.retain(|m| !m.is_read);
            self.page.total = self.original_page.total;
            self.selected_message = self
                .selected_message
                .min(self.page.items.len().saturating_sub(1));
        } else {
            self.page = self.original_page.clone();
            self.selected_message = self
                .selected_message
                .min(self.page.items.len().saturating_sub(1));
        }
    }

    /// Toggle thread expand/collapse for a thread key.
    pub fn toggle_thread_expand(&mut self, thread_key: &str) {
        if self.expanded_threads.contains(thread_key) {
            self.expanded_threads.remove(thread_key);
        } else {
            self.expanded_threads.insert(thread_key.to_string());
        }
    }

    /// Check if 30 seconds have passed since the last autosave.
    /// Returns `true` if autosave should run and resets the timer.
    pub fn should_autosave(&mut self) -> bool {
        let now = kestrel_core::clock::SystemClock.now_unix_ms();
        if self.autosave_timer == 0 {
            self.autosave_timer = now;
            return false;
        }
        if now - self.autosave_timer >= 30_000 {
            self.autosave_timer = now;
            true
        } else {
            false
        }
    }

    /// Reset the autosave timer (e.g., on compose exit or send).
    pub fn reset_autosave_timer(&mut self) {
        self.autosave_timer = 0;
    }

    /// Build the current sort specification from fields.
    #[must_use]
    pub fn sort_spec(&self) -> SortSpec {
        SortSpec {
            field: self.sort_field,
            dir: self.sort_dir,
        }
    }

    /// Command input push.
    pub fn push_command(&mut self, ch: char) {
        self.command_input.push(ch);
    }

    /// Command input pop.
    pub fn pop_command(&mut self) {
        self.command_input.pop();
    }

    /// Scroll preview up by `lines` (clamped to 0).
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Scroll preview down by `lines` (clamped to `scroll_max`).
    pub fn scroll_down(&mut self, lines: usize, visible_lines: usize) {
        let max = self.preview_total_lines.saturating_sub(visible_lines);
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(max);
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

    #[test]
    fn multi_select_toggle() {
        let mut s = AppState::default();
        assert!(!s.multi_select_mode);
        s.toggle_multi_select();
        assert!(s.multi_select_mode);
        s.toggle_multi_select();
        assert!(!s.multi_select_mode);
        assert!(s.selected_messages.is_empty());
    }

    #[test]
    fn multi_select_toggle_clears_selection() {
        let mut s = AppState::default();
        s.toggle_multi_select();
        s.selected_messages.push(0);
        s.selected_messages.push(2);
        assert_eq!(s.selected_messages.len(), 2);
        s.toggle_multi_select();
        assert!(s.selected_messages.is_empty());
    }

    #[test]
    fn select_current_adds_and_removes() {
        let mut s = AppState::default();
        s.set_page(MessagePage {
            items: (0..5).map(summary).collect(),
            total: 5,
        });
        s.selected_message = 2;
        s.toggle_multi_select();
        s.select_current();
        assert!(s.selected_messages.contains(&2));
        assert_eq!(s.selected_messages.len(), 1);
        s.select_current();
        assert!(!s.selected_messages.contains(&2));
        assert_eq!(s.selected_messages.len(), 0);
    }

    #[test]
    fn select_range_covers_inclusive() {
        let mut s = AppState::default();
        s.set_page(MessagePage {
            items: (0..10).map(summary).collect(),
            total: 10,
        });
        s.toggle_multi_select();
        s.select_range(2, 5);
        assert_eq!(s.selected_messages.len(), 4);
        assert!(s.selected_messages.contains(&2));
        assert!(s.selected_messages.contains(&5));
        // Reversed range should work too.
        s.selected_messages.clear();
        s.select_range(7, 3);
        assert_eq!(s.selected_messages.len(), 5);
        assert!(s.selected_messages.contains(&3));
        assert!(s.selected_messages.contains(&7));
    }

    #[test]
    fn select_all_selects_everything() {
        let mut s = AppState::default();
        s.set_page(MessagePage {
            items: (0..5).map(summary).collect(),
            total: 5,
        });
        s.toggle_multi_select();
        s.select_all();
        assert_eq!(s.selected_messages.len(), 5);
    }

    #[test]
    fn unread_filter_toggles() {
        let mut s = AppState::default();
        s.set_page(MessagePage {
            items: vec![
                summary(0), // unread
                {
                    let mut m = summary(1);
                    m.is_read = true;
                    m
                },
                summary(2), // unread
            ],
            total: 3,
        });
        assert!(!s.show_unread_only);
        assert_eq!(s.page.items.len(), 3);

        s.toggle_unread_filter();
        assert!(s.show_unread_only);
        assert_eq!(s.page.items.len(), 2, "only unread shown");

        s.toggle_unread_filter();
        assert!(!s.show_unread_only);
        assert_eq!(s.page.items.len(), 3, "all shown again");
    }

    #[test]
    fn thread_expand_toggle() {
        let mut s = AppState::default();
        assert!(!s.expanded_threads.contains("t1"));
        s.toggle_thread_expand("t1");
        assert!(s.expanded_threads.contains("t1"));
        s.toggle_thread_expand("t1");
        assert!(!s.expanded_threads.contains("t1"));
    }
}
