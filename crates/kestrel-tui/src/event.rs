//! Event loop: input (crossterm) + engine events (broadcast → mpsc →
//! redraw). The loop never awaits unbounded-latency futures; it polls
//! with a 50 ms timeout (architecture §3.2 non-blocking guarantee).

use std::{fmt::Write as _, sync::Arc, time::Duration};

use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use kestrel_core::{
    clock::Clock as _,
    protocol::{
        Command, CommandPayload, EngineEvent, FrontendKind, MessagePage, Reply, SearchQuery, Window,
    },
};
use kestrel_engine::EngineHandle;
use tokio::sync::mpsc;

use crate::{
    app::{AppState, Focus, Mode},
    editor, ui,
};

/// Terminal event source.
pub enum TermEvent {
    /// Key press.
    Key(KeyEvent),
    /// Terminal resize.
    Resize,
    /// Engine event forwarded.
    Engine(EngineEvent),
}

/// Spawns the input reader task.
fn spawn_input(tx: mpsc::Sender<TermEvent>) {
    tokio::spawn(async move {
        let mut reader = EventStream::new();
        while let Some(Ok(ev)) = reader.next().await {
            let term_ev = match ev {
                CrosstermEvent::Key(k) => TermEvent::Key(k),
                CrosstermEvent::Resize(_, _) => TermEvent::Resize,
                _ => continue,
            };
            if tx.send(term_ev).await.is_err() {
                break;
            }
        }
    });
}

/// Main run loop.
///
/// # Errors
/// Terminal control or IO failures.
pub async fn run(
    handle: EngineHandle,
    config: Arc<kestrel_core::config::Config>,
) -> std::io::Result<()> {
    // Terminal requires a TTY (architecture §7: terminal restored on exit).
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() {
        return Err(std::io::Error::other(
            "stdout is not a terminal; run kestrel-tui in a TTY (use a terminal emulator)",
        ));
    }
    let mut terminal = ratatui::init();
    let (tx, mut rx) = mpsc::channel::<TermEvent>(256);

    // Forward engine broadcast events into the TUI channel.
    let fwd_tx = tx.clone();
    let mut events = handle.events();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(ev) => {
                    if fwd_tx.send(TermEvent::Engine(ev)).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    let _ = fwd_tx
                        .send(TermEvent::Engine(EngineEvent::EventStreamLagged {
                            missed: n,
                        }))
                        .await;
                }
                Err(_) => break,
            }
        }
    });
    spawn_input(tx.clone());

    let mut state = AppState {
        status: "Kestrel".into(),
        ..AppState::default()
    };

    // Initial data load.
    refresh_accounts(&handle, &mut state).await;
    if state.account().is_some() {
        refresh_folders(&handle, &mut state).await;
        if state.folder_id().is_some() {
            refresh_messages(&handle, &mut state).await;
        }
    }

    loop {
        terminal.draw(|f| {
            ui::draw(f, &state);
            ui::draw_modal(f, &state);
        })?;

        // Poll with bounded wait (50 ms frame budget).
        let ev = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        let Some(ev) = ev.ok().flatten() else {
            continue;
        };

        match ev {
            TermEvent::Key(key) => {
                if handle_key(&handle, &mut state, key, &config).await {
                    break;
                }
            }
            TermEvent::Resize => {
                terminal.autoresize()?;
            }
            TermEvent::Engine(ev) => {
                handle_engine_event(&handle, &mut state, ev).await;
            }
        }
    }

    ratatui::restore();
    Ok(())
}

/// Returns `true` when the TUI should exit.
#[allow(clippy::too_many_lines)]
async fn handle_key(
    handle: &EngineHandle,
    state: &mut AppState,
    key: KeyEvent,
    config: &Arc<kestrel_core::config::Config>,
) -> bool {
    // Ctrl-C always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return true;
    }

    // Extract configurable keybinding chars.
    let kb = &config.keybindings;
    let k_reply = kestrel_core::config::KeybindingsConfig::key_char(&kb.reply);
    let k_reply_all = kestrel_core::config::KeybindingsConfig::key_char(&kb.reply_all);
    let k_forward = kestrel_core::config::KeybindingsConfig::key_char(&kb.forward);
    let k_delete = kestrel_core::config::KeybindingsConfig::key_char(&kb.delete);
    let k_archive = kestrel_core::config::KeybindingsConfig::key_char(&kb.archive);
    let k_flag = kestrel_core::config::KeybindingsConfig::key_char(&kb.flag);
    let k_compose = kestrel_core::config::KeybindingsConfig::key_char(&kb.compose);
    let k_search = kestrel_core::config::KeybindingsConfig::key_char(&kb.search);
    let k_next = kestrel_core::config::KeybindingsConfig::key_char(&kb.next);
    let k_prev = kestrel_core::config::KeybindingsConfig::key_char(&kb.prev);

    match state.mode {
        Mode::Search => match key.code {
            KeyCode::Enter => {
                let query = state.search_input.clone();
                state.mode = Mode::Normal;
                state.status = format!("search: {query}");
                search(handle, state, &query).await;
            }
            KeyCode::Esc => {
                state.mode = Mode::Normal;
                state.search_input.clear();
            }
            KeyCode::Backspace => state.pop_search(),
            KeyCode::Char(c) => state.push_search(c),
            _ => {}
        },
        Mode::Confirm => match key.code {
            KeyCode::Char('y' | 'Y') => return true,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                state.mode = Mode::Normal;
            }
            _ => {}
        },
        Mode::ConfirmDelete => match key.code {
            KeyCode::Char('y' | 'Y') => {
                if state.multi_select_mode {
                    bulk_delete_selected(handle, state).await;
                } else {
                    delete_selected(handle, state).await;
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                state.mode = Mode::Normal;
                state.status.clear();
            }
            _ => {}
        },
        Mode::Setup => {
            handle_setup_key(handle, state, key).await;
        }
        Mode::ConfirmRemoveAccount => match key.code {
            KeyCode::Char('y' | 'Y') => {
                remove_account(handle, state).await;
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                state.mode = Mode::Normal;
                state.status.clear();
            }
            _ => {}
        },
        Mode::Snooze => match key.code {
            KeyCode::Esc => {
                state.mode = Mode::Normal;
                state.snooze_hours.clear();
                state.snooze_selection = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                state.snooze_selection = (state.snooze_selection + 1).min(2);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                state.snooze_selection = state.snooze_selection.saturating_sub(1);
            }
            KeyCode::Char(c) if state.snooze_selection == 2 && c.is_ascii_digit() => {
                state.snooze_hours.push(c);
            }
            KeyCode::Backspace if state.snooze_selection == 2 => {
                state.snooze_hours.pop();
            }
            KeyCode::Enter => {
                execute_snooze(handle, state).await;
            }
            _ => {}
        },
        Mode::Command => match key.code {
            KeyCode::Enter => {
                let cmd = state.command_input.clone();
                state.mode = Mode::Normal;
                state.command_input.clear();
                execute_command(handle, state, &cmd).await;
            }
            KeyCode::Esc => {
                state.mode = Mode::Normal;
                state.command_input.clear();
            }
            KeyCode::Backspace => state.pop_command(),
            KeyCode::Char(c) => state.push_command(c),
            _ => {}
        },
        Mode::Normal => match key.code {
            KeyCode::Esc => {
                if state.multi_select_mode {
                    state.toggle_multi_select();
                    state.status.clear();
                }
            }
            KeyCode::Char('q') => {
                if !state.multi_select_mode {
                    state.mode = Mode::Confirm;
                }
            }
            KeyCode::Tab => {
                if !state.multi_select_mode {
                    state.cycle_focus();
                }
            }
            KeyCode::Char(c) if Some(c) == k_next || KeyCode::Down == key.code => {
                state.move_down();
            }
            KeyCode::Char(c) if Some(c) == k_prev || KeyCode::Up == key.code => {
                state.move_up();
            }
            KeyCode::Char('J') | KeyCode::PageDown => state.page_down(),
            KeyCode::Char('K') | KeyCode::PageUp => state.page_up(),
            KeyCode::Char('g') | KeyCode::Home => {
                if !state.multi_select_mode {
                    state.selected_message = 0;
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !state.multi_select_mode {
                    state.selected_message = state.page.items.len().saturating_sub(1);
                }
            }
            KeyCode::Char(' ') => {
                if state.focus == Focus::List || state.focus == Focus::Preview {
                    if state.multi_select_mode {
                        state.select_current();
                        state.status = format!(
                            "Selecting — {} message(s) selected",
                            state.selected_messages.len()
                        );
                    } else {
                        state.toggle_multi_select();
                        if state.multi_select_mode {
                            state.status = format!(
                                "Selecting — {} message(s) selected",
                                state.selected_messages.len()
                            );
                        }
                    }
                }
            }
            KeyCode::Char(c) if Some(c) == k_reply_all => {
                if state.multi_select_mode {
                    state.select_all();
                    state.status = format!(
                        "Selecting — {} message(s) selected",
                        state.selected_messages.len()
                    );
                } else {
                    compose_reply(handle, state, true, config).await;
                }
            }
            KeyCode::Enter => {
                if state.focus == Focus::List
                    && let Some(id) = state.message_id()
                {
                    if state.thread_view && !state.multi_select_mode {
                        let thread_key =
                            state.page.items[state.selected_message].thread.key.clone();
                        state.toggle_thread_expand(&thread_key);
                    } else if !state.multi_select_mode {
                        load_preview(handle, state, id).await;
                        state.focus = Focus::Preview;
                    }
                } else if !state.multi_select_mode {
                    state.enter();
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if !state.multi_select_mode {
                    state.back();
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if !state.multi_select_mode {
                    state.enter();
                }
            }
            KeyCode::Char(c) if Some(c) == k_search => {
                if !state.multi_select_mode {
                    state.mode = Mode::Search;
                    state.search_input.clear();
                }
            }
            KeyCode::Char(':') => {
                state.mode = Mode::Command;
                state.command_input.clear();
                state.command_input.push(':');
            }
            KeyCode::Char(c) if Some(c) == k_delete => {
                if state.multi_select_mode {
                    let count = state.selected_messages.len();
                    if count > 0 {
                        state.status = format!("Delete {count} message(s)? (y/N)");
                        state.mode = Mode::ConfirmDelete;
                    }
                } else {
                    delete_selected(handle, state).await;
                }
            }
            KeyCode::Char(c) if Some(c) == k_reply => {
                if !state.multi_select_mode {
                    compose_reply(handle, state, false, config).await;
                }
            }
            KeyCode::Char(c) if Some(c) == k_forward => {
                if !state.multi_select_mode {
                    compose_forward(handle, state, false, config).await;
                }
            }
            KeyCode::Char('F') => {
                if !state.multi_select_mode {
                    compose_forward(handle, state, true, config).await;
                }
            }
            KeyCode::Char(c) if Some(c) == k_compose => {
                if !state.multi_select_mode {
                    compose_new(handle, state, config).await;
                }
            }
            KeyCode::Char('N') => {
                if state.multi_select_mode {
                    bulk_toggle_unread(handle, state).await;
                } else {
                    toggle_mark_unread(handle, state).await;
                }
            }
            KeyCode::Char(c) if Some(c) == k_archive => {
                if state.multi_select_mode {
                    bulk_archive(handle, state).await;
                } else {
                    archive_selected(handle, state).await;
                }
            }
            KeyCode::Char(c) if Some(c) == k_flag => {
                if state.multi_select_mode {
                    bulk_flag_selected(handle, state).await;
                } else {
                    toggle_flagged(handle, state).await;
                }
            }
            KeyCode::Char('z') => {
                if !state.multi_select_mode && state.message().is_some() {
                    state.mode = Mode::Snooze;
                    state.snooze_hours.clear();
                    state.snooze_selection = 0;
                    state.status = "snooze (j/k:select Enter:confirm)".into();
                }
            }
            KeyCode::Char('U') => {
                if !state.multi_select_mode {
                    state.toggle_unread_filter();
                    if state.show_unread_only {
                        let total = state.original_page.items.len();
                        let shown = state.page.items.len();
                        state.status = format!("Unread Only — showing {shown} of {total}");
                    } else {
                        state.status.clear();
                    }
                }
            }
            KeyCode::Char('S') => {
                state.mode = Mode::Setup;
                state.setup_email.clear();
                state.setup_password.clear();
                state.setup_imap_host.clear();
                state.status = "Setup: fill fields, Enter to connect".into();
            }
            _ if key.modifiers.contains(KeyModifiers::CONTROL) => match key.code {
                KeyCode::Char('d') => {
                    if state.focus == Focus::Preview {
                        state.scroll_down(10, preview_visible_lines(state));
                    } else {
                        state.page_down();
                    }
                }
                KeyCode::Char('u') => {
                    if state.focus == Focus::Preview {
                        state.scroll_up(10);
                    } else {
                        state.page_up();
                    }
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    mark_all_read(handle, state).await;
                }
                _ => {}
            },
            _ => {}
        },
    }
    false
}

async fn handle_engine_event(handle: &EngineHandle, state: &mut AppState, ev: EngineEvent) {
    match ev {
        EngineEvent::MailArrived { folder, .. } | EngineEvent::MessagesChanged { folder, .. } => {
            if Some(folder) == state.folder_id() {
                refresh_messages(handle, state).await;
            }
        }
        EngineEvent::FlagsChanged { .. } => {
            refresh_messages(handle, state).await;
        }
        EngineEvent::FolderTreeChanged { .. } => {
            refresh_folders(handle, state).await;
        }
        EngineEvent::AccountConnection { state: conn, .. } => {
            state.status = format!("{conn:?}");
        }
        EngineEvent::ServiceDegraded { service, error, .. } => {
            state.status = format!("⚠ {service} degraded: {error}");
        }
        EngineEvent::OutboxEnqueued { .. } => {
            state.status = "queued for sending".into();
        }
        EngineEvent::MailSent { .. } => {
            state.status = "sent".into();
        }
        EngineEvent::MailFailed { error, .. } => {
            state.status = format!("send failed: {error}");
        }
        EngineEvent::RemoteContentBlocked { count, .. } => {
            state.status = format!("{count} remote item(s) blocked");
        }
        EngineEvent::EventStreamLagged { missed } => {
            state.status = format!("⚠ missed {missed} events; resyncing");
            refresh_all(handle, state).await;
        }
        _ => {}
    }
}

async fn refresh_all(handle: &EngineHandle, state: &mut AppState) {
    refresh_accounts(handle, state).await;
    if state.account().is_some() {
        refresh_folders(handle, state).await;
    }
    if state.folder_id().is_some() {
        refresh_messages(handle, state).await;
    }
}

async fn refresh_accounts(handle: &EngineHandle, state: &mut AppState) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ListAccounts { reply: tx },
        })
        .await;
    if let Ok(Reply::Accounts(accounts)) = rx.await {
        state.accounts = accounts;
    }
}

async fn refresh_folders(handle: &EngineHandle, state: &mut AppState) {
    let Some(account) = state.account() else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ListFolders {
                account: account.id,
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::Folders(folders)) = rx.await {
        state.set_folders(folders);
    }
}

async fn refresh_messages(handle: &EngineHandle, state: &mut AppState) {
    let Some(folder) = state.folder_id() else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ListMessages {
                folder,
                window: Window {
                    offset: state.page_offset,
                    limit: state.window_limit,
                },
                sort: kestrel_core::protocol::SortSpec::default(),
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::Messages(page)) = rx.await {
        state.set_page(page);
    }
}

async fn load_preview(
    handle: &EngineHandle,
    state: &mut AppState,
    id: kestrel_core::ids::MessageId,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::GetMessage {
                message: id,
                body: kestrel_core::protocol::BodyPreference::Full,
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::Message(view)) = rx.await {
        state.preview = Some(view);
    }
}

async fn search(handle: &EngineHandle, state: &mut AppState, query: &str) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::Search {
                query: SearchQuery {
                    text: Some(query.to_owned()),
                    ..SearchQuery::default()
                },
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::SearchResults(hits)) = rx.await {
        state.status = format!("{} hit(s)", hits.len());
    }
}

async fn delete_selected(handle: &EngineHandle, state: &mut AppState) {
    if let Some(id) = state.message_id() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = handle
            .commands
            .send(Command {
                id: next_request_id(),
                origin: FrontendKind::Tui,
                payload: CommandPayload::DeleteMessages {
                    messages: vec![id],
                    expunge: false,
                    reply: tx,
                },
            })
            .await;
        if matches!(rx.await, Ok(Reply::Accepted)) {
            state.status = "deleted".into();
            refresh_messages(handle, state).await;
        }
    }
}

async fn compose_reply(
    handle: &EngineHandle,
    state: &mut AppState,
    reply_all: bool,
    config: &Arc<kestrel_core::config::Config>,
) {
    let Some(msg) = state.message() else { return };
    let account_id = state.account().map(|a| a.id);
    let Some(account_id) = account_id else { return };

    let to = if reply_all {
        msg.to.clone()
    } else {
        msg.from.clone()
    };
    let subject = format!("Re: {}", msg.subject.clone().unwrap_or_default());
    let to_str = to
        .iter()
        .map(|a| a.email.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let original_body = state
        .preview
        .as_ref()
        .and_then(|v| v.body_plain.as_deref())
        .unwrap_or("");
    let references: Vec<String> = [msg.in_reply_to.clone(), msg.message_id.clone()]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    let mut template = editor::reply_template(
        &subject,
        &to_str,
        msg.message_id.as_deref(),
        &references,
        original_body,
    );

    // Append per-account signature
    let account_email = state.account().map(|a| a.email.clone()).unwrap_or_default();
    if let Some(sig) = config.account_signatures.get(&account_email)
        && !sig.is_empty()
    {
        template.push_str(sig);
    }

    let outcome = run_editor(&template, config);
    let Ok(outcome) = outcome else {
        state.status = "editor failed".into();
        return;
    };
    if outcome.body_markdown.trim().is_empty() {
        state.status = "empty draft — discarded".into();
        return;
    }

    let draft = kestrel_core::protocol::Draft {
        account: account_id,
        from: kestrel_core::protocol::Address::bare(
            state.account().map(|a| a.email.clone()).unwrap_or_default(),
        ),
        to,
        cc: if reply_all { msg.cc.clone() } else { vec![] },
        bcc: vec![],
        subject: outcome.subject,
        in_reply_to: msg.message_id.clone(),
        references: vec![
            msg.in_reply_to.clone().unwrap_or_default(),
            msg.message_id.clone().unwrap_or_default(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect(),
        body_markdown: outcome.body_markdown,
        attachments: vec![],
        pgp_sign: false,
        pgp_encrypt: false,
        smime_sign: false,
        smime_encrypt: false,
        send_after: None,
        priority: kestrel_core::protocol::Priority::Normal,
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ComposeSubmit { draft, reply: tx },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        state.status = "reply queued".into();
    }
}

async fn compose_forward(
    handle: &EngineHandle,
    state: &mut AppState,
    forward_as_eml: bool,
    config: &Arc<kestrel_core::config::Config>,
) {
    let Some(msg) = state.message() else { return };
    let Some(account_id) = state.account().map(|a| a.id) else {
        return;
    };
    let subject = format!("Fwd: {}", msg.subject.clone().unwrap_or_default());
    let mut template = editor::draft_template(&subject, "");

    // Append per-account signature
    let account_email = state.account().map(|a| a.email.clone()).unwrap_or_default();
    if let Some(sig) = config.account_signatures.get(&account_email)
        && !sig.is_empty()
    {
        template.push_str(sig);
    }

    let outcome = run_editor(&template, config);
    let Ok(outcome) = outcome else {
        state.status = "editor failed".into();
        return;
    };

    let attachments = if forward_as_eml {
        // Serialize original message as RFC 5322 .eml attachment
        let Some(preview) = state.preview.as_ref() else {
            state.status = "no message data for forward".into();
            return;
        };
        let eml_bytes = build_forward_eml(preview);
        vec![kestrel_core::protocol::DraftAttachment {
            name: "forwarded-message.eml".into(),
            mime_type: "message/rfc822".into(),
            data: eml_bytes,
        }]
    } else {
        // Carry original attachments (empty data — fetched on send).
        state
            .preview
            .as_ref()
            .map(|view| {
                view.parts
                    .iter()
                    .filter(|p| p.disposition.as_deref() == Some("attachment"))
                    .map(|p| kestrel_core::protocol::DraftAttachment {
                        name: p.filename.clone().unwrap_or_default(),
                        mime_type: p.mime_type.clone(),
                        data: vec![],
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let draft = kestrel_core::protocol::Draft {
        account: account_id,
        from: kestrel_core::protocol::Address::bare(
            state.account().map(|a| a.email.clone()).unwrap_or_default(),
        ),
        to: vec![],
        cc: vec![],
        bcc: vec![],
        subject: outcome.subject,
        in_reply_to: None,
        references: vec![],
        body_markdown: outcome.body_markdown,
        attachments,
        pgp_sign: false,
        pgp_encrypt: false,
        smime_sign: false,
        smime_encrypt: false,
        send_after: None,
        priority: kestrel_core::protocol::Priority::Normal,
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ComposeSubmit { draft, reply: tx },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        state.status = "forward queued".into();
    }
}

/// Serialize a `MessageView` as RFC 5322 bytes for forward-as-eml.
fn build_forward_eml(msg: &kestrel_core::protocol::MessageView) -> Vec<u8> {
    let summary = &msg.summary;
    let mut out = String::with_capacity(512);

    // From
    let from_str = summary
        .from
        .iter()
        .map(format_tui_address)
        .collect::<Vec<_>>()
        .join(", ");
    if !from_str.is_empty() {
        let _ = writeln!(out, "From: {from_str}");
    }

    // To
    let to_str = summary
        .to
        .iter()
        .map(format_tui_address)
        .collect::<Vec<_>>()
        .join(", ");
    if !to_str.is_empty() {
        let _ = writeln!(out, "To: {to_str}");
    }

    // Cc
    let cc_str = summary
        .cc
        .iter()
        .map(format_tui_address)
        .collect::<Vec<_>>()
        .join(", ");
    if !cc_str.is_empty() {
        let _ = writeln!(out, "Cc: {cc_str}");
    }

    // Subject
    if let Some(subj) = &summary.subject {
        let _ = writeln!(out, "Subject: {subj}");
    }

    // Date (use internal_date as RFC 5322)
    let date_str = format_internal_date(summary.internal_date);
    let _ = writeln!(out, "Date: {date_str}");

    // Message-ID
    if let Some(mid) = &summary.message_id {
        let _ = writeln!(out, "Message-ID: <{mid}>");
    }

    // In-Reply-To
    if let Some(irt) = &summary.in_reply_to {
        let _ = writeln!(out, "In-Reply-To: <{irt}>");
    }

    // Body
    out.push_str("\r\n");
    if let Some(body) = &msg.body_plain {
        out.push_str(body);
    }

    out.into_bytes()
}

fn format_tui_address(addr: &kestrel_core::protocol::Address) -> String {
    match &addr.name {
        Some(name) if !name.is_empty() => format!("{name} <{}>", addr.email),
        _ => addr.email.clone(),
    }
}

fn format_internal_date(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hours, mins, secs) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let dow = (days.rem_euclid(7) + 4) % 7;
    let dow_names: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month_names: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} +0000",
        dow_names[usize::try_from(dow).unwrap_or(0) % 7],
        d,
        month_names[usize::try_from(month - 1).unwrap_or(0) % 12],
        year,
        hours,
        mins,
        secs,
    )
}

async fn compose_new(
    handle: &EngineHandle,
    state: &mut AppState,
    config: &Arc<kestrel_core::config::Config>,
) {
    let Some(account_id) = state.account().map(|a| a.id) else {
        return;
    };
    let mut template = editor::draft_template("New message", "");

    // Append per-account signature
    let account_email = state.account().map(|a| a.email.clone()).unwrap_or_default();
    if let Some(sig) = config.account_signatures.get(&account_email)
        && !sig.is_empty()
    {
        template.push_str(sig);
    }

    let outcome = run_editor(&template, config);
    let Ok(outcome) = outcome else {
        state.status = "editor failed".into();
        return;
    };
    if outcome.body_markdown.trim().is_empty() {
        state.status = "empty draft — discarded".into();
        return;
    }
    let draft = kestrel_core::protocol::Draft {
        account: account_id,
        from: kestrel_core::protocol::Address::bare(
            state.account().map(|a| a.email.clone()).unwrap_or_default(),
        ),
        to: vec![],
        cc: vec![],
        bcc: vec![],
        subject: outcome.subject,
        in_reply_to: None,
        references: vec![],
        body_markdown: outcome.body_markdown,
        attachments: vec![],
        pgp_sign: false,
        pgp_encrypt: false,
        smime_sign: false,
        smime_encrypt: false,
        send_after: None,
        priority: kestrel_core::protocol::Priority::Normal,
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::ComposeSubmit { draft, reply: tx },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        state.status = "message queued".into();
    }
}

/// Handles keys in setup mode.
async fn handle_setup_key(handle: &EngineHandle, state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            state.mode = Mode::Normal;
            state.status = "setup cancelled".into();
        }
        KeyCode::Enter => {
            state.mode = Mode::Normal;
            state.status = "connecting…".into();
            add_account_from_setup(handle, state).await;
        }
        KeyCode::Tab => {
            state.setup_field = (state.setup_field + 1) % 3;
        }
        KeyCode::Backspace => match state.setup_field {
            0 => {
                state.setup_email.pop();
            }
            1 => {
                state.setup_password.pop();
            }
            _ => {
                state.setup_imap_host.pop();
            }
        },
        KeyCode::Char(c) => match state.setup_field {
            0 => state.setup_email.push(c),
            1 => state.setup_password.push(c),
            _ => state.setup_imap_host.push(c),
        },
        _ => {}
    }
}

async fn remove_account(handle: &EngineHandle, state: &mut AppState) {
    let Some(account) = state.account().cloned() else {
        state.status = "no account selected".into();
        state.mode = Mode::Normal;
        return;
    };
    let account_id = account.id;
    state.mode = Mode::Normal;
    state.status = format!("removing account: {}...", account.name);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::RemoveAccount {
                account: account_id,
                reply: tx,
            },
        })
        .await;
    match rx.await {
        Ok(Reply::Accepted) => {
            state.status = format!("removed account: {}", account.name);
            refresh_accounts(handle, state).await;
            if state.account().is_some() {
                refresh_folders(handle, state).await;
            }
        }
        Ok(Reply::Err(e)) => {
            state.status = format!("remove failed: {e}");
        }
        _ => {
            state.status = "remove: unexpected reply".into();
        }
    }
}

async fn add_account_from_setup(handle: &EngineHandle, state: &mut AppState) {
    use kestrel_core::{
        provider::{detect_provider, provider_preset},
        secrets::SecretString,
    };

    let email = state.setup_email.clone();
    let password = state.setup_password.clone();
    let imap_host = state.setup_imap_host.clone();
    if email.is_empty() || !email.contains('@') {
        state.status = "setup: valid email required".into();
        return;
    }
    let provider = detect_provider(&email);
    let mut config = provider_preset(&provider, &email);
    if !imap_host.is_empty() {
        if let Some((h, p)) = imap_host.split_once(':') {
            config.imap_host = h.to_owned();
            config.imap_port = p.parse().unwrap_or(config.imap_port);
        } else {
            config.imap_host = imap_host;
        }
    }

    // Step 1: Test connection
    state.status = "testing connection...".into();
    let (test_tx, test_rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::TestConnection {
                config: config.clone(),
                password: SecretString::new(password.clone()),
                reply: test_tx,
            },
        })
        .await;
    match test_rx.await {
        Ok(Reply::Accepted) => {
            state.status = "connection OK, adding account...".into();
        }
        Ok(Reply::Err(e)) => {
            state.status = format!("setup failed: {e}");
            return;
        }
        _ => {
            state.status = "setup: unexpected reply from connection test".into();
            return;
        }
    }

    // Step 2: Add account
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::AddAccount {
                config,
                password: SecretString::new(password),
                reply: tx,
            },
        })
        .await;
    match rx.await {
        Ok(Reply::Accounts(accounts)) => {
            state.status = format!("{} account(s) — syncing", accounts.len());
            state.accounts = accounts;
        }
        Ok(Reply::Err(e)) => {
            state.status = format!("setup failed: {e}");
        }
        _ => {
            state.status = "setup: unexpected reply".into();
        }
    }
}

/// Preview pane visible lines estimate for scroll bounds.
fn preview_visible_lines(_state: &AppState) -> usize {
    20
}

async fn toggle_flagged(handle: &EngineHandle, state: &mut AppState) {
    let Some(msg) = state.message() else {
        return;
    };
    let id = msg.id;
    let op = if msg.is_flagged {
        kestrel_core::protocol::FlagOp::Remove(vec![kestrel_core::protocol::Flag::Flagged])
    } else {
        kestrel_core::protocol::FlagOp::Add(vec![kestrel_core::protocol::Flag::Flagged])
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: vec![id],
                flags: op,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let label = if state.message().is_some_and(|m| m.is_flagged) {
            "flagged"
        } else {
            "unflagged"
        };
        state.status = label.into();
        refresh_messages(handle, state).await;
    }
}

async fn mark_all_read(handle: &EngineHandle, state: &mut AppState) {
    let ids: Vec<_> = state.page.items.iter().map(|m| m.id).collect();
    if ids.is_empty() {
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: ids,
                flags: kestrel_core::protocol::FlagOp::Add(vec![
                    kestrel_core::protocol::Flag::Seen,
                ]),
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        state.status = "all read".into();
        refresh_messages(handle, state).await;
    }
}

async fn toggle_mark_unread(handle: &EngineHandle, state: &mut AppState) {
    let Some(msg) = state.message() else {
        return;
    };
    let id = msg.id;
    let op = if msg.is_read {
        kestrel_core::protocol::FlagOp::Remove(vec![kestrel_core::protocol::Flag::Seen])
    } else {
        kestrel_core::protocol::FlagOp::Add(vec![kestrel_core::protocol::Flag::Seen])
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: vec![id],
                flags: op,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let label = if state.page.items[state.selected_message].is_read {
            "marked unread"
        } else {
            "marked read"
        };
        state.status = label.into();
        refresh_messages(handle, state).await;
    }
}

#[allow(clippy::unused_async)]
async fn archive_selected(_handle: &EngineHandle, state: &mut AppState) {
    let Some(id) = state.message_id() else {
        return;
    };
    state.status = format!("archived {id}");
}

async fn execute_snooze(handle: &EngineHandle, state: &mut AppState) {
    use kestrel_core::clock::Clock as _;
    let Some(msg) = state.message() else {
        state.mode = Mode::Normal;
        return;
    };
    let Some(account_id) = state.account().map(|a| a.id) else {
        state.mode = Mode::Normal;
        state.status = "no account selected".into();
        return;
    };
    let now = kestrel_core::clock::SystemClock.now_unix_ms();
    let hour_ms = 3_600_000;
    let day_ms = 86_400_000;
    let default_ms = 12 * hour_ms;
    let until = match state.snooze_selection {
        0 => now + default_ms,
        1 => now + 7 * day_ms,
        _ => {
            let hours: i64 = state.snooze_hours.parse().unwrap_or(1);
            now + hours * hour_ms
        }
    };
    let msg_id = msg.id;
    let folder_id = msg.folder;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SnoozeMessage {
                message: msg_id,
                account: account_id,
                folder: folder_id,
                until,
                reply: tx,
            },
        })
        .await;
    state.mode = Mode::Normal;
    state.snooze_hours.clear();
    state.snooze_selection = 0;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        state.status = "message snoozed".into();
        refresh_messages(handle, state).await;
    } else {
        state.status = "snooze failed".into();
    }
}

fn execute_account_command(state: &mut AppState, sub: &str) {
    match sub {
        "list" => {
            if state.accounts.is_empty() {
                state.status = "no accounts configured".into();
            } else {
                let list: Vec<String> = state
                    .accounts
                    .iter()
                    .map(|a| format!("{} ({})", a.name, a.email))
                    .collect();
                state.status = format!("accounts: {}", list.join(", "));
            }
        }
        "edit" => {
            if let Some(acc) = state.account() {
                state.status = format!(
                    "edit account: {} — IMAP: {} ({:?})",
                    acc.name, acc.host, acc.protocol
                );
            } else {
                state.status = "no account selected".into();
            }
        }
        "remove" => {
            if let Some(acc) = state.account() {
                state.status = format!(
                    "remove account: {} ({})? (y to confirm)",
                    acc.name, acc.email
                );
                state.mode = Mode::ConfirmRemoveAccount;
            } else {
                state.status = "no account selected".into();
            }
        }
        _ => {
            state.status = "usage: :account <list|edit|remove>".into();
        }
    }
}

async fn execute_event_command(handle: &EngineHandle, state: &mut AppState, sub: &str) {
    match sub {
        "create" => {
            // Prompt for event details via status line; use a simple
            // inline prompt: title, start, end (ISO-ish format).
            state.status =
                "event create: enter title, start (YYYYMMDDTHHMMSS), end (YYYYMMDDTHHMMSS), \
                 separated by |"
                    .into();
            // For now, create a default test event to prove the pipeline.
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = handle
                .commands
                .send(Command {
                    id: next_request_id(),
                    origin: FrontendKind::Tui,
                    payload: CommandPayload::CreateEvent {
                        calendar_id: String::new(),
                        uid: format!("{}@kestrel", uuid::Uuid::now_v7()),
                        summary: "New Event".into(),
                        description: None,
                        location: None,
                        start_time: kestrel_core::clock::SystemClock.now_unix_ms(),
                        end_time: kestrel_core::clock::SystemClock.now_unix_ms() + 3_600_000,
                        all_day: false,
                        reply: tx,
                    },
                })
                .await;
            match rx.await {
                Ok(Reply::Accepted) => {
                    state.status = "event created".into();
                }
                Ok(Reply::Err(e)) => {
                    state.status = format!("event create failed: {e}");
                }
                _ => {
                    state.status = "event create: unexpected reply".into();
                }
            }
        }
        _ => {
            state.status = "usage: :event create".into();
        }
    }
}

async fn execute_command(handle: &EngineHandle, state: &mut AppState, cmd: &str) {
    let cmd = cmd.strip_prefix(':').unwrap_or(cmd);
    let mut parts = cmd.splitn(3, ' ');
    let verb = parts.next().unwrap_or_default();
    match verb {
        "sort" => {
            let arg = parts.next().unwrap_or_default();
            match arg {
                "date" => {
                    state.sort_field = kestrel_core::protocol::SortField::Date;
                    state.status = format!("sort: date {}", sort_dir_label(state.sort_dir));
                }
                "from" | "sender" => {
                    state.sort_field = kestrel_core::protocol::SortField::Sender;
                    state.status = format!("sort: sender {}", sort_dir_label(state.sort_dir));
                }
                "subject" => {
                    state.sort_field = kestrel_core::protocol::SortField::Subject;
                    state.status = format!("sort: subject {}", sort_dir_label(state.sort_dir));
                }
                "asc" => {
                    state.sort_dir = kestrel_core::protocol::SortDir::Asc;
                    state.status = format!("sort: {} asc", field_label(state.sort_field));
                }
                "desc" => {
                    state.sort_dir = kestrel_core::protocol::SortDir::Desc;
                    state.status = format!("sort: {} desc", field_label(state.sort_field));
                }
                _ => {
                    state.status = "usage: :sort <date|from|subject> or :sort <asc|desc>".into();
                }
            }
        }
        "save-search" => {
            let name = parts.next().unwrap_or_default();
            if name.is_empty() {
                state.status = "usage: :save-search <name>".into();
            } else {
                let query = SearchQuery {
                    text: Some(state.search_input.clone()),
                    ..SearchQuery::default()
                };
                state
                    .saved_searches
                    .push(kestrel_core::config::SavedSearch {
                        name: name.to_string(),
                        query,
                    });
                state.status = format!("search saved: {name}");
            }
        }
        "load-search" => {
            let name = parts.next().unwrap_or_default();
            if name.is_empty() {
                state.status = "usage: :load-search <name>".into();
            } else if let Some(saved) = state.saved_searches.iter().find(|s| s.name == name) {
                let query = saved.query.clone();
                state.status = format!("loaded search: {name}");
                execute_saved_search(handle, state, &query).await;
            } else {
                let names: Vec<&str> = state
                    .saved_searches
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect();
                state.status = format!("unknown search: {name} (available: {})", names.join(", "));
            }
        }
        "list-searches" => {
            if state.saved_searches.is_empty() {
                state.status = "no saved searches".into();
            } else {
                let names: Vec<&str> = state
                    .saved_searches
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect();
                state.status = format!("saved searches: {}", names.join(", "));
            }
        }
        "account" => {
            let sub = parts.next().unwrap_or_default();
            execute_account_command(state, sub);
        }
        "event" => {
            let sub = parts.next().unwrap_or_default();
            execute_event_command(handle, state, sub).await;
        }
        _ => {
            state.status = format!("unknown command: {cmd}");
        }
    }
}

/// Delete all messages selected in multi-select mode.
async fn bulk_delete_selected(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    let mut folders = Vec::new();
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
            folders.push((msg.id, msg.folder));
        }
    }
    if ids.is_empty() {
        state.mode = Mode::Normal;
        state.status.clear();
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::DeleteMessages {
                messages: ids,
                expunge: false,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let count = state.selected_messages.len();
        state.toggle_multi_select();
        state.status = format!("deleted {count} message(s)");
        refresh_messages(handle, state).await;
    }
    state.mode = Mode::Normal;
}

/// Archive all messages selected in multi-select mode.
async fn bulk_archive(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
        }
    }
    if ids.is_empty() {
        return;
    }
    let count = ids.len();
    // For now, just report the action; archive folder lookup requires folder list.
    state.toggle_multi_select();
    state.status = format!("archived {count} message(s)");
    refresh_messages(handle, state).await;
    let _ = handle;
}

/// Toggle read/unread for all messages selected in multi-select mode.
async fn bulk_toggle_unread(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    let mut mark_unread = false;
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
            if msg.is_read {
                mark_unread = true;
            }
        }
    }
    if ids.is_empty() {
        return;
    }
    let op = if mark_unread {
        kestrel_core::protocol::FlagOp::Add(vec![kestrel_core::protocol::Flag::Seen])
    } else {
        kestrel_core::protocol::FlagOp::Remove(vec![kestrel_core::protocol::Flag::Seen])
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: ids,
                flags: op,
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let count = state.selected_messages.len();
        state.status = format!("toggled read status for {count} message(s)");
        refresh_messages(handle, state).await;
    }
}

/// Flag (star) all messages selected in multi-select mode.
async fn bulk_flag_selected(handle: &EngineHandle, state: &mut AppState) {
    let indices: Vec<usize> = state.selected_messages.clone();
    let mut ids = Vec::new();
    for &idx in &indices {
        if let Some(msg) = state.page.items.get(idx) {
            ids.push(msg.id);
        }
    }
    if ids.is_empty() {
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::SetFlags {
                messages: ids,
                flags: kestrel_core::protocol::FlagOp::Add(vec![
                    kestrel_core::protocol::Flag::Flagged,
                ]),
                reply: tx,
            },
        })
        .await;
    if matches!(rx.await, Ok(Reply::Accepted)) {
        let count = state.selected_messages.len();
        state.toggle_multi_select();
        state.status = format!("flagged {count} message(s)");
        refresh_messages(handle, state).await;
    }
}

fn run_editor(
    template: &str,
    config: &Arc<kestrel_core::config::Config>,
) -> std::io::Result<editor::EditorOutcome> {
    // Suspend → edit → resume (message-protocol §6).
    editor::suspend_terminal()?;
    let result = editor::edit_draft(template, config.editor.command.as_deref());
    editor::resume_terminal()?;
    result
}

fn next_request_id() -> kestrel_core::ids::RequestId {
    kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7())
}

async fn execute_saved_search(handle: &EngineHandle, state: &mut AppState, query: &SearchQuery) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .commands
        .send(Command {
            id: next_request_id(),
            origin: FrontendKind::Tui,
            payload: CommandPayload::Search {
                query: query.clone(),
                reply: tx,
            },
        })
        .await;
    if let Ok(Reply::SearchResults(hits)) = rx.await {
        let messages: Vec<kestrel_core::protocol::MessageSummary> =
            hits.iter().map(|h| h.message.clone()).collect();
        let total = hits.len() as u64;
        state.original_page = MessagePage {
            items: messages.clone(),
            total,
        };
        state.page.items = messages;
        state.page.total = total;
        state.selected_message = 0;
        state.status = format!("{} hit(s)", hits.len());
    }
}

fn sort_dir_label(dir: kestrel_core::protocol::SortDir) -> &'static str {
    match dir {
        kestrel_core::protocol::SortDir::Asc => "asc",
        kestrel_core::protocol::SortDir::Desc => "desc",
    }
}

fn field_label(field: kestrel_core::protocol::SortField) -> &'static str {
    match field {
        kestrel_core::protocol::SortField::Date => "date",
        kestrel_core::protocol::SortField::Sender => "sender",
        kestrel_core::protocol::SortField::Subject => "subject",
        kestrel_core::protocol::SortField::Uid => "uid",
    }
}
