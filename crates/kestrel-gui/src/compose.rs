//! Compose window, contacts autocomplete, templates, attachment handling,
//! HTML view, remote content toggle, and find bar.

use std::sync::Arc;

use kestrel_core::{
    clock::Clock as _,
    ids::AccountId,
    protocol::{
        Address, BodyPreference, Command, CommandPayload, Draft, FrontendKind, PartIdView, Reply,
    },
};
use slint::ComponentHandle as _;

use crate::{
    state::GuiState,
    util::{show_toast, update_contact_suggestions_gui},
};

/// Wire all compose, attachment, HTML, and find-bar callbacks.
pub(crate) fn install(state: &GuiState) {
    let Some(app) = state.app_weak.upgrade() else {
        return;
    };

    wire_compose(state, &app);
    wire_compose_cancel(state, &app);
    wire_image_paste(state, &app);
    wire_bcc_toggle(state, &app);
    wire_schedule_send(state, &app);
    wire_compose_preview(state, &app);
    wire_contacts_autocomplete(state, &app);
    wire_templates(state, &app);
    wire_priority(state, &app);
    wire_compose_submit(state, &app);
    wire_save_attachment(state, &app);
    wire_file_drop(state, &app);
    wire_html_view(state, &app);
    wire_remote_content(state, &app);
    wire_find_bar(state, &app);
}

// ────────────────────── compose (reply/forward) ──────────────────────

fn wire_compose(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let lists = Arc::clone(&state.lists);
    let reply = Arc::clone(&state.reply);

    app.on_compose(move || {
        let selected_idx = {
            let Some(app_ref) = w.upgrade() else { return };
            let idx = app_ref.get_selected_msg_idx();
            if idx < 0 {
                let Some(app) = w.upgrade() else { return };
                reply.clear();
                app.set_show_compose(true);
                app.set_compose_error(slint::SharedString::default());
                return;
            }
            usize::try_from(idx).unwrap_or(0)
        };
        let Some(message_id) = lists.message_at(selected_idx) else {
            let Some(app) = w.upgrade() else { return };
            reply.clear();
            app.set_show_compose(true);
            app.set_compose_error(slint::SharedString::default());
            return;
        };
        let h = h.clone();
        let w = w.clone();
        let reply2 = Arc::clone(&reply);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
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
                match rx.await {
                    Ok(Reply::Message(view)) => {
                        let sender = view
                            .summary
                            .from
                            .first()
                            .map(|a| a.name.as_deref().unwrap_or(&a.email).to_string())
                            .unwrap_or_default();
                        let sender_email = view
                            .summary
                            .from
                            .first()
                            .map(|a| a.email.clone())
                            .unwrap_or_default();
                        let subject = view.summary.subject.unwrap_or_default();
                        let date = kestrel_core::time::format_datetime(view.summary.internal_date);
                        let plain = view.body_plain.unwrap_or_default();
                        let quoted: String = plain
                            .lines()
                            .map(|line| format!("> {line}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let compose_body = format!("\n\nOn {date}, {sender} wrote:\n{quoted}");
                        let compose_to = if sender_email.is_empty() {
                            String::new()
                        } else {
                            format!("{sender} <{sender_email}>")
                        };
                        let compose_subject = format!("Re: {subject}");
                        reply2.start_reply_from(
                            view.summary.in_reply_to.clone(),
                            view.summary.message_id.clone(),
                        );
                        let compose_cc: String = view
                            .summary
                            .cc
                            .iter()
                            .map(|a| {
                                if let Some(ref name) = a.name {
                                    format!("{} <{}>", name, a.email)
                                } else {
                                    a.email.clone()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                app.set_compose_to(compose_to.into());
                                app.set_compose_subject(compose_subject.into());
                                app.set_compose_body(compose_body.into());
                                app.set_compose_cc(compose_cc.into());
                                app.set_show_compose(true);
                                app.set_compose_error(slint::SharedString::default());
                            }
                        })
                        .ok();
                    }
                    Ok(Reply::Err(e)) => {
                        let msg = e.user_message();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                app.set_compose_error(msg.into());
                                app.set_show_compose(true);
                            }
                        })
                        .ok();
                    }
                    _ => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                app.set_show_compose(true);
                                app.set_compose_error(slint::SharedString::default());
                            }
                        })
                        .ok();
                    }
                }
            });
        });
    });
}

fn wire_compose_cancel(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_compose_cancel(move || {
        if let Some(app) = w.upgrade() {
            app.set_show_compose(false);
            app.set_show_bcc(false);
            app.set_show_schedule_input(false);
            app.set_compose_send_after(0);
        }
    });
}

fn wire_image_paste(state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    let draft = Arc::clone(&state.draft);
    app.on_compose_body_pasted({
        let w = w.clone();
        move |mime_hint| {
            let Some(app) = w.upgrade() else { return };
            let _ = &draft;
            #[cfg(feature = "tray")]
            {
                let mime_str = mime_hint.to_string();
                match arboard::Clipboard::new() {
                    Ok(mut clipboard) => {
                        if let Ok(img) = clipboard.get_image() {
                            let width = img.width;
                            let height = img.height;
                            let bytes = img.bytes.clone();
                            let mime = if mime_str.contains("jpeg") || mime_str.contains("jpg") {
                                "image/jpeg"
                            } else {
                                "image/png"
                            };
                            let ext = if mime == "image/jpeg" { "jpg" } else { "png" };
                            let name = format!("pasted-image-{width}x{height}.{ext}");
                            let attachment = kestrel_core::protocol::DraftAttachment {
                                name: name.clone(),
                                mime_type: mime.to_string(),
                                data: bytes.to_vec(),
                            };
                            draft.attach(attachment);
                            let mut names: String = app.get_compose_attachment_names().to_string();
                            if !names.is_empty() {
                                names.push_str(", ");
                            }
                            names.push_str(&name);
                            app.set_compose_attachment_names(names.into());
                            app.set_status_text(format!("Pasted image: {name}").into());
                        } else {
                            app.set_compose_error("No image found in clipboard".into());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("clipboard access failed: {e}");
                        app.set_compose_error(format!("Clipboard error: {e}").into());
                    }
                }
            }
            #[cfg(not(feature = "tray"))]
            {
                let _ = app;
                let _ = mime_hint;
            }
        }
    });
}

fn wire_bcc_toggle(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_toggle_bcc(move || {
        if let Some(app) = w.upgrade() {
            let current = app.get_show_bcc();
            app.set_show_bcc(!current);
        }
    });
}

fn wire_schedule_send(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_schedule_send(move |hours_str| {
        let Some(app) = w.upgrade() else { return };
        let hours_str = hours_str.to_string();
        match hours_str.parse::<f64>() {
            Ok(hours) if hours > 0.0 => {
                let now_ms = kestrel_core::clock::SystemClock.now_unix_ms();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let delay_ms = (hours * 3600.0 * 1000.0) as i64;
                let combined = now_ms.saturating_add(delay_ms);
                app.set_compose_send_after(i32::try_from(combined).unwrap_or(i32::MAX));
                app.set_show_schedule_input(false);
                app.set_status_text(format!("Scheduled: send in {hours} hours").into());
            }
            _ => {
                app.set_compose_error("invalid hours value".into());
            }
        }
    });
}

fn wire_compose_preview(state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    let vp = Arc::clone(&state.vp_state);
    app.on_toggle_compose_preview(move || {
        let Some(app) = w.upgrade() else { return };
        let current = app.get_compose_preview_mode();
        app.set_compose_preview_mode(!current);
        if !current {
            let body = app.get_compose_body();
            if !body.is_empty() {
                let html = kestrel_core::compose::markdown_to_html(&body);
                let wrapped = kestrel_gui::wrap_html_with_csp(&html);
                let parts = {
                    let state = vp.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.parts_for_display()
                };
                if let Err(e) = kestrel_gui::viewport::spawn_wry_viewport(&wrapped, parts) {
                    tracing::warn!("compose preview viewport: {e}");
                }
            }
        }
    });
}

fn wire_contacts_autocomplete(state: &GuiState, app: &crate::AppWindow) {
    let contacts_cache: Arc<std::sync::Mutex<Vec<kestrel_core::protocol::ContactSummary>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let h_contacts = state.handle.clone();
    let w_contacts = app.as_weak();

    // Pre-load contacts when compose is opened
    app.on_compose_to_edited({
        let h = h_contacts.clone();
        let w = w_contacts.clone();
        let cache = Arc::clone(&contacts_cache);
        move |text| {
            let text_str = text.to_string();
            if text_str.is_empty() {
                if let Some(app) = w.upgrade() {
                    app.set_show_contact_suggestions(false);
                    app.set_contact_suggestions(vec![].as_slice().into());
                }
                return;
            }
            let cache_empty = { cache.lock().map_or(true, |c| c.is_empty()) };
            if cache_empty {
                let h2 = h.clone();
                let w2 = w.clone();
                let cache2 = Arc::clone(&cache);
                let query = text_str.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(async move {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        let _ = h2
                            .commands
                            .send(Command {
                                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                                origin: FrontendKind::Gui,
                                payload: CommandPayload::ListAccounts { reply: tx },
                            })
                            .await;
                        let account_id = if let Ok(Reply::Accounts(accts)) = rx.await {
                            accts.first().map(|a| a.id)
                        } else {
                            None
                        };
                        let Some(account_id) = account_id else { return };
                        let (tx2, rx2) = tokio::sync::oneshot::channel();
                        let _ = h2
                            .commands
                            .send(Command {
                                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                                origin: FrontendKind::Gui,
                                payload: CommandPayload::ListContacts {
                                    account: account_id,
                                    reply: tx2,
                                },
                            })
                            .await;
                        if let Ok(Reply::Contacts(contacts)) = rx2.await {
                            if let Ok(mut c) = cache2.lock() {
                                *c = contacts;
                            }
                            update_contact_suggestions_gui(&w2, &cache2, &query);
                        }
                    });
                });
            } else {
                update_contact_suggestions_gui(&w, &cache, &text_str);
            }
        }
    });

    // Select a contact suggestion
    app.on_select_contact_suggestion({
        let w = w_contacts.clone();
        let cache = Arc::clone(&contacts_cache);
        move |idx| {
            let idx = usize::try_from(idx).unwrap_or(0);
            let entry = { cache.lock().ok().and_then(|c| c.get(idx).cloned()) };
            if let Some(contact) = entry {
                let email_entry = if contact.display_name.is_empty() {
                    contact.email.clone()
                } else {
                    format!("{} <{}>", contact.display_name, contact.email)
                };
                if let Some(app) = w.upgrade() {
                    let current_to = app.get_compose_to().to_string();
                    let new_to = if current_to.is_empty() {
                        email_entry
                    } else {
                        format!("{current_to}, {email_entry}")
                    };
                    app.set_compose_to(new_to.into());
                    app.set_show_contact_suggestions(false);
                    app.set_contact_suggestions(vec![].as_slice().into());
                }
            }
        }
    });
}

fn wire_templates(state: &GuiState, app: &crate::AppWindow) {
    let cfg = Arc::clone(&state.config);
    app.on_apply_template({
        let w = app.as_weak();
        move |name| {
            let name_str = name.to_string();
            let body = cfg.templates.get(&name_str).cloned().unwrap_or_default();
            if let Some(app) = w.upgrade() {
                let current = app.get_compose_body().to_string();
                let new_body = if current.is_empty() {
                    body
                } else {
                    format!("{current}\n\n{body}")
                };
                app.set_compose_body(new_body.into());
            }
        }
    });
}

fn wire_priority(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_set_priority(move |idx| {
        if let Some(app) = w.upgrade() {
            app.set_compose_priority_idx(idx);
        }
    });
}

fn wire_compose_submit(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let accounts_cache = Arc::clone(&state.accounts);
    let cfg_compose = Arc::clone(&state.config);
    let reply_compose = Arc::clone(&state.reply);
    let draft_compose = Arc::clone(&state.draft);

    app.on_compose_submit(move |to, cc, bcc, subject, body| {
        let Some(app) = w.upgrade() else { return };
        app.set_compose_busy(true);
        app.set_compose_error(slint::SharedString::default());

        let to_addrs: Vec<Address> = to
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Address::bare(s.to_string()))
            .collect();
        let cc_addrs: Vec<Address> = if cc.is_empty() {
            vec![]
        } else {
            cc.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Address::bare(s.to_string()))
                .collect()
        };
        let bcc_addrs: Vec<Address> = if bcc.is_empty() {
            vec![]
        } else {
            bcc.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Address::bare(s.to_string()))
                .collect()
        };
        if to_addrs.is_empty() {
            app.set_compose_error("no recipients".into());
            app.set_compose_busy(false);
            return;
        }

        let pgp_sign = app.get_compose_pgp_sign();
        let pgp_encrypt = app.get_compose_pgp_encrypt();

        let send_after_val = app.get_compose_send_after();
        let send_after: Option<i64> = if send_after_val > 0 {
            Some(i64::from(send_after_val))
        } else {
            None
        };

        let compose_from = app.get_compose_from().to_string();
        let compose_priority_idx = app.get_compose_priority_idx();

        let priority = match compose_priority_idx {
            0 => kestrel_core::protocol::Priority::High,
            2 => kestrel_core::protocol::Priority::Low,
            _ => kestrel_core::protocol::Priority::Normal,
        };

        let (account_id, account_email) = {
            let ids = accounts_cache.ids();
            let emails = accounts_cache.emails();
            let idx = usize::try_from(app.get_selected_folder_idx()).unwrap_or(0);
            let account_id = ids.get(idx).copied().unwrap_or_else(|| {
                ids.first()
                    .copied()
                    .unwrap_or_else(|| AccountId::from_uuid(uuid::Uuid::now_v7()))
            });
            let account_email = emails
                .get(idx)
                .cloned()
                .or_else(|| emails.first().cloned())
                .unwrap_or_default();
            (account_id, account_email)
        };

        let from_email = if compose_from.is_empty() {
            account_email.clone()
        } else {
            compose_from
        };

        let body_with_sig = {
            let sig = cfg_compose
                .account_signatures
                .get(&account_email)
                .cloned()
                .filter(|s| !s.is_empty());
            if let Some(sig) = sig {
                format!("{body}\n\n{sig}")
            } else {
                body.to_string()
            }
        };

        let draft = Draft {
            account: account_id,
            from: Address::bare(from_email),
            to: to_addrs,
            cc: cc_addrs,
            bcc: bcc_addrs,
            subject: subject.to_string(),
            in_reply_to: (reply_compose.headers()).0,
            references: (reply_compose.headers()).1,
            body_markdown: body_with_sig,
            attachments: draft_compose.take_attachments(),
            pgp_sign,
            pgp_encrypt,
            smime_sign: false,
            smime_encrypt: false,
            send_after,
            priority,
        };

        let h2 = h.clone();
        let w2 = w.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = h2
                    .commands
                    .send(Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: CommandPayload::ComposeSubmit { draft, reply: tx },
                    })
                    .await;
                match rx.await {
                    Ok(Reply::Accepted) => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                app.set_show_compose(false);
                                app.set_compose_busy(false);
                                show_toast(&app, "Message queued for sending", "success");
                            }
                        })
                        .ok();
                    }
                    Ok(Reply::Err(e)) => {
                        let msg = e.user_message();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                show_toast(&app, &msg, "error");
                                app.set_compose_busy(false);
                            }
                        })
                        .ok();
                    }
                    _ => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                show_toast(&app, "Unexpected error sending message", "error");
                                app.set_compose_busy(false);
                            }
                        })
                        .ok();
                    }
                }
            });
        });
    });
}

fn wire_save_attachment(state: &GuiState, app: &crate::AppWindow) {
    let h = state.handle.clone();
    let w = app.as_weak();
    let message_view = Arc::clone(&state.message_view);

    app.on_save_attachment(move |idx| {
        let idx = usize::try_from(idx).unwrap_or(0);
        let Some((part_key, message_id)) = message_view.attachment_at(idx) else {
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
                        payload: CommandPayload::GetAttachment {
                            message: message_id,
                            part: PartIdView {
                                key: part_key.clone(),
                            },
                            reply: tx,
                        },
                    })
                    .await;
                match rx.await {
                    Ok(Reply::AttachmentData(data)) => {
                        let filename = format!("attachment-{part_key}");
                        let w2 = w.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w2.upgrade() {
                                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                                let path = std::path::PathBuf::from(&home)
                                    .join("Downloads")
                                    .join(&filename);
                                if let Err(e) = std::fs::write(&path, &data) {
                                    show_toast(&app, &format!("Save failed: {e}"), "error");
                                } else {
                                    show_toast(
                                        &app,
                                        &format!("Saved to {}", path.display()),
                                        "success",
                                    );
                                }
                            }
                        })
                        .ok();
                    }
                    Ok(Reply::Err(e)) => {
                        let msg = e.user_message();
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, &msg, "error");
                            }
                        })
                        .ok();
                    }
                    _ => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(app) = w.upgrade() {
                                show_toast(&app, "Failed to save attachment", "error");
                            }
                        })
                        .ok();
                    }
                }
            });
        });
    });
}

// The whole body is `tray`-gated, so the params are dead without it.
#[cfg_attr(not(feature = "tray"), allow(unused_variables))]
fn wire_file_drop(state: &GuiState, app: &crate::AppWindow) {
    #[cfg(feature = "tray")]
    {
        use kestrel_core::protocol::DraftAttachment;
        let draft = Arc::clone(&state.draft);
        app.on_file_dropped({
            let w = app.as_weak();
            move |path_str| {
                let path = std::path::PathBuf::from(path_str.to_string());
                let file_name = path
                    .file_name()
                    .map_or_else(|| "attachment".into(), |n| n.to_string_lossy().into_owned());
                let mime = mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .to_string();
                match std::fs::read(&path) {
                    Ok(data) => {
                        let attachment = DraftAttachment {
                            name: file_name.clone(),
                            mime_type: mime,
                            data,
                        };
                        draft.attach(attachment);
                        if let Some(app) = w.upgrade() {
                            let mut names: String = app.get_compose_attachment_names().to_string();
                            if !names.is_empty() {
                                names.push_str(", ");
                            }
                            names.push_str(&file_name);
                            app.set_compose_attachment_names(names.into());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to read dropped file {path_str}: {e}");
                        if let Some(app) = w.upgrade() {
                            app.set_compose_error(
                                format!("failed to read {file_name}: {e}").into(),
                            );
                        }
                    }
                }
            }
        });

        // DropApi global: wire can-drop / transfer-to-string callbacks
        {
            use slint::private_unstable_api::re_exports as sp;

            let drop_api = app.global::<kestrel_gui::DropApi<'_>>();
            drop_api.on_can_drop(|data: sp::DataTransfer| -> sp::DragAction {
                if data.has_plain_text() {
                    sp::DragAction::Copy
                } else {
                    sp::DragAction::None
                }
            });
            drop_api.on_transfer_to_string(|data: sp::DataTransfer| -> sp::SharedString {
                let text = data.plain_text().unwrap_or_default();
                let uri = text.lines().next().unwrap_or("");
                let path = uri.strip_prefix("file://").unwrap_or(uri);
                sp::SharedString::from(path)
            });
        }
    }
}

fn wire_html_view(state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    let vp = Arc::clone(&state.vp_state);
    app.on_open_html_view(move || {
        if let Some(app) = w.upgrade() {
            let body = app.get_preview_body();
            if !body.is_empty() {
                let parts = {
                    let state = vp.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.parts_for_display()
                };
                if let Err(e) = kestrel_gui::viewport::spawn_wry_viewport(&body, parts) {
                    tracing::warn!("html viewport: {e}");
                }
            }
        }
    });
}

fn wire_remote_content(state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    let message_view = Arc::clone(&state.message_view);
    app.on_toggle_remote_content(move || {
        let Some(app) = w.upgrade() else { return };
        let Some(html) = message_view.html() else {
            return;
        };
        let sanitized = kestrel_core::sanitizer::sanitize_html_body_with_remote(&html, true);
        let wrapped = kestrel_gui::viewport::wrap_html_with_csp(&sanitized.html);
        app.set_preview_body(slint::SharedString::from(wrapped.as_str()));
        app.set_show_remote_content(true);
        app.set_remote_blocked_count(0);
        app.set_status_text("Remote content loaded".into());
    });
}

fn wire_find_bar(_state: &GuiState, app: &crate::AppWindow) {
    let w = app.as_weak();
    app.on_toggle_find_bar(move || {
        if let Some(app) = w.upgrade() {
            let visible = app.get_show_find_bar();
            app.set_show_find_bar(!visible);
            if visible {
                app.set_find_query(slint::SharedString::default());
                app.set_find_results(0);
            }
        }
    });

    let w = app.as_weak();
    app.on_find_in_message(move |query| {
        let Some(app) = w.upgrade() else { return };
        let query_str = query.to_string();
        if query_str.is_empty() {
            app.set_find_results(0);
            return;
        }
        let body = app.get_preview_body().to_string();
        let query_lower = query_str.to_lowercase();
        let body_lower = body.to_lowercase();
        let count = body_lower.matches(&query_lower).count();
        app.set_find_results(i32::try_from(count).unwrap_or(i32::MAX));
    });
}
