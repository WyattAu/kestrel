#![allow(clippy::wildcard_imports)] // sibling-module glue (issue #4)
//! Display helpers: addresses, dates, sort/field labels, preview height.

use crate::app::AppState;

pub(crate) fn format_tui_address(addr: &kestrel_core::protocol::Address) -> String {
    match &addr.name {
        Some(name) if !name.is_empty() => format!("{name} <{}>", addr.email),
        _ => addr.email.clone(),
    }
}

pub(crate) fn format_internal_date(unix_ms: i64) -> String {
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

/// Preview pane visible lines estimate for scroll bounds.
pub(crate) fn preview_visible_lines(_state: &AppState) -> usize {
    20
}

pub(crate) fn sort_dir_label(dir: kestrel_core::protocol::SortDir) -> &'static str {
    match dir {
        kestrel_core::protocol::SortDir::Asc => "asc",
        kestrel_core::protocol::SortDir::Desc => "desc",
    }
}

pub(crate) fn field_label(field: kestrel_core::protocol::SortField) -> &'static str {
    match field {
        kestrel_core::protocol::SortField::Date => "date",
        kestrel_core::protocol::SortField::Sender => "sender",
        kestrel_core::protocol::SortField::Subject => "subject",
        kestrel_core::protocol::SortField::Uid => "uid",
    }
}
