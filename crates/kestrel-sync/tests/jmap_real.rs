//! Real-world Fastmail integration test (JMAP).
//!
//! Every test is `#[ignore]`d and gated by the `KESTREL_JMAP_INTEGRATION=1`
//! env var. Credentials are loaded from env vars — **never** committed.
//!
//! Required env vars:
//!   - `KESTREL_JMAP_INTEGRATION=1`
//!   - `KESTREL_JMAP_API_TOKEN`
//!   - `KESTREL_JMAP_HOST` (optional, defaults to `api.fastmail.com`)

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    clippy::print_stderr
)]

use std::time::Duration;

use kestrel_core::secrets::SecretString;
use kestrel_sync::jmap::JmapClient;

fn jmap_integration_enabled() -> bool {
    std::env::var("KESTREL_JMAP_INTEGRATION").is_ok_and(|v| v == "1")
}

fn env_or_skip(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("skipping: {name} not set");
        std::process::exit(0);
    })
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client")
}

#[tokio::test]
#[ignore = "real: KESTREL_JMAP_INTEGRATION=1 + env vars required"]
async fn jmap_real_full_flow() {
    if !jmap_integration_enabled() {
        eprintln!("skipping: KESTREL_JMAP_INTEGRATION not set");
        return;
    }

    let _ = tracing_subscriber::fmt::try_init();

    let api_token = env_or_skip("KESTREL_JMAP_API_TOKEN");
    let host = std::env::var("KESTREL_JMAP_HOST").unwrap_or_else(|_| "api.fastmail.com".into());

    let http = http_client();
    let token = SecretString::new(api_token);

    // 1. Discover session via .well-known/jmap.
    let client = JmapClient::discover(http, &host, &token)
        .await
        .expect("JMAP session discovery failed");

    // 2. Mailbox/get → verify at least one folder exists.
    {
        let resp = client.get_mailboxes().await.expect("Mailbox/get failed");
        let method_responses = &resp.method_responses;
        assert!(
            !method_responses.is_empty(),
            "Mailbox/get returned no method responses"
        );

        // Parse the first response to find the mailbox list.
        let first = &method_responses[0];
        let list = first
            .get(1)
            .and_then(|v| v.get("list"))
            .and_then(|v| v.as_array());
        assert!(list.is_some(), "Mailbox/get response missing 'list'");
        let folders = list.unwrap();
        assert!(
            !folders.is_empty(),
            "Mailbox/get returned empty folder list"
        );
        eprintln!("jmap_real: found {} folders", folders.len());
    }

    // 3. Email/query → fetch 5 messages.
    {
        let resp = client
            .query_emails(vec![], None, 5)
            .await
            .expect("Email/query+get failed");
        let method_responses = &resp.method_responses;
        assert!(
            !method_responses.is_empty(),
            "Email/query returned no method responses"
        );

        // The response should have at least two method responses:
        // one for Email/query ("q1") and one for Email/get ("g1").
        assert!(
            method_responses.len() >= 2,
            "Expected at least 2 method responses (query + get), got {}",
            method_responses.len()
        );

        // 4. Verify message structure.
        // The Email/get response should contain a list of messages with ids.
        let get_response = &method_responses[1];
        let list = get_response
            .get(1)
            .and_then(|v| v.get("list"))
            .and_then(|v| v.as_array());
        if let Some(messages) = list {
            eprintln!("jmap_real: fetched {} messages", messages.len());
            for msg in messages {
                // Each message should have an "id" field.
                assert!(
                    msg.get("id").is_some(),
                    "Email/get message missing 'id' field"
                );
            }
        } else {
            eprintln!("jmap_real: no messages found (empty mailbox)");
        }
    }
}

#[tokio::test]
#[ignore = "real: KESTREL_JMAP_INTEGRATION=1 + env vars required"]
async fn jmap_real_session_discovery_parses_correctly() {
    if !jmap_integration_enabled() {
        eprintln!("skipping: KESTREL_JMAP_INTEGRATION not set");
        return;
    }

    let api_token = env_or_skip("KESTREL_JMAP_API_TOKEN");
    let host = std::env::var("KESTREL_JMAP_HOST").unwrap_or_else(|_| "api.fastmail.com".into());

    let http = http_client();
    let token = SecretString::new(api_token);

    let client = JmapClient::discover(http, &host, &token)
        .await
        .expect("session discovery");

    // A second call should work (idempotent).
    let resp = client
        .get_mailboxes()
        .await
        .expect("Mailbox/get after discovery");
    assert!(!resp.method_responses.is_empty());
}
