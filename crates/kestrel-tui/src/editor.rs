//! $EDITOR composition (requirements §5): suspend the terminal (raw mode
//! off), spawn the editor with a Markdown draft, resume on exit, return
//! the edited body.

use std::{io::Write as _, path::PathBuf, process::Stdio};

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Draft template written to the temp file.
#[must_use]
pub fn draft_template(subject: &str, to: &str) -> String {
    format!("# {subject}\n\nTo: {to}\n\n---\n\nWrite your reply below this line in Markdown.\n")
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
    let _ = std::io::stdout().flush();
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
}
