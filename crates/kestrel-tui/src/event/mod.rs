//! Event loop: input (crossterm) + engine events (broadcast → mpsc → redraw).
//! Polls with a 50 ms timeout (architecture §3.2). Submodules (issue #4):
//! `engine` (refresh), `message` (actions), `compose`, `account`, `command`, `fmt`.

use std::{sync::Arc, time::Duration};

use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use kestrel_core::protocol::EngineEvent;
use kestrel_engine::EngineHandle;
use tokio::sync::mpsc;

use crate::{
    app::{AppState, Focus, Mode},
    ui,
};

mod account;
mod command;
mod compose;
mod engine;
mod fmt;
mod message;

use self::{
    account::{handle_setup_key, remove_account},
    command::execute_command,
    compose::{compose_forward, compose_new, compose_reply},
    engine::{
        handle_engine_event, load_preview, refresh_accounts, refresh_folders, refresh_messages,
        search,
    },
    fmt::preview_visible_lines,
    message::{
        archive_selected, bulk_archive, bulk_delete_selected, bulk_flag_selected,
        bulk_toggle_unread, delete_selected, execute_snooze, mark_all_read, toggle_flagged,
        toggle_mark_unread,
    },
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
    mut sla_probe: Option<&mut crate::sla::ColdStartProbe>,
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
            // First presented frame = time-to-interactive marker
            // (phase-3 gate 1). Sticky: later draws don't overwrite it.
            if let Some(probe) = sla_probe.as_deref_mut() {
                probe.mark_first_frame();
            }
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
    // SLA enforcement (phase-3 gate 1): the report is written at
    // first-frame time (the harness kills the app mid-loop); a caller can
    // additionally request a hard failure on breach via KESTREL_SLA_ENFORCE.
    if let Some(probe) = sla_probe
        && let Some(ms) = probe.first_frame_ms()
        && let Err(breach) = crate::sla::ColdStartProbe::check_sla(ms)
        && std::env::var_os("KESTREL_SLA_ENFORCE").is_some()
    {
        return Err(std::io::Error::other(breach));
    }
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

pub(crate) fn next_request_id() -> kestrel_core::ids::RequestId {
    kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7())
}
