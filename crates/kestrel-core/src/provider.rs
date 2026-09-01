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
        "yahoo.com" | "yahoo.co.uk" | "yahoo.ca" | "yahoo.com.au" | "yahoo.co.in"
        | "yahoo.co.jp" => Provider::Yahoo,
        "aol.com" | "aim.com" => Provider::Aol,
        "icloud.com" | "me.com" | "mac.com" => Provider::Icloud,
        "zoho.com" | "zohomail.com" | "zoho.eu" => Provider::Zoho,
        "gmx.com" | "gmx.de" | "gmx.net" | "gmx.at" | "gmx.ch" => Provider::Gmx,
        "web.de" => Provider::Webde,
        "mail.ru" | "inbox.ru" | "list.ru" | "bk.ru" => Provider::Mailru,
        "yandex.ru" | "yandex.com" | "ya.ru" | "yandex.ua" | "yandex.by" | "yandex.kz" => {
            Provider::Yandex
        }
        "comcast.net" => Provider::Comcast,
        "att.net" | "sbcglobal.net" | "bellsouth.net" => Provider::Att,
        "verizon.net" | "verizon.com" => Provider::Verizon,
        "t-online.de" => Provider::Tonline,
        "1and1.com" | "1und1.de" | "ionos.com" => Provider::Ionos,
        "rackspace.com" => Provider::Rackspace,
        "mailbox.org" => Provider::Mailbox,
        _ => Provider::Generic,
    }
}

/// Returns the default server settings for a provider.
#[must_use]
#[allow(clippy::too_many_lines)]
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
        Provider::Yahoo => AccountConfig {
            imap_host: "imap.mail.yahoo.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.mail.yahoo.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Aol => AccountConfig {
            imap_host: "imap.aol.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.aol.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Icloud => AccountConfig {
            imap_host: "imap.mail.me.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.mail.me.com".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            ..base
        },
        Provider::Proton => AccountConfig {
            imap_host: "127.0.0.1".into(),
            imap_port: 1143,
            imap_security: "starttls".into(),
            smtp_host: "127.0.0.1".into(),
            smtp_port: 1025,
            smtp_security: "starttls".into(),
            ..base
        },
        Provider::Zoho => AccountConfig {
            imap_host: "imap.zoho.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.zoho.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Gmx => AccountConfig {
            imap_host: "imap.gmx.net".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.gmx.net".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Webde => AccountConfig {
            imap_host: "imap.web.de".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.web.de".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            ..base
        },
        Provider::Mailru => AccountConfig {
            imap_host: "imap.mail.ru".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.mail.ru".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Yandex => AccountConfig {
            imap_host: "imap.yandex.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.yandex.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Comcast => AccountConfig {
            imap_host: "imap.comcast.net".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.comcast.net".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            ..base
        },
        Provider::Att => AccountConfig {
            imap_host: "imap.mail.att.net".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.mail.att.net".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Verizon => AccountConfig {
            imap_host: "imap.verizon.net".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.verizon.net".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Tonline => AccountConfig {
            imap_host: "secureimap.t-online.de".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "securesmtp.t-online.de".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            ..base
        },
        Provider::Ionos => AccountConfig {
            imap_host: "imap.ionos.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.ionos.com".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            ..base
        },
        Provider::Rackspace => AccountConfig {
            imap_host: "secure.emailsrvr.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "secure.emailsrvr.com".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            ..base
        },
        Provider::Mailbox => AccountConfig {
            imap_host: "imap.mailbox.org".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.mailbox.org".into(),
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

/// Human-readable display name for a provider.
#[must_use]
pub fn provider_display_name(p: &Provider) -> &'static str {
    match p {
        Provider::Gmail => "Gmail",
        Provider::Outlook => "Outlook / Microsoft 365",
        Provider::Yahoo => "Yahoo Mail",
        Provider::Icloud => "iCloud Mail",
        Provider::Aol => "AOL Mail",
        Provider::Proton => "Proton Mail (Bridge required)",
        Provider::Fastmail => "Fastmail",
        Provider::Zoho => "Zoho Mail",
        Provider::Gmx => "GMX Mail",
        Provider::Webde => "Web.de",
        Provider::Mailru => "Mail.ru / VK Mail",
        Provider::Yandex => "Yandex Mail",
        Provider::Comcast => "Comcast Mail",
        Provider::Att => "AT&T Mail",
        Provider::Verizon => "Verizon Mail",
        Provider::Tonline => "T-Online Mail",
        Provider::Ionos => "IONOS / 1&1 Mail",
        Provider::Rackspace => "Rackspace Email",
        Provider::Mailbox => "Mailbox.org",
        Provider::Generic => "Custom IMAP",
        Provider::Jmap => "JMAP",
    }
}

/// Provider-specific help text for authentication or setup guidance.
/// Returns `None` for providers that need no special instructions.
#[must_use]
pub fn provider_help(p: &Provider) -> Option<&'static str> {
    match p {
        Provider::Icloud => Some(
            "iCloud requires an App-Specific Password. Generate one at appleid.apple.com > Sign-In and Security > App-Specific Passwords.",
        ),
        Provider::Yahoo => Some(
            "Yahoo requires either OAuth2 or an App Password. Generate an App Password at account.yahoo.com > Account Security > Generate app password.",
        ),
        Provider::Aol => Some(
            "AOL requires an App Password. Generate one at account.aol.com > Account Security > Generate app password.",
        ),
        Provider::Proton => Some(
            "Proton Mail requires the Proton Mail Bridge desktop app to be running. Download from proton.me/mail/bridge.",
        ),
        Provider::Gmx => {
            Some("GMX requires IMAP to be enabled in account settings (Settings > POP3 & IMAP).")
        }
        Provider::Webde => {
            Some("Web.de requires IMAP to be enabled in account settings (Settings > POP3 & IMAP).")
        }
        _ => None,
    }
}

/// Returns `true` when the provider supports `OAuth2` browser flow.
#[must_use]
pub fn provider_supports_oauth2(p: &Provider) -> bool {
    matches!(
        p,
        Provider::Gmail | Provider::Outlook | Provider::Yahoo | Provider::Fastmail
    )
}

/// Human-readable label for the `OAuth2` sign-in button.
#[must_use]
pub fn provider_oauth2_button_label(p: &Provider) -> &'static str {
    match p {
        Provider::Gmail => "Sign in with Google",
        Provider::Outlook => "Sign in with Microsoft",
        Provider::Yahoo => "Sign in with Yahoo",
        Provider::Fastmail => "Sign in with Fastmail",
        _ => "Sign in with provider",
    }
}

/// Result of auto-discovering IMAP/SMTP servers for a domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoDiscoverResult {
    /// IMAP hostname.
    pub imap_host: String,
    /// IMAP port (typically 993 for TLS).
    pub imap_port: u16,
    /// SMTP hostname.
    pub smtp_host: String,
    /// SMTP port (typically 465 for TLS or 587 for STARTTLS).
    pub smtp_port: u16,
}

/// Attempts to auto-discover IMAP/SMTP servers for a domain.
///
/// Tries common hostname patterns (`imap.<domain>`, `smtp.<domain>`) and
/// verifies connectivity via a short TCP probe. Returns `None` if no
/// combination of hosts responds within the timeout.
///
/// # Errors
/// Returns `None` on any connection failure (DNS, timeout, refused).
pub async fn auto_discover(domain: &str) -> Option<AutoDiscoverResult> {
    let candidates = [
        ("imap", 993, "smtp", 465),
        ("imap", 993, "smtp", 587),
        ("mail", 993, "mail", 465),
        ("mail", 993, "mail", 587),
    ];

    for (imap_prefix, imap_port, smtp_prefix, smtp_port) in candidates {
        let imap_host = format!("{imap_prefix}.{domain}");
        let smtp_host = format!("{smtp_prefix}.{domain}");

        let imap_addr = format!("{imap_host}:{imap_port}");
        let smtp_addr = format!("{smtp_host}:{smtp_port}");

        let imap_ok = tokio::net::TcpStream::connect(&imap_addr).await.is_ok();
        let smtp_ok = tokio::net::TcpStream::connect(&smtp_addr).await.is_ok();

        if imap_ok && smtp_ok {
            return Some(AutoDiscoverResult {
                imap_host,
                imap_port,
                smtp_host,
                smtp_port,
            });
        }
    }
    None
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

    #[test]
    fn yahoo_detected() {
        assert_eq!(detect_provider("user@yahoo.com"), Provider::Yahoo);
        assert_eq!(detect_provider("user@yahoo.co.uk"), Provider::Yahoo);
        assert_eq!(detect_provider("user@yahoo.ca"), Provider::Yahoo);
        assert_eq!(detect_provider("user@yahoo.com.au"), Provider::Yahoo);
        assert_eq!(detect_provider("user@yahoo.co.in"), Provider::Yahoo);
        assert_eq!(detect_provider("user@yahoo.co.jp"), Provider::Yahoo);
    }

    #[test]
    fn provider_display_names_are_nonempty() {
        // Exhaustive: every variant must have a non-empty display name.
        let all = [
            Provider::Gmail,
            Provider::Outlook,
            Provider::Yahoo,
            Provider::Icloud,
            Provider::Aol,
            Provider::Proton,
            Provider::Fastmail,
            Provider::Zoho,
            Provider::Gmx,
            Provider::Webde,
            Provider::Mailru,
            Provider::Yandex,
            Provider::Comcast,
            Provider::Att,
            Provider::Verizon,
            Provider::Tonline,
            Provider::Ionos,
            Provider::Rackspace,
            Provider::Mailbox,
            Provider::Generic,
            Provider::Jmap,
        ];
        for p in &all {
            let name = provider_display_name(p);
            assert!(!name.is_empty(), "display name must be non-empty for {p:?}");
        }
    }

    #[test]
    fn provider_help_covers_app_password_providers() {
        assert!(provider_help(&Provider::Icloud).is_some());
        assert!(provider_help(&Provider::Yahoo).is_some());
        assert!(provider_help(&Provider::Aol).is_some());
        assert!(provider_help(&Provider::Proton).is_some());
        assert!(provider_help(&Provider::Gmx).is_some());
        assert!(provider_help(&Provider::Webde).is_some());
        // Providers with no special help.
        assert!(provider_help(&Provider::Gmail).is_none());
        assert!(provider_help(&Provider::Generic).is_none());
    }

    #[test]
    fn oauth2_supported_providers() {
        assert!(provider_supports_oauth2(&Provider::Gmail));
        assert!(provider_supports_oauth2(&Provider::Outlook));
        assert!(provider_supports_oauth2(&Provider::Yahoo));
        assert!(provider_supports_oauth2(&Provider::Fastmail));
        assert!(!provider_supports_oauth2(&Provider::Generic));
        assert!(!provider_supports_oauth2(&Provider::Icloud));
        assert!(!provider_supports_oauth2(&Provider::Aol));
        assert!(!provider_supports_oauth2(&Provider::Proton));
    }

    #[test]
    fn oauth2_button_labels_are_nonempty() {
        let oauth_providers = [
            Provider::Gmail,
            Provider::Outlook,
            Provider::Yahoo,
            Provider::Fastmail,
        ];
        for p in &oauth_providers {
            let label = provider_oauth2_button_label(p);
            assert!(
                !label.is_empty(),
                "button label must be non-empty for {p:?}"
            );
        }
    }

    #[test]
    fn aol_detected() {
        assert_eq!(detect_provider("user@aol.com"), Provider::Aol);
        assert_eq!(detect_provider("user@aim.com"), Provider::Aol);
    }

    #[test]
    fn icloud_detected() {
        assert_eq!(detect_provider("user@icloud.com"), Provider::Icloud);
        assert_eq!(detect_provider("user@me.com"), Provider::Icloud);
        assert_eq!(detect_provider("user@mac.com"), Provider::Icloud);
    }

    #[test]
    fn zoho_detected() {
        assert_eq!(detect_provider("user@zoho.com"), Provider::Zoho);
        assert_eq!(detect_provider("user@zohomail.com"), Provider::Zoho);
        assert_eq!(detect_provider("user@zoho.eu"), Provider::Zoho);
    }

    #[test]
    fn gmx_detected() {
        assert_eq!(detect_provider("user@gmx.com"), Provider::Gmx);
        assert_eq!(detect_provider("user@gmx.de"), Provider::Gmx);
        assert_eq!(detect_provider("user@gmx.net"), Provider::Gmx);
        assert_eq!(detect_provider("user@gmx.at"), Provider::Gmx);
        assert_eq!(detect_provider("user@gmx.ch"), Provider::Gmx);
    }

    #[test]
    fn webde_detected() {
        assert_eq!(detect_provider("user@web.de"), Provider::Webde);
    }

    #[test]
    fn mailru_detected() {
        assert_eq!(detect_provider("user@mail.ru"), Provider::Mailru);
        assert_eq!(detect_provider("user@inbox.ru"), Provider::Mailru);
        assert_eq!(detect_provider("user@list.ru"), Provider::Mailru);
        assert_eq!(detect_provider("user@bk.ru"), Provider::Mailru);
    }

    #[test]
    fn yandex_detected() {
        assert_eq!(detect_provider("user@yandex.ru"), Provider::Yandex);
        assert_eq!(detect_provider("user@yandex.com"), Provider::Yandex);
        assert_eq!(detect_provider("user@ya.ru"), Provider::Yandex);
        assert_eq!(detect_provider("user@yandex.ua"), Provider::Yandex);
        assert_eq!(detect_provider("user@yandex.by"), Provider::Yandex);
        assert_eq!(detect_provider("user@yandex.kz"), Provider::Yandex);
    }

    #[test]
    fn comcast_detected() {
        assert_eq!(detect_provider("user@comcast.net"), Provider::Comcast);
    }

    #[test]
    fn att_detected() {
        assert_eq!(detect_provider("user@att.net"), Provider::Att);
        assert_eq!(detect_provider("user@sbcglobal.net"), Provider::Att);
        assert_eq!(detect_provider("user@bellsouth.net"), Provider::Att);
    }

    #[test]
    fn verizon_detected() {
        assert_eq!(detect_provider("user@verizon.net"), Provider::Verizon);
        assert_eq!(detect_provider("user@verizon.com"), Provider::Verizon);
    }

    #[test]
    fn tonline_detected() {
        assert_eq!(detect_provider("user@t-online.de"), Provider::Tonline);
    }

    #[test]
    fn ionos_detected() {
        assert_eq!(detect_provider("user@1and1.com"), Provider::Ionos);
        assert_eq!(detect_provider("user@1und1.de"), Provider::Ionos);
        assert_eq!(detect_provider("user@ionos.com"), Provider::Ionos);
    }

    #[test]
    fn rackspace_detected() {
        assert_eq!(detect_provider("user@rackspace.com"), Provider::Rackspace);
    }

    #[test]
    fn mailbox_detected() {
        assert_eq!(detect_provider("user@mailbox.org"), Provider::Mailbox);
    }

    struct ExpectedPreset {
        imap_host: &'static str,
        imap_port: u16,
        imap_security: &'static str,
        smtp_host: &'static str,
        smtp_port: u16,
        smtp_security: &'static str,
    }

    fn assert_preset(provider: &Provider, email: &str, expected: &ExpectedPreset) {
        let c = provider_preset(provider, email);
        assert_eq!(
            c.imap_host, expected.imap_host,
            "IMAP host mismatch for {provider:?}"
        );
        assert_eq!(
            c.imap_port, expected.imap_port,
            "IMAP port mismatch for {provider:?}"
        );
        assert_eq!(
            c.imap_security, expected.imap_security,
            "IMAP security mismatch for {provider:?}"
        );
        assert_eq!(
            c.smtp_host, expected.smtp_host,
            "SMTP host mismatch for {provider:?}"
        );
        assert_eq!(
            c.smtp_port, expected.smtp_port,
            "SMTP port mismatch for {provider:?}"
        );
        assert_eq!(
            c.smtp_security, expected.smtp_security,
            "SMTP security mismatch for {provider:?}"
        );
        let errors = validate_account_config(&c);
        assert!(
            errors.is_empty(),
            "validation failed for {provider:?}: {errors:?}"
        );
    }

    #[test]
    fn yahoo_preset_correct() {
        assert_preset(
            &Provider::Yahoo,
            "user@yahoo.com",
            &ExpectedPreset {
                imap_host: "imap.mail.yahoo.com",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.mail.yahoo.com",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn aol_preset_correct() {
        assert_preset(
            &Provider::Aol,
            "user@aol.com",
            &ExpectedPreset {
                imap_host: "imap.aol.com",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.aol.com",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn icloud_preset_correct() {
        assert_preset(
            &Provider::Icloud,
            "user@icloud.com",
            &ExpectedPreset {
                imap_host: "imap.mail.me.com",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.mail.me.com",
                smtp_port: 587,
                smtp_security: "starttls",
            },
        );
    }

    #[test]
    fn proton_preset_correct() {
        assert_preset(
            &Provider::Proton,
            "user@proton.me",
            &ExpectedPreset {
                imap_host: "127.0.0.1",
                imap_port: 1143,
                imap_security: "starttls",
                smtp_host: "127.0.0.1",
                smtp_port: 1025,
                smtp_security: "starttls",
            },
        );
    }

    #[test]
    fn zoho_preset_correct() {
        assert_preset(
            &Provider::Zoho,
            "user@zoho.com",
            &ExpectedPreset {
                imap_host: "imap.zoho.com",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.zoho.com",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn gmx_preset_correct() {
        assert_preset(
            &Provider::Gmx,
            "user@gmx.com",
            &ExpectedPreset {
                imap_host: "imap.gmx.net",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.gmx.net",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn webde_preset_correct() {
        assert_preset(
            &Provider::Webde,
            "user@web.de",
            &ExpectedPreset {
                imap_host: "imap.web.de",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.web.de",
                smtp_port: 587,
                smtp_security: "starttls",
            },
        );
    }

    #[test]
    fn mailru_preset_correct() {
        assert_preset(
            &Provider::Mailru,
            "user@mail.ru",
            &ExpectedPreset {
                imap_host: "imap.mail.ru",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.mail.ru",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn yandex_preset_correct() {
        assert_preset(
            &Provider::Yandex,
            "user@yandex.com",
            &ExpectedPreset {
                imap_host: "imap.yandex.com",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.yandex.com",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn comcast_preset_correct() {
        assert_preset(
            &Provider::Comcast,
            "user@comcast.net",
            &ExpectedPreset {
                imap_host: "imap.comcast.net",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.comcast.net",
                smtp_port: 587,
                smtp_security: "starttls",
            },
        );
    }

    #[test]
    fn att_preset_correct() {
        assert_preset(
            &Provider::Att,
            "user@att.net",
            &ExpectedPreset {
                imap_host: "imap.mail.att.net",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.mail.att.net",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn verizon_preset_correct() {
        assert_preset(
            &Provider::Verizon,
            "user@verizon.net",
            &ExpectedPreset {
                imap_host: "imap.verizon.net",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.verizon.net",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn tonline_preset_correct() {
        assert_preset(
            &Provider::Tonline,
            "user@t-online.de",
            &ExpectedPreset {
                imap_host: "secureimap.t-online.de",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "securesmtp.t-online.de",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn ionos_preset_correct() {
        assert_preset(
            &Provider::Ionos,
            "user@ionos.com",
            &ExpectedPreset {
                imap_host: "imap.ionos.com",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.ionos.com",
                smtp_port: 587,
                smtp_security: "starttls",
            },
        );
    }

    #[test]
    fn rackspace_preset_correct() {
        assert_preset(
            &Provider::Rackspace,
            "user@rackspace.com",
            &ExpectedPreset {
                imap_host: "secure.emailsrvr.com",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "secure.emailsrvr.com",
                smtp_port: 587,
                smtp_security: "starttls",
            },
        );
    }

    #[test]
    fn mailbox_preset_correct() {
        assert_preset(
            &Provider::Mailbox,
            "user@mailbox.org",
            &ExpectedPreset {
                imap_host: "imap.mailbox.org",
                imap_port: 993,
                imap_security: "tls",
                smtp_host: "smtp.mailbox.org",
                smtp_port: 465,
                smtp_security: "tls",
            },
        );
    }

    #[test]
    fn auto_discover_result_fields() {
        let result = AutoDiscoverResult {
            imap_host: "imap.example.com".into(),
            imap_port: 993,
            smtp_host: "smtp.example.com".into(),
            smtp_port: 465,
        };
        assert_eq!(result.imap_host, "imap.example.com");
        assert_eq!(result.imap_port, 993);
        assert_eq!(result.smtp_host, "smtp.example.com");
        assert_eq!(result.smtp_port, 465);
    }

    #[tokio::test]
    async fn auto_discover_nonexistent_domain_returns_none() {
        let result = auto_discover("this-domain-definitely-does-not-exist-xyzzy.invalid").await;
        assert!(result.is_none());
    }
}
