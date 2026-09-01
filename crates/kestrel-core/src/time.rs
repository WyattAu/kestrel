/// Civil-time formatting utilities for unix-ms timestamps.
///
/// Pure arithmetic with no external dependencies; used by both TUI and GUI
/// frontends to avoid duplicating date math.
/// Formats a unix-ms timestamp into a short date string: "DD Mon YY".
#[must_use]
pub fn format_datetime(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hours, mins, _secs) = (
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

    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let _ = (hours, mins);
    format!(
        "{:02} {} {:02}",
        d,
        month_names[usize::try_from(month - 1).unwrap_or(0) % 12],
        year % 100
    )
}

/// Formats a unix-ms timestamp into a date+time string: "DD Mon YY HH:MM".
#[must_use]
pub fn format_datetime_full(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hours, mins) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);
    format!("{} {:02}:{:02}", format_datetime(unix_ms), hours, mins)
}

use crate::clock::Clock as _;

/// Returns the number of whole days from a unix-ms timestamp to now.
/// Positive means the timestamp is in the past.
#[must_use]
pub fn days_ago(unix_ms: i64) -> i64 {
    let now_ms = crate::clock::SystemClock.now_unix_ms();
    let diff_secs = (now_ms - unix_ms).div_euclid(1000);
    diff_secs.div_euclid(86_400)
}

/// Classifies a message timestamp into a date-group label for the UI.
/// Returns `None` when the message is recent enough to omit the header.
#[must_use]
pub fn date_group(unix_ms: i64) -> Option<&'static str> {
    let ago = days_ago(unix_ms);
    if ago <= 0 {
        Some("Today")
    } else if ago == 1 {
        Some("Yesterday")
    } else if ago <= 7 {
        Some("Last 7 days")
    } else {
        Some("Older")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn format_datetime_returns_short_date() {
        // 2024-01-15 17:50:00 UTC
        let result = format_datetime(1_705_312_200_000);
        assert_eq!(result, "15 Jan 24");
    }

    #[test]
    fn format_datetime_full_includes_time() {
        // 2024-01-15 12:30:00 UTC = 1705321800000
        let result = format_datetime_full(1_705_321_800_000);
        assert_eq!(result, "15 Jan 24 12:30");
    }

    #[test]
    fn format_datetime_epoch() {
        let result = format_datetime(0);
        assert_eq!(result, "01 Jan 70");
    }
}
