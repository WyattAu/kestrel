//! Event loop: input (crossterm) + engine events (broadcast → mpsc →
//! redraw). The loop never awaits unbounded-latency futures; it polls
//! with a 50 ms timeout (architecture §3.2 non-blocking guarantee).

use std::{sync::Arc, time::Duration};

use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use kestrel_core::protocol::{
    Command, CommandPayload, EngineEvent, FrontendKind, Reply, SearchQuery, Window,
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
    let mut terminal = ratatui::init();
    let (tx, mut rx) = mpsc::channel::<TermEvent>(256);

    // Forward engine broadcast events into the TUI channel.
    let fwd_tx = tx.clone();
    let mut events = handle.events.resubscribe();
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
        Mode::Normal => match key.code {
            KeyCode::Char('q') => {
                state.mode = Mode::Confirm;
            }
            KeyCode::Tab => state.cycle_focus(),
            KeyCode::Char('j') | KeyCode::Down => state.move_down(),
            KeyCode::Char('k') | KeyCode::Up => state.move_up(),
            KeyCode::Char('J') | KeyCode::PageDown => state.page_down(),
            KeyCode::Char('K') | KeyCode::PageUp => state.page_up(),
            KeyCode::Char('g') | KeyCode::Home => {
                state.selected_message = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                state.selected_message = state.page.items.len().saturating_sub(1);
            }
            KeyCode::Enter => {
                if state.focus == Focus::List
                    && let Some(id) = state.message_id()
                {
                    load_preview(handle, state, id).await;
                    state.focus = Focus::Preview;
                } else {
                    state.enter();
                }
            }
            KeyCode::Left | KeyCode::Char('h') => state.back(),
            KeyCode::Right | KeyCode::Char('l') => state.enter(),
            KeyCode::Char('/') => {
                state.mode = Mode::Search;
                state.search_input.clear();
            }
            KeyCode::Char('d') => {
                delete_selected(handle, state).await;
            }
            KeyCode::Char('r' | 'a') => {
                compose_reply(handle, state, key.code == KeyCode::Char('a'), config).await;
            }
            KeyCode::Char('f') => {
                compose_forward(handle, state, config).await;
            }
            KeyCode::Char('c') => {
                compose_new(handle, state, config).await;
            }
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
    let template = editor::draft_template(
        &subject,
        &to.iter()
            .map(|a| a.email.clone())
            .collect::<Vec<_>>()
            .join(", "),
    );

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
    config: &Arc<kestrel_core::config::Config>,
) {
    let Some(msg) = state.message() else { return };
    let Some(account_id) = state.account().map(|a| a.id) else {
        return;
    };
    let subject = format!("Fwd: {}", msg.subject.clone().unwrap_or_default());
    let template = editor::draft_template(&subject, "");
    let outcome = run_editor(&template, config);
    let Ok(outcome) = outcome else {
        state.status = "editor failed".into();
        return;
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
        attachments: vec![],
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

async fn compose_new(
    handle: &EngineHandle,
    state: &mut AppState,
    config: &Arc<kestrel_core::config::Config>,
) {
    let Some(account_id) = state.account().map(|a| a.id) else {
        return;
    };
    let template = editor::draft_template("New message", "");
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
