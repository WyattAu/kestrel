//! Generic provider integration test.
//!
//! Every test is `#[ignore]`d and gated by `KESTREL_PROVIDER_INTEGRATION=1`.
//! This single test file can validate ANY provider by just changing env vars.
//! No need for separate test files per provider.
//!
//! Env vars:
//!   - `KESTREL_PROVIDER_INTEGRATION=1`
//!   - `KESTREL_PROVIDER_NAME` (e.g., "yahoo", "icloud", "zoho")
//!   - `KESTREL_PROVIDER_IMAP_HOST` (e.g., "imap.mail.yahoo.com")
//!   - `KESTREL_PROVIDER_IMAP_PORT` (default: 993)
//!   - `KESTREL_PROVIDER_EMAIL`
//!   - `KESTREL_PROVIDER_PASSWORD`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::{sync::Arc, time::Duration};

use kestrel_core::{sasl::SaslMechanism, secrets::SecretString};
use kestrel_sync::{ConnectParams, ImapSession, Security};

fn integration_enabled() -> bool {
    std::env::var("KESTREL_PROVIDER_INTEGRATION").is_ok_and(|v| v == "1")
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

fn build_connect_params(host: &str, port: u16, email: &str, password: &str) -> ConnectParams {
    ConnectParams {
        host: host.into(),
        port,
        security: Security::Tls,
        username: email.into(),
        secret: SecretString::new(password.into()),
        mechanisms: vec![
            SaslMechanism::Plain,
            SaslMechanism::Login,
            SaslMechanism::ScramSha256,
        ],
        tls: tokio_rustls::TlsConnector::from(tls_connector()),
        sasl_factory: Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        }),
    }
}

#[tokio::test]
#[ignore = "real: KESTREL_PROVIDER_INTEGRATION=1 + env vars required"]
async fn provider_real_password_auth() {
    if !integration_enabled() {
        eprintln!("skipping: KESTREL_PROVIDER_INTEGRATION not set");
        return;
    }

    let _ = tracing_subscriber::fmt::try_init();

    let provider_name = std::env::var("KESTREL_PROVIDER_NAME").unwrap_or_else(|_| "generic".into());
    let host = env_or_skip("KESTREL_PROVIDER_IMAP_HOST");
    let port: u16 = std::env::var("KESTREL_PROVIDER_IMAP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(993);
    let email = env_or_skip("KESTREL_PROVIDER_EMAIL");
    let password = env_or_skip("KESTREL_PROVIDER_PASSWORD");

    eprintln!("provider_real: [{provider_name}] connecting to {host}:{port} as {email}");

    let params = build_connect_params(&host, port, &email, &password);

    // 1. Connect + authenticate.
    let mut session = ImapSession::connect_and_authenticate(&params)
        .await
        .expect("IMAP connect+auth failed");

    eprintln!("provider_real: [{provider_name}] connected and authenticated");

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

        let folder_count = outcome.data.len();
        eprintln!("provider_real: [{provider_name}] found {folder_count} folders");

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
        eprintln!("provider_real: [{provider_name}] INBOX selected");
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
        eprintln!("provider_real: [{provider_name}] fetched {fetch_count} envelopes");
        assert!(fetch_count > 0, "No envelopes fetched");
    }

    // 5. Clean disconnect.
    session.logout().await;
    eprintln!("provider_real: [{provider_name}] test passed");
}
