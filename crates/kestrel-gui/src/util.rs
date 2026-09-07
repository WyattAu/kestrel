//! Shared UI helpers used across GUI modules.

use std::sync::Arc;

use slint::ComponentHandle as _;

use crate::AppWindow;

/// Update the contact autocomplete suggestions shown in the compose UI.
pub fn update_contact_suggestions_gui(
    w: &slint::Weak<AppWindow>,
    cache: &Arc<std::sync::Mutex<Vec<kestrel_core::protocol::ContactSummary>>>,
    query: &str,
) {
    let q = query.to_string();
    let suggestions: Vec<slint::SharedString> = {
        let Ok(contacts) = cache.lock() else {
            return;
        };
        contacts
            .iter()
            .filter(|c| {
                let q_lower = q.to_lowercase();
                c.display_name.to_lowercase().contains(&q_lower)
                    || c.email.to_lowercase().contains(&q_lower)
            })
            .map(|c| {
                if c.display_name.is_empty() {
                    slint::SharedString::from(c.email.as_str())
                } else {
                    slint::SharedString::from(format!("{} <{}>", c.display_name, c.email))
                }
            })
            .collect()
    };
    let show = !suggestions.is_empty();
    if let Some(app) = w.upgrade() {
        app.set_contact_suggestions(suggestions.as_slice().into());
        app.set_show_contact_suggestions(show);
    }
}

/// Show a transient toast notification for 3 seconds.
pub fn show_toast(app: &AppWindow, message: &str, toast_type: &str) {
    app.set_show_toast(true);
    app.set_toast_message(slint::SharedString::from(message));
    app.set_toast_type(slint::SharedString::from(toast_type));
    let w = app.as_weak();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        slint::invoke_from_event_loop(move || {
            if let Some(app) = w.upgrade() {
                app.set_show_toast(false);
            }
        })
        .ok();
    });
}

/// Catppuccin-derived colour palette for account avatars.
const ACCOUNT_COLOR_PALETTE: [&str; 8] = [
    "#f38ba8", "#a6e3a1", "#89b4fa", "#f9e2af", "#cba6f7", "#94e2d5", "#fab387", "#74c7ec",
];

/// Convert a hex colour string (`"#aarrggbb"` or `"#rrggbb"`) to a Slint colour.
pub fn hex_to_slint_color(hex: &str) -> slint::Color {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    slint::Color::from_rgb_u8(r, g, b)
}

/// Return the palette colour for a given account index (wrapping).
pub fn account_color_for_index(idx: usize) -> &'static str {
    ACCOUNT_COLOR_PALETTE[idx % ACCOUNT_COLOR_PALETTE.len()]
}
