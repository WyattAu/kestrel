//! Rendering (ratatui): 3-pane layout, focus highlighting, status bar,
//! OSC 8 hyperlink passthrough in the preview pane (requirements §5).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{AppState, Focus, Mode},
    html::{html_to_lines, osc8_link},
};

/// Draws the full UI.
pub fn draw(f: &mut Frame<'_>, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_panes(f, state, chunks[0]);
    draw_status_bar(f, state, chunks[1]);
    draw_mode_line(f, state, chunks[2]);
}

fn draw_panes(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    // Focus mode: when only one pane fits (low width), expand it.
    if area.width < 80 {
        match state.focus {
            Focus::Folders => draw_folder_pane(f, state, area),
            Focus::List => draw_list_pane(f, state, area),
            Focus::Preview => draw_preview_pane(f, state, area),
        }
        return;
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(40),
            Constraint::Percentage(40),
        ])
        .split(area);
    draw_folder_pane(f, state, columns[0]);
    draw_list_pane(f, state, columns[1]);
    draw_preview_pane(f, state, columns[2]);
}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_folder_pane(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    let focused = state.focus == Focus::Folders;
    let mut items: Vec<ListItem<'_>> = Vec::new();

    for (i, acc) in state.accounts.iter().enumerate() {
        let color = acc
            .color
            .as_deref()
            .and_then(parse_hex_color)
            .unwrap_or(match acc.state {
                kestrel_core::protocol::ConnectionState::Idle
                | kestrel_core::protocol::ConnectionState::Syncing => Color::Green,
                kestrel_core::protocol::ConnectionState::OfflineMode => Color::Yellow,
                _ => Color::Gray,
            });
        let indicator = if i == state.selected_account {
            Span::styled(" ◀", Style::default().fg(color))
        } else {
            Span::raw(String::new())
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::raw(acc.name.clone()),
            indicator,
        ])));
        if items.len() > state.selected_account + 1 {
            for folder in &state.folders {
                let unread = if folder.unread > 0 {
                    format!(" ({})", folder.unread)
                } else {
                    String::new()
                };
                let role_icon = match folder.role {
                    Some(
                        kestrel_core::protocol::FolderRole::Inbox
                        | kestrel_core::protocol::FolderRole::UnifiedInbox,
                    ) => "📥",
                    Some(kestrel_core::protocol::FolderRole::Sent) => "📤",
                    Some(kestrel_core::protocol::FolderRole::Drafts) => "📝",
                    Some(kestrel_core::protocol::FolderRole::Trash) => "🗑",
                    Some(kestrel_core::protocol::FolderRole::Archive) => "📦",
                    Some(kestrel_core::protocol::FolderRole::Junk) => "⚠",
                    None => "  ",
                };
                items.push(ListItem::new(Line::from(Span::raw(format!(
                    "  {role_icon} {}{unread}",
                    folder.remote_name
                )))));
            }
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Folders ")
        .border_style(focus_style(focused));
    let mut list_state = ListState::default();
    // Rough scroll: keep folder selection visible.
    list_state.select(Some(
        state.selected_folder.min(items.len().saturating_sub(1)),
    ));
    f.render_stateful_widget(
        List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        area,
        &mut list_state,
    );
}

fn draw_list_pane(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    let focused = state.focus == Focus::List;
    let mut items: Vec<ListItem<'_>> = Vec::new();
    let mut last_group: Option<&str> = None;
    for m in &state.page.items {
        // Insert date group header when the group changes.
        if let Some(group) = kestrel_core::time::date_group(m.internal_date)
            && last_group != Some(group)
        {
            last_group = Some(group);
            items.push(ListItem::new(Line::from(Span::styled(
                format!("  ── {group} ──"),
                Style::default().fg(Color::DarkGray),
            ))));
        }
        let flags = if m.is_read { " " } else { "●" };
        let star = if m.is_flagged { "★" } else { " " };
        let attach = if m.has_attachments { "📎" } else { " " };
        let from = m
            .from
            .first()
            .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
            .unwrap_or_default();
        let subject = m.subject.clone().unwrap_or_else(|| "(no subject)".into());
        let date = format_date(m.internal_date);
        items.push(ListItem::new(Line::from(vec![
            Span::styled(flags.to_string(), Style::default().fg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(star.to_string(), Style::default().fg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(attach.to_string(), Style::default()),
            Span::raw(" "),
            Span::styled(format!("{from:<20}"), Style::default().fg(Color::Blue)),
            Span::raw(" "),
            Span::styled(
                format!("{subject:<40}"),
                if m.is_read {
                    Style::default()
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                },
            ),
            Span::raw(" "),
            Span::styled(date, Style::default().fg(Color::DarkGray)),
        ])));
    }

    let total = state.page.total;
    let shown = state.page.items.len();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Messages ({shown}/{total}) "))
        .border_style(focus_style(focused));
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_message));
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(Color::DarkGray)),
        area,
        &mut list_state,
    );
}

fn draw_preview_pane(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    let focused = state.focus == Focus::Preview;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Preview ")
        .border_style(focus_style(focused));

    if let Some(view) = &state.preview {
        let mut lines: Vec<Line<'_>> = Vec::new();
        // Header.
        lines.push(Line::from(Span::styled(
            format!(
                "From: {}",
                view.summary
                    .from
                    .first()
                    .map(|a| { a.name.clone().unwrap_or_else(|| a.email.clone()) })
                    .unwrap_or_default()
            ),
            Style::default().fg(Color::Blue),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "Subject: {}",
                view.summary.subject.clone().unwrap_or_default()
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("Date: {}", format_date(view.summary.internal_date)),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));

        // Body: prefer plain, fallback to sanitized HTML → terminal text.
        let body = if let Some(plain) = &view.body_plain {
            let sanitized = kestrel_core::sanitizer::sanitize_terminal_text(plain);
            sanitized
                .lines()
                .map(|l| Line::from(l.to_owned()))
                .collect::<Vec<_>>()
        } else if let Some(html) = &view.body_html {
            let rendered = html_to_lines(html, area.width.saturating_sub(2) as usize);
            rendered
                .iter()
                .map(|rl| {
                    let base_style = if rl.is_quoted {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                    };
                    if rl.links.is_empty() {
                        Line::from(Span::styled(rl.text.clone(), base_style))
                    } else {
                        // OSC 8 hyperlinks inline with dim styling for quotes.
                        let mut spans: Vec<Span<'_>> = Vec::new();
                        let mut last_end = 0usize;
                        for (start, end, url) in &rl.links {
                            if *start > last_end {
                                spans.push(Span::styled(
                                    rl.text[last_end..*start].to_string(),
                                    base_style,
                                ));
                            }
                            let label = rl.text[*start..*end].to_string();
                            spans.push(Span::styled(osc8_link(url, &label), base_style));
                            last_end = *end;
                        }
                        if last_end < rl.text.len() {
                            spans.push(Span::styled(rl.text[last_end..].to_string(), base_style));
                        }
                        Line::from(spans)
                    }
                })
                .collect::<Vec<_>>()
        } else {
            vec![Line::from(Span::styled(
                "(no body — press Enter to fetch)",
                Style::default().fg(Color::DarkGray),
            ))]
        };
        lines.extend(body);

        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    } else {
        f.render_widget(
            Paragraph::new("(no message selected)")
                .block(block)
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }
}

fn draw_status_bar(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    let connection = state
        .account()
        .map_or_else(|| "no account".into(), |a| format!("{:?}", a.state));
    let text = format!(" {} │ {} ", state.status, connection);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().bg(Color::DarkGray).fg(Color::White),
        )))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_mode_line(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    let text = match state.mode {
        Mode::Setup => " Tab:next field │ Enter:connect │ Esc:cancel ",
        Mode::Normal => {
            " j/k:navigate │ Tab:focus │ d:delete │ s:star │ z:snooze │ /:search │ :command │ r:reply │ a:reply-all │ f:forward │ c:compose │ q:quit "
        }
        Mode::Search => " type query │ Enter:search │ Esc:cancel ",
        Mode::Confirm => " y:confirm │ n/Esc:cancel ",
        Mode::ConfirmDelete => " y:delete │ n/Esc:cancel ",
        Mode::ConfirmRemoveAccount => " y:remove account │ n/Esc:cancel ",
        Mode::Snooze => " j/k:select │ Enter:confirm │ Esc:cancel ",
        Mode::Command => " type command │ Enter:execute │ Esc:cancel ",
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

fn format_date(unix_ms: i64) -> String {
    // Compact date: MM-DD HH:MM (civil-time math, no chrono).
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let civil_days = days + 719_468;
    let era = civil_days.div_euclid(146_097);
    let day_of_era = civil_days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_phase = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_phase + 2) / 5 + 1;
    let month = if month_phase < 10 {
        month_phase + 3
    } else {
        month_phase - 9
    };
    let (hour, minute) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);
    let _ = year;
    format!("{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Draws a modal overlay (search box, confirm, setup).
pub fn draw_modal(f: &mut Frame<'_>, state: &AppState) {
    match state.mode {
        Mode::Setup => {
            let area = centered_rect(f.area(), 60, 7);
            f.render_widget(Clear, area);
            let fields = [
                ("Email: ", &state.setup_email),
                ("Pass:  ", &"*".repeat(state.setup_password.len())),
                ("IMAP:  ", &state.setup_imap_host),
            ];
            let mut lines = vec![Line::from(Span::styled(
                "  Account Setup  ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))];
            for (i, (label, value)) in fields.iter().enumerate() {
                let cursor = if i == state.setup_field { "█" } else { " " };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(*label, Style::default().fg(Color::Blue)),
                    Span::raw((*value).clone()),
                    Span::styled(cursor, Style::default().fg(Color::Yellow)),
                ]));
            }
            lines.push(Line::from(Span::styled(
                "  Tab:next Enter:connect Esc:cancel",
                Style::default().fg(Color::DarkGray),
            )));
            f.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
        Mode::Search => {
            let area = centered_rect(f.area(), 50, 3);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("/search: ", Style::default().fg(Color::Cyan)),
                    Span::raw(state.search_input.clone()),
                    Span::styled("_", Style::default().fg(Color::DarkGray)),
                ]))
                .block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
        Mode::Confirm => {
            let area = centered_rect(f.area(), 40, 3);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new("Confirm quit? (y/n)")
                    .block(Block::default().borders(Borders::ALL))
                    .style(Style::default().fg(Color::Yellow)),
                area,
            );
        }
        Mode::Normal => {}
        Mode::ConfirmDelete => {
            let area = centered_rect(f.area(), 50, 3);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    &state.status,
                    Style::default().fg(Color::Yellow),
                )))
                .block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
        Mode::ConfirmRemoveAccount => draw_confirm_remove_account_modal(f, state),
        Mode::Snooze => draw_snooze_modal(f, state),
        Mode::Command => {
            let area = centered_rect(f.area(), 60, 3);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(":", Style::default().fg(Color::Cyan)),
                    Span::raw(
                        state
                            .command_input
                            .strip_prefix(':')
                            .unwrap_or(&state.command_input)
                            .to_owned(),
                    ),
                    Span::styled("_", Style::default().fg(Color::DarkGray)),
                ]))
                .block(Block::default().borders(Borders::ALL).title(" Command ")),
                area,
            );
        }
    }
}

fn centered_rect(area: Rect, pct_width: u16, height: u16) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_width) / 2),
            Constraint::Percentage(pct_width),
            Constraint::Percentage((100 - pct_width) / 2),
        ])
        .split(popup[1])[1]
}

/// Renders the snooze options modal overlay.
fn draw_snooze_modal(f: &mut Frame<'_>, state: &AppState) {
    let area = centered_rect(f.area(), 50, 8);
    f.render_widget(Clear, area);

    let options = [
        "Tomorrow morning (9 AM)",
        "Next week",
        &format!("Custom ({} hours)_", state.snooze_hours),
    ];

    let mut lines: Vec<Line<'_>> = vec![Line::from(Span::styled(
        "  Snooze message  ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];

    for (i, opt) in options.iter().enumerate() {
        let selected = i == state.snooze_selection;
        let prefix = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::styled(opt.to_string(), style),
        ]));
    }

    lines.push(Line::from(Span::styled(
        "  j/k:select │ Enter:confirm │ Esc:cancel",
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_confirm_remove_account_modal(f: &mut Frame<'_>, state: &AppState) {
    let area = centered_rect(f.area(), 60, 3);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            &state.status,
            Style::default().fg(Color::Red),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Confirm Account Removal "),
        ),
        area,
    );
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}
