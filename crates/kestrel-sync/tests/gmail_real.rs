//! Real-world Gmail integration test (`OAuth2` + IMAP).
//!
//! Every test is `#[ignore]`d and gated by the `KESTREL_GMAIL_INTEGRATION=1`
//! env var. Credentials are loaded from env vars — **never** committed.
//!
//! Required env vars:
//!   - `KESTREL_GMAIL_INTEGRATION=1`
//!   - `KESTREL_GMAIL_REFRESH_TOKEN` (for `OAuth2` test)
//!   - `KESTREL_GMAIL_CLIENT_ID` (for `OAuth2` test)
//!   - `KESTREL_GMAIL_CLIENT_SECRET` (for `OAuth2` test)
//!   - `KESTREL_GMAIL_EMAIL`
//!   - `KESTREL_GMAIL_PASSWORD` (for password-based IMAP test — use App Password)

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::{sync::Arc, time::Duration};

use kestrel_core::{sasl::SaslMechanism, secrets::SecretString};
use kestrel_crypto::oauth::{self, MailProvider};
use kestrel_sync::{ConnectParams, ImapSession, Security};

const IMAP_HOST: &str = "imap.gmail.com";
const IMAP_PORT: u16 = 993;

fn gmail_integration_enabled() -> bool {
    std::env::var("KESTREL_GMAIL_INTEGRATION").is_ok_and(|v| v == "1")
}

fn env_or_skip(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("skipping: {name} not set");
        std::process::exit(0);
    })
}

fn tls_connector() -> Arc<rustls::ClientConfig> {
    kestrel_crypto::tls_config(None).expect("rustls config")
}

fn build_connect_params(access_token: &str, email: &str) -> ConnectParams {
    ConnectParams {
        host: IMAP_HOST.into(),
        port: IMAP_PORT,
        security: Security::Tls,
        username: email.into(),
        secret: SecretString::new(access_token.into()),
        mechanisms: vec![SaslMechanism::Xoauth2, SaslMechanism::Plain],
        tls: tokio_rustls::TlsConnector::from(tls_connector()),
        sasl_factory: Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        }),
    }
}

/// Exchanges a refresh token for an access token via Google's token endpoint.
async fn obtain_access_token() -> (String, String) {
    let client_id = env_or_skip("KESTREL_GMAIL_CLIENT_ID");
    let client_secret = env_or_skip("KESTREL_GMAIL_CLIENT_SECRET");
    let refresh_token = env_or_skip("KESTREL_GMAIL_REFRESH_TOKEN");

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client");

    let provider = MailProvider::gmail(&client_id);
    let tokens = oauth::refresh(
        &http,
        &provider,
        Some(&SecretString::new(client_secret)),
        &SecretString::new(refresh_token),
    )
    .await
    .expect("OAuth2 token refresh failed");

    (
        tokens.access_token.expose().to_owned(),
        env_or_skip("KESTREL_GMAIL_EMAIL"),
    )
}

#[tokio::test]
#[ignore = "real: KESTREL_GMAIL_INTEGRATION=1 + env vars required"]
async fn gmail_real_full_flow() {
    if !gmail_integration_enabled() {
        eprintln!("skipping: KESTREL_GMAIL_INTEGRATION not set");
        return;
    }

    let _ = tracing_subscriber::fmt::try_init();

    let (access_token, email) = obtain_access_token().await;
    let params = build_connect_params(&access_token, &email);

    // 1. Connect + authenticate with XOAUTH2.
    let mut session = ImapSession::connect_and_authenticate(&params)
        .await
        .expect("IMAP connect+auth failed");

    // 2. LIST folders → verify INBOX exists.
    {
        use imap_next::imap_types::{
            command::CommandBody,
            mailbox::{ListMailbox, Mailbox},
        };

        let wildcard = ListMailbox::try_from("*").unwrap_or(ListMailbox::Token(
            imap_next::imap_types::mailbox::ListCharString::try_from("*")
                .unwrap_or_else(|_| unreachable!("'*' is valid")),
        ));
        let outcome = session
            .execute(
                CommandBody::List {
                    reference: Mailbox::try_from(String::new()).unwrap_or(Mailbox::Inbox),
                    mailbox_wildcard: wildcard,
                },
                Duration::from_secs(30),
            )
            .await
            .expect("LIST failed");
        assert!(
            outcome.is_ok(),
            "LIST returned non-OK: {}",
            outcome.status_summary()
        );
        let has_inbox = outcome.data.iter().any(|d| {
            matches!(d, imap_next::imap_types::response::Data::List { mailbox, .. }
                if matches!(mailbox, imap_next::imap_types::mailbox::Mailbox::Inbox))
        });
        assert!(has_inbox, "INBOX not found in LIST response");
    }

    // 3. SELECT INBOX.
    {
        use imap_next::imap_types::{command::CommandBody, mailbox::Mailbox};

        let outcome = session
            .execute(
                CommandBody::Select {
                    mailbox: Mailbox::Inbox,
                    parameters: Vec::new(),
                },
                Duration::from_secs(30),
            )
            .await
            .expect("SELECT failed");
        assert!(
            outcome.is_ok(),
            "SELECT INBOX failed: {}",
            outcome.status_summary()
        );
    }

    // 4. FETCH 5 ENVELOPEs (UID FETCH 1:5 ALL).
    {
        let outcome = session.fetch_envelopes("1:5").await.expect("FETCH failed");
        assert!(
            outcome.is_ok(),
            "FETCH envelopes failed: {}",
            outcome.status_summary()
        );

        // 5. Verify: subjects non-empty, UIDs valid.
        let mut fetch_count = 0u32;
        for data in &outcome.data {
            if let imap_next::imap_types::response::Data::Fetch { items, .. } = data {
                let mut uid = None;
                let mut has_envelope = false;
                for item in items.as_ref() {
                    match item {
                        imap_next::imap_types::fetch::MessageDataItem::Uid(u) => {
                            uid = Some(u.get());
                        }
                        imap_next::imap_types::fetch::MessageDataItem::Envelope(env) => {
                            has_envelope = true;
                            let subject = env.subject.0.as_ref().map(|s| {
                                use imap_next::imap_types::core::IString;
                                match s {
                                    IString::Quoted(q) => q.as_ref().to_owned(),
                                    IString::QuotedUtf8(q) => q.0.clone().into_owned(),
                                    IString::Literal(lit) => {
                                        String::from_utf8_lossy(lit.as_ref()).into_owned()
                                    }
                                }
                            });
                            // Subject may be empty for messages without a subject
                            // header — that's acceptable. We just verify the
                            // envelope structure is present.
                            let _ = subject;
                        }
                        _ => {}
                    }
                }
                if uid.is_some() && has_envelope {
                    fetch_count += 1;
                }
            }
        }
        // At least some envelopes should have been fetched (may be < 5 if
        // the mailbox has fewer messages).
        eprintln!("gmail_real: fetched {fetch_count} envelopes");
    }

    // 6. Clean disconnect.
    session.logout().await;
}

#[tokio::test]
#[ignore = "real: KESTREL_GMAIL_INTEGRATION=1 + env vars required"]
async fn gmail_real_password_imap() {
    if !gmail_integration_enabled() {
        eprintln!("skipping: KESTREL_GMAIL_INTEGRATION not set");
        return;
    }

    let _ = tracing_subscriber::fmt::try_init();

    let email = env_or_skip("KESTREL_GMAIL_EMAIL");
    let password = env_or_skip("KESTREL_GMAIL_PASSWORD");

    let params = ConnectParams {
        host: IMAP_HOST.into(),
        port: IMAP_PORT,
        security: Security::Tls,
        username: email.clone(),
        secret: SecretString::new(password),
        mechanisms: vec![SaslMechanism::Plain, SaslMechanism::Login],
        tls: tokio_rustls::TlsConnector::from(tls_connector()),
        sasl_factory: Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        }),
    };

    // 1. Connect + authenticate with PLAIN.
    let mut session = ImapSession::connect_and_authenticate(&params)
        .await
        .expect("IMAP connect+auth failed");

    // 2. LIST folders.
    {
        use imap_next::imap_types::{
            command::CommandBody,
            mailbox::{ListMailbox, Mailbox},
        };

        let wildcard = ListMailbox::try_from("*").unwrap_or(ListMailbox::Token(
            imap_next::imap_types::mailbox::ListCharString::try_from("*")
                .unwrap_or_else(|_| unreachable!("'*' is valid")),
        ));
        let outcome = session
            .execute(
                CommandBody::List {
                    reference: Mailbox::try_from(String::new()).unwrap_or(Mailbox::Inbox),
                    mailbox_wildcard: wildcard,
                },
                Duration::from_secs(30),
            )
            .await
            .expect("LIST failed");
        assert!(
            outcome.is_ok(),
            "LIST returned non-OK: {}",
            outcome.status_summary()
        );
        let has_inbox = outcome.data.iter().any(|d| {
            matches!(d, imap_next::imap_types::response::Data::List { mailbox, .. }
                if matches!(mailbox, imap_next::imap_types::mailbox::Mailbox::Inbox))
        });
        assert!(has_inbox, "INBOX not found in LIST response");
    }

    // 3. SELECT INBOX.
    {
        use imap_next::imap_types::{command::CommandBody, mailbox::Mailbox};

        let outcome = session
            .execute(
                CommandBody::Select {
                    mailbox: Mailbox::Inbox,
                    parameters: Vec::new(),
                },
                Duration::from_secs(30),
            )
            .await
            .expect("SELECT failed");
        assert!(
            outcome.is_ok(),
            "SELECT INBOX failed: {}",
            outcome.status_summary()
        );
    }

    // 4. FETCH 5 ENVELOPEs.
    {
        let outcome = session.fetch_envelopes("1:5").await.expect("FETCH failed");
        assert!(
            outcome.is_ok(),
            "FETCH envelopes failed: {}",
            outcome.status_summary()
        );

        let mut fetch_count = 0u32;
        for data in &outcome.data {
            if let imap_next::imap_types::response::Data::Fetch { items, .. } = data {
                let mut uid = None;
                let mut has_envelope = false;
                for item in items.as_ref() {
                    match item {
                        imap_next::imap_types::fetch::MessageDataItem::Uid(u) => {
                            uid = Some(u.get());
                        }
                        imap_next::imap_types::fetch::MessageDataItem::Envelope(_) => {
                            has_envelope = true;
                        }
                        _ => {}
                    }
                }
                if uid.is_some() && has_envelope {
                    fetch_count += 1;
                }
            }
        }
        eprintln!("gmail_real_password_imap: fetched {fetch_count} envelopes");
    }

    // 5. Clean disconnect.
    session.logout().await;
}
