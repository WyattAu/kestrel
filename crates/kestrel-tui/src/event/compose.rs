#![allow(clippy::wildcard_imports)] // sibling-module glue (issue #4)
//! Composition: reply / forward / new, `$EDITOR` launch, MIME building.

use std::{fmt::Write as _, sync::Arc};

use kestrel_core::protocol::{Command, CommandPayload, FrontendKind, Reply};
use kestrel_engine::EngineHandle;

use crate::{
    app::AppState,
    editor,
    event::{fmt::*, next_request_id},
};

pub(crate) async fn compose_reply(
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
    match rx.await {
        Ok(Reply::Accepted) => state.status = "reply queued".into(),
        other => state.status = format!("reply submit failed: {other:?}"),
    }
}

pub(crate) async fn compose_forward(
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
pub(crate) fn build_forward_eml(msg: &kestrel_core::protocol::MessageView) -> Vec<u8> {
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

pub(crate) async fn compose_new(
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

pub(crate) fn run_editor(
    template: &str,
    config: &Arc<kestrel_core::config::Config>,
) -> std::io::Result<editor::EditorOutcome> {
    // The suspend/resume cycle toggles raw mode + alternate screen on the
    // real stdout; under a test harness (no TTY, e.g. the daily-loop gate
    // driving a TestBackend) those ioctls fail, and a spawned editor child
    // doesn't need the terminal suspended anyway. Skip suspension there.
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() {
        return editor::edit_draft(template, config.editor.command.as_deref());
    }
    // Suspend → edit → resume (message-protocol §6).
    editor::suspend_terminal()?;
    let result = editor::edit_draft(template, config.editor.command.as_deref());
    editor::resume_terminal()?;
    result
}
