//! Folder selection, message list, message preview, and message actions.

use std::sync::Arc;

use kestrel_core::{
    ids::MessageId,
    protocol::{
        BodyPreference, Command, CommandPayload, Flag, FlagOp, FrontendKind, Reply, SearchQuery,
        SortSpec, Window,
    },
};
use slint::ComponentHandle as _;

use crate::{state::GuiState, util::show_toast};

/// Wire folder/message selection, search, and message action callbacks.
pub(crate) fn install(state: &GuiState) {
    let Some(app) = state.app_weak.upgrade() else {
        return;
    };

    wire_search(state, &app);
    wire_select_folder(state, &app);
    wire_select_message(state, &app);
    wire_message_actions(state, &app);
    wire_navigation_keys(state, &app);
}

// ────────────────────── search ──────────────────────

fn wire_search(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let mids = Arc::clone(&state.message_ids);
    app.on_search(move |query| {
        let h = h.clone();
        let w = w.clone();
        let mids = Arc::clone(&mids);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = h
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::Search {
                            query: SearchQuery {
                                text: if query.is_empty() {
                                    None
                                } else {
                                    Some(query.to_string())
                                },
                                ..SearchQuery::default()
                            },
                            reply: tx,
                        },
                    })
                    .await;
                if let Ok(Reply::SearchResults(hits)) = rx.await {
                    let subjects: Vec<String> = hits
                        .iter()
                        .map(|h| {
                            h.message
                                .subject
                                .clone()
                                .unwrap_or_else(|| "(no subject)".into())
                        })
                        .collect();
                    let froms: Vec<String> = hits
                        .iter()
                        .map(|h| {
                            h.message
                                .from
                                .first()
                                .and_then(|a| a.name.as_deref().or(Some(&*a.email)))
                                .unwrap_or("(unknown)")
                                .to_string()
                        })
                        .collect();
                    let dates: Vec<String> = hits
                        .iter()
                        .map(|h| kestrel_core::time::format_datetime(h.message.internal_date))
                        .collect();
                    let ids: Vec<MessageId> = hits.iter().map(|h| h.message.id).collect();
                    let thread_depths: Vec<i32> = hits
                        .iter()
                        .map(|h| i32::from(h.message.in_reply_to.is_some()))
                        .collect();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w.upgrade() {
                            let count = i32::try_from(subjects.len()).unwrap_or(i32::MAX);
                            let ss: Vec<slint::SharedString> = subjects
                                .iter()
                                .map(|s| slint::SharedString::from(s.as_str()))
                                .collect();
                            let fs: Vec<slint::SharedString> = froms
                                .iter()
                                .map(|s| slint::SharedString::from(s.as_str()))
                                .collect();
                            let ds: Vec<slint::SharedString> = dates
                                .iter()
                                .map(|s| slint::SharedString::from(s.as_str()))
                                .collect();
                            let td: Vec<i32> = thread_depths;
                            app.set_message_subjects(ss.as_slice().into());
                            app.set_message_froms(fs.as_slice().into());
                            app.set_message_dates(ds.as_slice().into());
                            app.set_thread_depths(td.as_slice().into());
                            app.set_total_messages(count);
                            app.set_selected_msg_idx(-1);
                            app.set_status_text(format!("{count} results").into());
                        }
                        if let Ok(mut mid) = mids.lock() {
                            *mid = ids;
                        }
                    })
                    .ok();
                }
            });
        });
    });
}

// ────────────────────── select folder ──────────────────────

fn wire_select_folder(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let fids = Arc::clone(&state.folder_ids);
    let mids = Arc::clone(&state.message_ids);
    app.on_select_folder(move |idx| {
        let idx = usize::try_from(idx).unwrap_or(0);
        let folder_id = {
            let ids = fids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match ids.get(idx) {
                Some(id) => *id,
                None => return,
            }
        };
        let is_unified = folder_id == kestrel_core::ids::FolderId::from_uuid(uuid::Uuid::nil());
        let h = h.clone();
        let w = w.clone();
        let mids = Arc::clone(&mids);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let w2 = w.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w2.upgrade() {
                        app.set_loading_messages(true);
                    }
                })
                .ok();
                let (tx, rx) = tokio::sync::oneshot::channel();
                let payload = if is_unified {
                    CommandPayload::ListUnifiedInbox {
                        window: Window {
                            offset: 0,
                            limit: 50,
                        },
                        sort: SortSpec::default(),
                        reply: tx,
                    }
                } else {
                    CommandPayload::ListMessages {
                        folder: folder_id,
                        window: Window {
                            offset: 0,
                            limit: 50,
                        },
                        sort: SortSpec::default(),
                        reply: tx,
                    }
                };
                let _ = h
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload,
                    })
                    .await;
                if let Ok(Reply::Messages(page)) = rx.await {
                    let subjects: Vec<String> = page
                        .items
                        .iter()
                        .map(|m| m.subject.clone().unwrap_or_else(|| "(no subject)".into()))
                        .collect();
                    let froms: Vec<String> = page
                        .items
                        .iter()
                        .map(|m| {
                            m.from
                                .first()
                                .and_then(|a| a.name.as_deref().or(Some(&*a.email)))
                                .unwrap_or("(unknown)")
                                .to_string()
                        })
                        .collect();
                    let dates: Vec<String> = page
                        .items
                        .iter()
                        .map(|m| kestrel_core::time::format_datetime(m.internal_date))
                        .collect();
                    let ids: Vec<MessageId> = page.items.iter().map(|m| m.id).collect();
                    let total = i32::try_from(page.total).unwrap_or(0);
                    let thread_depths: Vec<i32> = page
                        .items
                        .iter()
                        .map(|m| i32::from(m.in_reply_to.is_some()))
                        .collect();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w.upgrade() {
                            let ss: Vec<slint::SharedString> = subjects
                                .iter()
                                .map(|s| slint::SharedString::from(s.as_str()))
                                .collect();
                            let fs: Vec<slint::SharedString> = froms
                                .iter()
                                .map(|s| slint::SharedString::from(s.as_str()))
                                .collect();
                            let ds: Vec<slint::SharedString> = dates
                                .iter()
                                .map(|s| slint::SharedString::from(s.as_str()))
                                .collect();
                            let td: Vec<i32> = thread_depths;
                            app.set_message_subjects(ss.as_slice().into());
                            app.set_message_froms(fs.as_slice().into());
                            app.set_message_dates(ds.as_slice().into());
                            app.set_thread_depths(td.as_slice().into());
                            app.set_total_messages(total);
                            app.set_selected_msg_idx(-1);
                            app.set_preview_from(slint::SharedString::default());
                            app.set_preview_subject(slint::SharedString::default());
                            app.set_preview_body(slint::SharedString::default());
                            app.set_loading_messages(false);
                            app.set_status_text(format!("{total} messages").into());
                        }
                        if let Ok(mut mid) = mids.lock() {
                            *mid = ids;
                        }
                    })
                    .ok();
                }
            });
        });
    });
}

// ────────────────────── select message ──────────────────────

fn wire_select_message(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let mids = Arc::clone(&state.message_ids);
    let att_keys_outer = Arc::clone(&state.current_attachment_keys);
    let att_msg_outer = Arc::clone(&state.current_message_for_attachments);
    let html_cache = Arc::clone(&state.current_message_html);

    app.on_select_message(move |idx| {
        let idx = usize::try_from(idx).unwrap_or(0);
        let message_id = {
            let ids = mids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match ids.get(idx) {
                Some(id) => *id,
                None => return,
            }
        };
        let h = h.clone();
        let w = w.clone();
        let att_keys = Arc::clone(&att_keys_outer);
        let att_msg = Arc::clone(&att_msg_outer);
        let html_cache2 = Arc::clone(&html_cache);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let w2 = w.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w2.upgrade() {
                        app.set_loading_preview(true);
                    }
                })
                .ok();
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = h
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::GetMessage {
                            message: message_id,
                            body: BodyPreference::Full,
                            reply: tx,
                        },
                    })
                    .await;
                if let Ok(Reply::Message(view)) = rx.await {
                    let from = view
                        .summary
                        .from
                        .first()
                        .and_then(|a| a.name.as_deref().or(Some(&*a.email)))
                        .unwrap_or("(unknown)")
                        .to_string();
                    let subject = view.summary.subject.unwrap_or_default();
                    let is_html = view.body_html.is_some() && view.body_plain.is_none();
                    let body = view.body_plain.unwrap_or_default();
                    let raw_html = view.body_html.clone();
                    let remote_blocked = raw_html
                        .as_deref()
                        .map_or(0, kestrel_core::sanitizer::count_remote_refs);
                    {
                        if let Ok(mut cache) = html_cache2.lock() {
                            *cache = raw_html;
                        }
                    }
                    let attachments: Vec<(String, String, String)> = view
                        .parts
                        .iter()
                        .filter(|p| {
                            p.disposition.as_deref() == Some("attachment") || p.filename.is_some()
                        })
                        .map(|p| {
                            let name = p.filename.clone().unwrap_or_else(|| "unnamed".into());
                            #[allow(clippy::cast_precision_loss)]
                            let size = if p.byte_size >= 1_048_576 {
                                format!("{:.1} MB", p.byte_size as f64 / 1_048_576.0)
                            } else if p.byte_size >= 1024 {
                                format!("{:.1} KB", p.byte_size as f64 / 1024.0)
                            } else {
                                format!("{} B", p.byte_size)
                            };
                            (name, size, p.id.key.clone())
                        })
                        .collect();
                    let att_names: Vec<slint::SharedString> = attachments
                        .iter()
                        .map(|(n, _, _)| slint::SharedString::from(n.as_str()))
                        .collect();
                    let att_sizes: Vec<slint::SharedString> = attachments
                        .iter()
                        .map(|(_, s, _)| slint::SharedString::from(s.as_str()))
                        .collect();
                    let att_keys_clone: Vec<String> =
                        attachments.iter().map(|(_, _, k)| k.clone()).collect();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = w.upgrade() {
                            app.set_preview_from(from.into());
                            app.set_preview_subject(subject.into());
                            app.set_preview_body(body.into());
                            app.set_preview_is_html(is_html);
                            app.set_loading_preview(false);
                            app.set_attachment_names(att_names.as_slice().into());
                            app.set_attachment_sizes(att_sizes.as_slice().into());
                            app.set_remote_blocked_count(
                                i32::try_from(remote_blocked).unwrap_or(0),
                            );
                            app.set_show_remote_content(false);
                            app.set_show_find_bar(false);
                            app.set_find_query(slint::SharedString::default());
                            app.set_find_results(0);
                        }
                    })
                    .ok();
                    {
                        if let Ok(mut keys) = att_keys.lock() {
                            *keys = att_keys_clone;
                        }
                        if let Ok(mut msg) = att_msg.lock() {
                            *msg = Some(message_id);
                        }
                    }
                    // Mark message as read
                    let (tx_read, _rx_read) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::SetFlags {
                                messages: vec![message_id],
                                flags: FlagOp::Add(vec![Flag::Seen]),
                                reply: tx_read,
                            },
                        })
                        .await;
                }
            });
        });
    });
}

// ────────────────────── message actions ──────────────────────

fn wire_message_actions(state: &GuiState, app: &crate::AppWindow) {
    // ── Delete ──
    {
        let h = state.handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&state.message_ids);
        app.on_delete_message(move || {
            let selected_idx = {
                let Some(app_ref) = w.upgrade() else { return };
                let idx = app_ref.get_selected_msg_idx();
                if idx < 0 {
                    return;
                }
                usize::try_from(idx).unwrap_or(0)
            };
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(selected_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::DeleteMessages {
                                messages: vec![message_id],
                                expunge: false,
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message deleted", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        });
    }

    // ── Archive ──
    {
        let h = state.handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&state.message_ids);
        let fids = Arc::clone(&state.folder_ids);
        app.on_archive_message(move || {
            let selected_msg_idx = {
                let Some(app_ref) = w.upgrade() else { return };
                let idx = app_ref.get_selected_msg_idx();
                if idx < 0 {
                    return;
                }
                usize::try_from(idx).unwrap_or(0)
            };
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(selected_msg_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let archive_folder_id = {
                let ids = fids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ids.iter()
                    .find(|id| **id != kestrel_core::ids::FolderId::from_uuid(uuid::Uuid::nil()))
                    .copied()
            };
            let Some(dest) = archive_folder_id else {
                let w_err = w.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w_err.upgrade() {
                        show_toast(&app, "No archive folder available", "error");
                    }
                })
                .ok();
                return;
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::MoveMessages {
                                messages: vec![message_id],
                                to: dest,
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message archived", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        });
    }

    // ── Flag ──
    {
        let h = state.handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&state.message_ids);
        app.on_flag_message(move || {
            let selected_idx = {
                let Some(app_ref) = w.upgrade() else { return };
                let idx = app_ref.get_selected_msg_idx();
                if idx < 0 {
                    return;
                }
                usize::try_from(idx).unwrap_or(0)
            };
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(selected_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::SetFlags {
                                messages: vec![message_id],
                                flags: FlagOp::Add(vec![Flag::Flagged]),
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message flagged", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        });
    }

    // ── Move (drag-and-drop) ──
    {
        let h = state.handle.clone();
        let w = app.as_weak();
        let mids = Arc::clone(&state.message_ids);
        let fids = Arc::clone(&state.folder_ids);
        app.on_move_message_to_folder(move |msg_idx, folder_idx| {
            let msg_idx = usize::try_from(msg_idx).unwrap_or(0);
            let dest_folder_idx = usize::try_from(folder_idx).unwrap_or(0);
            let message_id = {
                let ids = mids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(msg_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let dest_folder_id = {
                let ids = fids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match ids.get(dest_folder_idx) {
                    Some(id) => *id,
                    None => return,
                }
            };
            let h = h.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = h
                        .commands
                        .send(Command {
                            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                            origin: FrontendKind::Gui,
                            payload: CommandPayload::MoveMessages {
                                messages: vec![message_id],
                                to: dest_folder_id,
                                reply: tx,
                            },
                        })
                        .await;
                    if matches!(rx.await, Ok(Reply::Accepted)) {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Message moved", "success");
                            }
                        })
                        .ok();
                    }
                });
            });
        });
    }
}

// ────────────────────── keyboard navigation ──────────────────────

fn wire_navigation_keys(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_select_next_message(move || {
        let Some(app) = w.upgrade() else { return };
        let total = app.get_total_messages();
        let current = app.get_selected_msg_idx();
        let next = if current < 0 {
            0
        } else if current + 1 < total {
            current + 1
        } else {
            return;
        };
        app.set_selected_msg_idx(next);
    });

    let w = app.as_weak();
    app.on_select_prev_message(move || {
        let Some(app) = w.upgrade() else { return };
        let current = app.get_selected_msg_idx();
        if current <= 0 {
            return;
        }
        app.set_selected_msg_idx(current - 1);
    });
}
