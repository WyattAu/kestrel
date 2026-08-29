//! Provider presets (requirements §2.3): auto-detect server settings from
//! the email domain, with manual override for generic IMAP/SMTP.

use crate::protocol::Provider;

/// Full server configuration for one account.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountConfig {
    /// Display name shown in the UI.
    pub display_name: String,
    /// Email address (also the IMAP/SMTP username by default).
    pub email: String,
    /// Provider family (drives preset detection).
    pub provider: Provider,
    /// Auth kind: "password" or "oauth2".
    pub auth_kind: String,
    /// IMAP host.
    pub imap_host: String,
    /// IMAP port.
    pub imap_port: u16,
    /// IMAP security: "tls" (993) or "starttls" (143).
    pub imap_security: String,
    /// SMTP host.
    pub smtp_host: String,
    /// SMTP port.
    pub smtp_port: u16,
    /// SMTP security: "tls" (465) or "starttls" (587).
    pub smtp_security: String,
    /// Override username (defaults to email).
    pub username: Option<String>,
}

/// Detects the provider from an email domain.
#[must_use]
pub fn detect_provider(email: &str) -> Provider {
    let domain = email.rsplit('@').next().unwrap_or_default().to_lowercase();
    match domain.as_str() {
        "gmail.com" | "googlemail.com" => Provider::Gmail,
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" | "passport.com" => {
            Provider::Outlook
        }
        "fastmail.com" | "fastmail.fm" => Provider::Fastmail,
        _ => Provider::Generic,
    }
}

/// Returns the default server settings for a provider.
#[must_use]
pub fn provider_preset(provider: &Provider, email: &str) -> AccountConfig {
    let display_name = email.split('@').next().unwrap_or("User").to_title_case();
    let base = AccountConfig {
        display_name,
        email: email.to_owned(),
        provider: provider.clone(),
        auth_kind: "password".into(),
        imap_host: String::new(),
        imap_port: 993,
        imap_security: "tls".into(),
        smtp_host: String::new(),
        smtp_port: 587,
        smtp_security: "starttls".into(),
        username: None,
    };
    match provider {
        Provider::Gmail => AccountConfig {
            imap_host: "imap.gmail.com".into(),
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Outlook => AccountConfig {
            imap_host: "outlook.office365.com".into(),
            smtp_host: "smtp.office365.com".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            ..base
        },
        Provider::Fastmail => AccountConfig {
            imap_host: "imap.fastmail.com".into(),
            smtp_host: "smtp.fastmail.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        _ => base,
    }
}

/// Validates the config before submission (fail fast, requirements §7).
#[must_use]
pub fn validate_account_config(config: &AccountConfig) -> Vec<String> {
    let mut errors = Vec::new();
    if config.email.trim().is_empty() || !config.email.contains('@') {
        errors.push("email must be a valid address".into());
    }
    if config.imap_host.trim().is_empty() {
        errors.push("IMAP host is required".into());
    }
    if config.smtp_host.trim().is_empty() {
        errors.push("SMTP host is required".into());
    }
    if config.imap_port == 0 {
        errors.push("IMAP port must be > 0".into());
    }
    if config.smtp_port == 0 {
        errors.push("SMTP port must be > 0".into());
    }
    if !matches!(config.imap_security.as_str(), "tls" | "starttls") {
        errors.push("IMAP security must be 'tls' or 'starttls'".into());
    }
    if !matches!(config.smtp_security.as_str(), "tls" | "starttls") {
        errors.push("SMTP security must be 'tls' or 'starttls'".into());
    }
    if !matches!(config.auth_kind.as_str(), "password" | "oauth2") {
        errors.push("auth kind must be 'password' or 'oauth2'".into());
    }
    errors
}

/// Title-case helper for display names.
trait TitleCase {
    fn to_title_case(&self) -> String;
}

impl TitleCase for str {
    fn to_title_case(&self) -> String {
        self.split(['.', '_', '-', ' '])
            .filter(|s| !s.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn gmail_detected() {
        assert_eq!(detect_provider("user@gmail.com"), Provider::Gmail);
        assert_eq!(detect_provider("user@googlemail.com"), Provider::Gmail);
    }

    #[test]
    fn outlook_detected() {
        assert_eq!(detect_provider("x@outlook.com"), Provider::Outlook);
        assert_eq!(detect_provider("x@hotmail.com"), Provider::Outlook);
    }

    #[test]
    fn fastmail_detected() {
        assert_eq!(detect_provider("x@fastmail.com"), Provider::Fastmail);
    }

    #[test]
    fn generic_fallback() {
        assert_eq!(detect_provider("x@example.org"), Provider::Generic);
        assert_eq!(detect_provider("not-an-email"), Provider::Generic);
    }

    #[test]
    fn gmail_preset_correct() {
        let c = provider_preset(&Provider::Gmail, "john.doe@gmail.com");
        assert_eq!(c.imap_host, "imap.gmail.com");
        assert_eq!(c.imap_port, 993);
        assert_eq!(c.smtp_host, "smtp.gmail.com");
        assert_eq!(c.smtp_port, 465);
        assert_eq!(c.display_name, "John Doe");
    }

    #[test]
    fn validation_catches_errors() {
        let mut c = provider_preset(&Provider::Gmail, "x@gmail.com");
        assert!(validate_account_config(&c).is_empty());
        c.imap_host.clear();
        assert!(
            validate_account_config(&c)
                .iter()
                .any(|e| e.contains("IMAP host"))
        );
        c.email = "bad".into();
        assert!(
            validate_account_config(&c)
                .iter()
                .any(|e| e.contains("email"))
        );
    }
}
