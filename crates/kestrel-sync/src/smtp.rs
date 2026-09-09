//! SMTP submission via `lettre` (requirements §2.1): rustls TLS (1.3
//! default/1.2 minimum), implicit TLS on 465 or STARTTLS on 587, AUTH via
//! XOAUTH2/PLAIN/LOGIN. Raw RFC 5322 submission uses `send_raw` with an
//! explicit envelope so Bcc recipients receive mail without headers.

use std::time::Duration;

use kestrel_core::{error::KestrelError, secrets::SecretString};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    address::{Address, Envelope},
    transport::smtp::authentication::{Credentials, Mechanism},
};
use tracing::instrument;

/// SMTP transport security.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpSecurity {
    /// Implicit TLS (port 465).
    ImplicitTls,
    /// STARTTLS upgrade (port 587).
    StartTls,
    /// Cleartext — **integration fixtures only** (threat model §7 notes
    /// the cleartext fixture posture; production config never selects
    /// this).
    Insecure,
}

/// SMTP submission parameters. TLS uses webpki roots (rustls inside
/// lettre).
#[derive(Clone)]
pub struct SmtpParams {
    /// Relay host.
    pub host: String,
    /// Port (465 implicit TLS / 587 STARTTLS / fixture port).
    pub port: u16,
    /// Username.
    pub username: String,
    /// Password or OAuth token.
    pub secret: SecretString,
    /// Optional live-token handoff shared with the IMAP connect params:
    /// the engine's `OAuth2` refresh worker publishes refreshed access
    /// tokens here so each submission authenticates with a fresh
    /// credential. `None` for password accounts.
    pub secret_override: Option<std::sync::Arc<std::sync::RwLock<Option<SecretString>>>>,
    /// Use XOAUTH2 (token) instead of PLAIN.
    pub oauth2: bool,
    /// Transport security mode.
    pub security: SmtpSecurity,
}

impl SmtpParams {
    /// Shares a live-token cell with (e.g.) the account's IMAP params so
    /// one refresh-worker update serves both protocols.
    #[must_use]
    pub fn with_secret_cell(
        mut self,
        cell: std::sync::Arc<std::sync::RwLock<Option<SecretString>>>,
    ) -> Self {
        self.secret_override = Some(cell);
        self
    }

    /// The credential to submit with right now: the live override when
    /// set (refresh worker), else the static secret.
    fn current_secret(&self) -> SecretString {
        match &self.secret_override {
            Some(cell) => cell
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .unwrap_or_else(|| self.secret.clone()),
            None => self.secret.clone(),
        }
    }
}

fn tls_err(host: &str) -> impl Fn(lettre::transport::smtp::Error) -> KestrelError + '_ {
    move |e| KestrelError::TlsHandshake {
        detail: format!("smtp relay {host}: {e}"),
    }
}

fn build_transport(
    params: &SmtpParams,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, KestrelError> {
    let builder = match params.security {
        SmtpSecurity::ImplicitTls => {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&params.host).map_err(tls_err(&params.host))
        }
        SmtpSecurity::StartTls => {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&params.host)
                .map_err(tls_err(&params.host))
        }
        SmtpSecurity::Insecure => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            &params.host,
        )),
    }?
    .port(params.port)
    .timeout(Some(Duration::from_mins(1)));

    let creds = Credentials::new(
        params.username.clone(),
        params.current_secret().expose().to_owned(),
    );
    let mechanism = if params.oauth2 {
        vec![Mechanism::Xoauth2]
    } else {
        vec![Mechanism::Plain, Mechanism::Login]
    };
    Ok(builder.credentials(creds).authentication(mechanism).build())
}

/// Submits raw RFC 5322 bytes with an explicit envelope: `envelope_from` is
/// the MAIL FROM, `recipients` are RCPT TO (headers travel unchanged in
/// `raw`, so Bcc works without leaking).
///
/// # Errors
/// Mapped SMTP failures: 4xx → `SmtpTransient`, 5xx → `MessageRejected`,
/// connection issues → `ConnectionLost`/`TlsHandshake`.
#[instrument(skip_all)]
pub async fn submit_envelope(
    params: &SmtpParams,
    envelope_from: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<(), KestrelError> {
    let transport = build_transport(params)?;
    let from: Address = envelope_from
        .parse()
        .map_err(|e| KestrelError::DraftInvalid {
            detail: format!("envelope from {envelope_from:?}: {e}"),
        })?;
    let to: Vec<Address> = recipients.iter().filter_map(|r| r.parse().ok()).collect();
    let envelope = Envelope::new(Some(from), to).map_err(|e| KestrelError::DraftInvalid {
        detail: format!("envelope: {e:?}"),
    })?;
    transport
        .send_raw(&envelope, raw)
        .await
        .map(|_| ())
        .map_err(|e| map_smtp_error(&e))
}

fn map_smtp_error(err: &lettre::transport::smtp::Error) -> KestrelError {
    if err.is_permanent() {
        KestrelError::MessageRejected {
            detail: err.to_string(),
        }
    } else if err.is_transient() {
        KestrelError::SmtpTransient {
            code: err
                .status()
                .map_or(450, |s| s.to_string().parse().unwrap_or(450)),
        }
    } else {
        KestrelError::ConnectionLost {
            detail: err.to_string(),
        }
    }
}
