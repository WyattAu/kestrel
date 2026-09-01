//! $EDITOR composition (requirements §5): suspend the terminal (raw mode
//! off), spawn the editor with a Markdown draft, resume on exit, return
//! the edited body.

use std::{fmt::Write as _, path::PathBuf, process::Stdio};

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Extra RFC 822 headers to include in the draft template.
pub struct DraftHeaders<'a> {
    /// `In-Reply-To` value.
    pub in_reply_to: Option<&'a str>,
    /// `References` value.
    pub references: Option<&'a str>,
}

/// Draft template written to the temp file.
#[must_use]
pub fn draft_template(subject: &str, to: &str) -> String {
    draft_template_with_headers(subject, to, None)
}

/// Draft template with optional extra headers and quoted original body.
#[must_use]
pub fn draft_template_with_headers(
    subject: &str,
    to: &str,
    extra: Option<&DraftHeaders<'_>>,
) -> String {
    let mut s = format!("# {subject}\n\nTo: {to}\n");
    if let Some(h) = extra {
        if let Some(irt) = h.in_reply_to {
            let _ = writeln!(s, "In-Reply-To: <{irt}>");
        }
        if let Some(refs) = h.references {
            let _ = writeln!(s, "References: {refs}");
        }
    }
    s.push_str("\n---\n\nWrite your reply below this line in Markdown.\n");
    s
}

/// Builds a reply template with quoted original message.
#[must_use]
pub fn reply_template(
    subject: &str,
    to: &str,
    in_reply_to: Option<&str>,
    references: &[String],
    original_body: &str,
) -> String {
    let refs_str = if references.is_empty() {
        String::new()
    } else {
        references.join(" ")
    };
    let mut template = draft_template_with_headers(
        subject,
        to,
        Some(&DraftHeaders {
            in_reply_to,
            references: if refs_str.is_empty() {
                None
            } else {
                Some(&refs_str)
            },
        }),
    );
    // Trim quotes before appending.
    let trimmed = trim_quote(original_body);
    // Append quoted original.
    if !trimmed.trim().is_empty() {
        let quoted: String = trimmed
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        template.push('\n');
        template.push_str(&quoted);
        template.push('\n');
    }
    template
}

/// Trims common signature patterns, legal disclaimers, and nested quoted
/// text from an email body before quoting it in a reply.
#[must_use]
pub fn trim_quote(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut trimmed_until = lines.len();

    // Two-pass scan from the bottom:
    // Pass 1: find the lowest line that matches a signature/disclaimer pattern.
    // Pass 2: include any trailing blank lines above it in the trim.
    let mut sig_start = None;
    for i in (0..lines.len()).rev() {
        let lower = lines[i].trim().to_lowercase();

        if is_signature_line(&lower) || is_disclaimer_line(&lower) {
            sig_start = Some(i);
            break;
        }
    }

    if let Some(start) = sig_start {
        trimmed_until = start;
        // Also strip blank lines immediately above the signature block.
        while trimmed_until > 0 && lines[trimmed_until - 1].trim().is_empty() {
            trimmed_until -= 1;
        }
    }

    let trimmed = lines[..trimmed_until].join("\n");

    // Strip nested quoted text that repeats the same pattern 2+ times.
    // E.g. ">>>\n>>>\n>>> text" — collapse to just the innermost content.
    strip_nested_quotes(&trimmed)
}

fn is_signature_line(lower: &str) -> bool {
    lower == "-- "
        || lower == "--"
        || lower.starts_with("best regards")
        || lower.starts_with("kind regards")
        || lower.starts_with("regards,")
        || lower.starts_with("sincerely,")
        || lower.starts_with("cheers,")
        || lower.starts_with("sent from my")
        || lower.starts_with("get outlook for")
        || lower.starts_with("sent from my iphone")
        || lower.starts_with("sent from my android")
        || lower.starts_with("on behalf of")
        || lower.starts_with("via ")
}

fn is_disclaimer_line(lower: &str) -> bool {
    lower.contains("confidentiality notice:")
        || lower.contains("confidentiality and privilege")
        || lower.contains("disclaimer:")
        || lower.contains("this message is intended only for")
        || lower.contains("this email is confidential")
        || lower.contains("the information contained in this")
        || lower.contains("if you are not the intended recipient")
        || lower.contains("unauthorized use, disclosure, copying")
}

/// Strips nested quote markers (>>>, >>, >) when they appear in repeated
/// blocks, keeping only the deepest unique level.
fn strip_nested_quotes(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut skip_until_quote_depth: Option<usize> = None;

    for line in &lines {
        let depth = count_quote_prefix(line);

        if let Some(skip_depth) = skip_until_quote_depth {
            if depth >= skip_depth {
                // Still in a repeated quote block — skip.
                continue;
            }
            // Depth decreased — reset.
            skip_until_quote_depth = None;
        }

        // Detect runs of 3+ consecutive lines with the same quote depth >= 2.
        if depth >= 2 && result.len() >= 2 {
            let prev1_depth = count_quote_prefix(result[result.len() - 1]);
            let prev2_depth = count_quote_prefix(result[result.len() - 2]);
            if prev1_depth == depth && prev2_depth == depth {
                // Start skipping the outer layer; keep inner content.
                skip_until_quote_depth = Some(depth);
                continue;
            }
        }

        result.push(*line);
    }

    result.join("\n")
}

/// Counts leading `>` quote markers in a line.
fn count_quote_prefix(line: &str) -> usize {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let mut count = 0;
    for c in trimmed.chars() {
        if c == '>' {
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// Result of an editor session.
pub struct EditorOutcome {
    /// The edited Markdown body (below the `---` separator).
    pub body_markdown: String,
    /// Subject extracted from the `# heading` line.
    pub subject: String,
}

/// Spawns $EDITOR (or $VISUAL, or config override) on a temp file.
/// The terminal must be suspended (`suspend_terminal`) before calling
/// and resumed after.
///
/// # Errors
/// IO failures or a missing editor binary.
pub fn edit_draft(initial: &str, editor_cmd: Option<&str>) -> std::io::Result<EditorOutcome> {
    let editor = editor_cmd
        .map(str::to_owned)
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".to_string());

    let dir = tempfile::tempdir()?;
    let path: PathBuf = dir.path().join("kestrel-draft.md");
    std::fs::write(&path, initial)?;

    let words: Vec<String> = shell_words::split(&editor)
        .map_err(|e| std::io::Error::other(format!("editor command split: {e}")))?;
    if words.is_empty() {
        return Err(std::io::Error::other("empty editor command"));
    }

    let mut cmd = std::process::Command::new(&words[0]);
    cmd.args(&words[1..])
        .arg(&path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = cmd.status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "editor exited with {status}"
        )));
    }

    let content = std::fs::read_to_string(&path)?;
    parse_draft(&content)
}

/// Parses the edited draft: `# subject` + `---` + body.
///
/// # Errors
/// Never fails in practice (the Result shape keeps the editor pipeline
/// uniform); body/subject default to empty strings on unparseable input.
pub fn parse_draft(content: &str) -> std::io::Result<EditorOutcome> {
    let mut subject = String::new();
    let mut body = String::new();
    let mut past_separator = false;
    let mut in_header = true;

    for line in content.lines() {
        if in_header && line.starts_with("# ") {
            line[2..].trim().clone_into(&mut subject);
            in_header = false;
            continue;
        }
        if line.trim() == "---" && !past_separator {
            past_separator = true;
            continue;
        }
        if past_separator {
            body.push_str(line);
            body.push('\n');
        }
    }
    if body.is_empty() && subject.is_empty() {
        // The user may have deleted the template; treat the whole file
        // as the body.
        content.clone_into(&mut body);
    }
    // Strip a leading blank line from the body.
    let body = body.trim_start_matches('\n').to_string();
    Ok(EditorOutcome {
        body_markdown: body,
        subject,
    })
}

/// Suspends the terminal for an external process: leaves alternate screen,
/// disables raw mode.
///
/// # Errors
/// Terminal control failures.
pub fn suspend_terminal() -> std::io::Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    Ok(())
}

/// Resumes the terminal after an external process.
///
/// # Errors
/// Terminal control failures.
pub fn resume_terminal() -> std::io::Result<()> {
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    enable_raw_mode()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn draft_template_shape() {
        let t = draft_template("Hello", "bob@x.example");
        assert!(t.starts_with("# Hello"));
        assert!(t.contains("To: bob@x.example"));
        assert!(t.contains("---"));
    }

    #[test]
    fn draft_template_with_headers_includes_reply_fields() {
        let t = draft_template_with_headers(
            "Re: Hi",
            "bob@x.example",
            Some(&DraftHeaders {
                in_reply_to: Some("msg123@example"),
                references: Some("<msg123@example> <msg100@example>"),
            }),
        );
        assert!(t.contains("In-Reply-To: <msg123@example>"));
        assert!(t.contains("References: <msg123@example> <msg100@example>"));
    }

    #[test]
    fn reply_template_quotes_original() {
        let t = reply_template(
            "Re: Hi",
            "bob@x.example",
            Some("msg123@example"),
            &["msg123@example".into()],
            "Hello world\nSecond line",
        );
        assert!(t.contains("> Hello world"));
        assert!(t.contains("> Second line"));
        assert!(t.contains("In-Reply-To:"));
    }

    #[test]
    fn parse_extracts_subject_and_body() {
        let content = "# Re: Hello\n\nTo: bob@x\n\n---\n\nThis is **my** reply.\n";
        let out = parse_draft(content).unwrap();
        assert_eq!(out.subject, "Re: Hello");
        assert_eq!(out.body_markdown.trim(), "This is **my** reply.");
    }

    #[test]
    fn parse_empty_template_falls_back_to_full_body() {
        let out = parse_draft("just a body line\n").unwrap();
        assert!(out.body_markdown.contains("just a body line"));
    }

    #[test]
    fn edit_draft_via_cat_editor() {
        // `cat` as editor: reads stdin (empty → template unchanged).
        let initial = "# Test\n\nTo: x@y\n\n---\n\nbody here\n";
        let out = edit_draft(initial, Some("true")).unwrap();
        assert_eq!(out.subject, "Test");
        assert!(out.body_markdown.contains("body here"));
    }

    #[test]
    fn trim_quote_strips_signature() {
        let body = "Thanks for the info.\n\n-- \nBest regards,\nAlice";
        let trimmed = trim_quote(body);
        assert!(trimmed.contains("Thanks for the info."));
        assert!(!trimmed.contains("Best regards"));
    }

    #[test]
    fn trim_quote_strips_disclaimer() {
        let body =
            "Hello\n\nCONFIDENTIALITY NOTICE: This message is intended only for the recipient.";
        let trimmed = trim_quote(body);
        assert!(trimmed.contains("Hello"));
        assert!(!trimmed.contains("CONFIDENTIALITY"));
    }

    #[test]
    fn trim_quote_strips_sent_from_my() {
        let body = "Sure thing.\n\nSent from my iPhone";
        let trimmed = trim_quote(body);
        assert!(trimmed.contains("Sure thing."));
        assert!(!trimmed.contains("Sent from my"));
    }

    #[test]
    fn trim_quote_strips_empty_line_above_signature() {
        let body = "Body text.\n\n\n-- \nSignature";
        let trimmed = trim_quote(body);
        assert!(trimmed.contains("Body text."));
        assert!(!trimmed.contains("Signature"));
    }

    #[test]
    fn trim_quote_preserves_normal_body() {
        let body = "This is a normal email body with no signature or disclaimer.";
        let trimmed = trim_quote(body);
        assert_eq!(trimmed, body);
    }

    #[test]
    fn trim_quote_strips_nested_quotes() {
        let body = ">>>\n>>>\n>>> original\nactual reply";
        let trimmed = trim_quote(body);
        assert!(trimmed.contains("actual reply"));
    }

    #[test]
    fn reply_template_uses_trimmed_body() {
        let body = "Thanks!\n\n-- \nSent from my iPhone";
        let t = reply_template(
            "Re: Hi",
            "bob@x.example",
            Some("msg123@example"),
            &["msg123@example".into()],
            body,
        );
        // The quoted body should not contain the signature.
        assert!(t.contains("> Thanks!"));
        assert!(!t.contains("> Sent from my iPhone"));
    }
}
