//! Color theme system for the TUI.
//!
//! Each [`Theme`] provides hex color strings for every UI element. The
//! `hex_to_color` helper converts them to [`ratatui::style::Color`] at
//! render time.

use serde::{Deserialize, Serialize};

/// Color theme for the TUI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    /// Theme name
    pub name: String,
    /// Focus color (selected item)
    pub focus: String,
    /// Normal text color
    pub normal: String,
    /// Dim/muted text color
    pub dim: String,
    /// Accent color (links, highlights)
    pub accent: String,
    /// Error color
    pub error: String,
    /// Success color
    pub success: String,
    /// Warning color
    pub warning: String,
    /// Header/title color
    pub header: String,
    /// Attachment indicator color
    pub attachment: String,
    /// Unread indicator color
    pub unread: String,
    /// Read text color
    pub read: String,
    /// Preview link color
    pub preview_link: String,
    /// Border color
    pub border: String,
    /// Status bar background
    pub status_bg: String,
    /// Mode line background
    pub mode_line_bg: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self::catppuccin_mocha()
    }
}

impl Theme {
    /// Catppuccin Mocha — warm dark theme.
    #[must_use]
    pub fn catppuccin_mocha() -> Self {
        Self {
            name: "Catppuccin Mocha".into(),
            focus: "#89b4fa".into(),
            normal: "#cdd6f4".into(),
            dim: "#6c7086".into(),
            accent: "#89dceb".into(),
            error: "#f38ba8".into(),
            success: "#a6e3a1".into(),
            warning: "#f9e2af".into(),
            header: "#cba6f7".into(),
            attachment: "#fab387".into(),
            unread: "#f9e2af".into(),
            read: "#6c7086".into(),
            preview_link: "#89b4fa".into(),
            border: "#45475a".into(),
            status_bg: "#313244".into(),
            mode_line_bg: "#181825".into(),
        }
    }

    /// Catppuccin Latte — warm light theme.
    #[must_use]
    pub fn catppuccin_latte() -> Self {
        Self {
            name: "Catppuccin Latte".into(),
            focus: "#1e66f5".into(),
            normal: "#4c4f69".into(),
            dim: "#9ca0b0".into(),
            accent: "#179299".into(),
            error: "#d20f39".into(),
            success: "#40a02b".into(),
            warning: "#df8e1d".into(),
            header: "#8839ef".into(),
            attachment: "#fe640b".into(),
            unread: "#df8e1d".into(),
            read: "#9ca0b0".into(),
            preview_link: "#1e66f5".into(),
            border: "#ccd0da".into(),
            status_bg: "#eff1f5".into(),
            mode_line_bg: "#e6e9ef".into(),
        }
    }

    /// Dracula — purple-accented dark theme.
    #[must_use]
    pub fn dracula() -> Self {
        Self {
            name: "Dracula".into(),
            focus: "#bd93f9".into(),
            normal: "#f8f8f2".into(),
            dim: "#6272a4".into(),
            accent: "#8be9fd".into(),
            error: "#ff5555".into(),
            success: "#50fa7b".into(),
            warning: "#f1fa8c".into(),
            header: "#bd93f9".into(),
            attachment: "#ffb86c".into(),
            unread: "#f1fa8c".into(),
            read: "#6272a4".into(),
            preview_link: "#8be9fd".into(),
            border: "#44475a".into(),
            status_bg: "#282a36".into(),
            mode_line_bg: "#21222c".into(),
        }
    }

    /// Nord — arctic blue palette.
    #[must_use]
    pub fn nord() -> Self {
        Self {
            name: "Nord".into(),
            focus: "#88c0d0".into(),
            normal: "#d8dee9".into(),
            dim: "#4c566a".into(),
            accent: "#81a1c1".into(),
            error: "#bf616a".into(),
            success: "#a3be8c".into(),
            warning: "#ebcb8b".into(),
            header: "#b48ead".into(),
            attachment: "#d08770".into(),
            unread: "#ebcb8b".into(),
            read: "#4c566a".into(),
            preview_link: "#88c0d0".into(),
            border: "#3b4252".into(),
            status_bg: "#2e3440".into(),
            mode_line_bg: "#2e3440".into(),
        }
    }

    /// Gruvbox Dark — retro warm colors.
    #[must_use]
    pub fn gruvbox_dark() -> Self {
        Self {
            name: "Gruvbox Dark".into(),
            focus: "#fabd2f".into(),
            normal: "#ebdbb2".into(),
            dim: "#928374".into(),
            accent: "#83a598".into(),
            error: "#fb4934".into(),
            success: "#b8bb26".into(),
            warning: "#fabd2f".into(),
            header: "#d3869b".into(),
            attachment: "#fe8019".into(),
            unread: "#fabd2f".into(),
            read: "#928374".into(),
            preview_link: "#83a598".into(),
            border: "#504945".into(),
            status_bg: "#282828".into(),
            mode_line_bg: "#1d2021".into(),
        }
    }

    /// Solarized Dark — precision color scheme.
    #[must_use]
    pub fn solarized_dark() -> Self {
        Self {
            name: "Solarized Dark".into(),
            focus: "#268bd2".into(),
            normal: "#839496".into(),
            dim: "#586e75".into(),
            accent: "#2aa198".into(),
            error: "#dc322f".into(),
            success: "#859900".into(),
            warning: "#b58900".into(),
            header: "#6c71c4".into(),
            attachment: "#cb4b16".into(),
            unread: "#b58900".into(),
            read: "#586e75".into(),
            preview_link: "#268bd2".into(),
            border: "#073642".into(),
            status_bg: "#002b36".into(),
            mode_line_bg: "#073642".into(),
        }
    }

    /// One Dark — Atom's dark theme.
    #[must_use]
    pub fn one_dark() -> Self {
        Self {
            name: "One Dark".into(),
            focus: "#61afef".into(),
            normal: "#abb2bf".into(),
            dim: "#5c6370".into(),
            accent: "#56b6c2".into(),
            error: "#e06c75".into(),
            success: "#98c379".into(),
            warning: "#e5c07b".into(),
            header: "#c678dd".into(),
            attachment: "#d19a66".into(),
            unread: "#e5c07b".into(),
            read: "#5c6370".into(),
            preview_link: "#61afef".into(),
            border: "#3e4451".into(),
            status_bg: "#282c34".into(),
            mode_line_bg: "#21252b".into(),
        }
    }

    /// Monokai — vibrant dark theme.
    #[must_use]
    pub fn monokai() -> Self {
        Self {
            name: "Monokai".into(),
            focus: "#66d9ef".into(),
            normal: "#f8f8f2".into(),
            dim: "#75715e".into(),
            accent: "#a6e22e".into(),
            error: "#f92672".into(),
            success: "#a6e22e".into(),
            warning: "#e6db74".into(),
            header: "#ae81ff".into(),
            attachment: "#fd971f".into(),
            unread: "#e6db74".into(),
            read: "#75715e".into(),
            preview_link: "#66d9ef".into(),
            border: "#3e3d32".into(),
            status_bg: "#272822".into(),
            mode_line_bg: "#1e1f1c".into(),
        }
    }
}

/// Available themes.
#[must_use]
pub fn available_themes() -> Vec<Theme> {
    vec![
        Theme::catppuccin_mocha(),
        Theme::catppuccin_latte(),
        Theme::dracula(),
        Theme::nord(),
        Theme::gruvbox_dark(),
        Theme::solarized_dark(),
        Theme::one_dark(),
        Theme::monokai(),
    ]
}

/// Find a theme by name (case-insensitive). Returns `None` if not found.
#[must_use]
pub fn find_theme(name: &str) -> Option<Theme> {
    let lower = name.to_lowercase();
    available_themes()
        .into_iter()
        .find(|t| t.name.to_lowercase() == lower)
}

/// Parse a ratatui Color from a hex string like `#ff00aa`.
///
/// # Panics
/// Does not panic — invalid hex falls back to `Color::Gray`.
#[must_use]
pub fn hex_to_color(hex: &str) -> ratatui::style::Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        ratatui::style::Color::Rgb(r, g, b)
    } else {
        ratatui::style::Color::Gray
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_color_parses_valid() {
        let c = hex_to_color("#ff0000");
        assert_eq!(c, ratatui::style::Color::Rgb(255, 0, 0));
    }

    #[test]
    fn hex_to_color_handles_no_hash() {
        let c = hex_to_color("00ff00");
        assert_eq!(c, ratatui::style::Color::Rgb(0, 255, 0));
    }

    #[test]
    fn hex_to_color_invalid_falls_back() {
        let c = hex_to_color("zzz");
        assert_eq!(c, ratatui::style::Color::Gray);
    }

    #[test]
    fn find_theme_case_insensitive() {
        assert!(find_theme("DRACULA").is_some());
        assert!(find_theme("nord").is_some());
        assert!(find_theme("nonexistent").is_none());
    }

    #[test]
    fn all_themes_have_unique_names() {
        let themes = available_themes();
        let mut names: Vec<_> = themes.iter().map(|t| &t.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), themes.len());
    }

    #[test]
    fn default_is_catppuccin_mocha() {
        let t = Theme::default();
        assert_eq!(t.name, "Catppuccin Mocha");
    }

    #[test]
    fn themes_serialize_deserialize() {
        let t = Theme::dracula();
        let json = serde_json::to_string(&t).unwrap();
        let t2: Theme = serde_json::from_str(&json).unwrap();
        assert_eq!(t.name, t2.name);
        assert_eq!(t.focus, t2.focus);
    }
}
