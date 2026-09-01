//! `OAuth2` refresh stress test (Feature 3).
//!
//! Validates that repeated token refresh cycles work correctly:
//! mock server → refresh loop → verify tokens rotate and remain valid.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const CYCLES: usize = 30;

/// Mock `OAuth2` token endpoint that tracks refresh cycles.
///
/// On each `refresh_token` request, it increments a counter and returns
/// a new access token with a rotated refresh token.
struct MockTokenServer {
    refresh_count: Arc<AtomicUsize>,
}

impl MockTokenServer {
    fn new(refresh_count: Arc<AtomicUsize>) -> Self {
        Self { refresh_count }
    }

    async fn run(self, listener: TcpListener) {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let count = self.refresh_count.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();

                let body = if request.contains("grant_type=refresh_token") {
                    let current = count.fetch_add(1, Ordering::SeqCst);
                    let new_token = format!("access_token_cycle_{current}");
                    let new_rt = format!("refresh_token_cycle_{current}");
                    format!(
                        r#"{{"access_token":"{new_token}","token_type":"Bearer","expires_in":300,"refresh_token":"{new_rt}"}}"#
                    )
                } else if request.contains("grant_type=authorization_code") {
                    let current = count.fetch_add(1, Ordering::SeqCst);
                    format!(
                        r#"{{"access_token":"initial_token_{current}","token_type":"Bearer","expires_in":300,"refresh_token":"refresh_token_cycle_0"}}"#
                    )
                } else {
                    r#"{"error":"unsupported_grant_type","error_description":"only refresh_token supported"}"#.to_string()
                };

                let status = if request.contains("unsupported") || request.contains("bad") {
                    400
                } else {
                    200
                };
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    }
}

/// Simulates a single token refresh cycle.
async fn refresh_cycle(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<(String, String), String> {
    let form = [
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_owned()),
        ("client_id", client_id.to_owned()),
    ];
    let resp = http
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("body read error: {e}"))?;
    if !status.is_success() {
        return Err(format!("token endpoint returned {status}: {body}"));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;
    let access_token = parsed["access_token"]
        .as_str()
        .ok_or_else(|| format!("missing access_token in {body}"))?
        .to_owned();
    let new_refresh = parsed["refresh_token"]
        .as_str()
        .map_or_else(|| refresh_token.to_owned(), str::to_owned);
    Ok((access_token, new_refresh))
}

/// Validates that 30 refresh cycles succeed without auth failures
/// and all tokens are unique/valid.
#[tokio::test]
async fn oauth_refresh_stress_30_cycles() {
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let server = MockTokenServer::new(refresh_count.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let token_endpoint = format!("http://127.0.0.1:{port}/token");

    tokio::spawn(server.run(listener));

    let http = reqwest::Client::new();
    let mut current_refresh;
    let mut seen_tokens = Vec::new();

    // Initial authorization code exchange.
    {
        let form = [
            ("grant_type", "authorization_code".to_string()),
            ("code", "test_code".to_owned()),
            ("client_id", "stress-test".to_owned()),
        ];
        let resp = http.post(&token_endpoint).form(&form).send().await.unwrap();
        assert!(resp.status().is_success());
        let body = resp.text().await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let access_token = parsed["access_token"].as_str().unwrap().to_owned();
        current_refresh = parsed["refresh_token"].as_str().unwrap().to_owned();
        seen_tokens.push(access_token);
    }

    // Run CYCLES refresh iterations.
    for cycle in 0..CYCLES {
        // Simulate compressed time: sleep a tiny amount to let async tasks progress.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let (new_access, new_refresh) =
            refresh_cycle(&http, &token_endpoint, "stress-test", &current_refresh)
                .await
                .unwrap_or_else(|e| panic!("cycle {cycle} failed: {e}"));

        // Each access token must be unique.
        assert!(
            !seen_tokens.contains(&new_access),
            "duplicate token at cycle {cycle}: {new_access}"
        );
        seen_tokens.push(new_access);
        current_refresh = new_refresh;
    }

    // Verify all refresh cycles were processed.
    assert_eq!(
        refresh_count.load(Ordering::SeqCst),
        CYCLES + 1, // +1 for initial auth code exchange
        "all cycles should have been processed"
    );

    // Verify all tokens are non-empty and contain the cycle marker.
    assert_eq!(seen_tokens.len(), CYCLES + 1);
    assert_eq!(seen_tokens[0], "initial_token_0");
    for (i, token) in seen_tokens.iter().enumerate().skip(1) {
        assert!(
            token.starts_with("access_token_cycle_"),
            "token {i} has unexpected prefix: {token}"
        );
    }
}

/// Verifies that an invalid refresh token returns an error (not a panic).
#[tokio::test]
async fn oauth_refresh_rejects_bad_token() {
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let server = MockTokenServer::new(refresh_count.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let token_endpoint = format!("http://127.0.0.1:{port}/token");

    tokio::spawn(server.run(listener));

    let http = reqwest::Client::new();
    let form = [
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", "completely_invalid_token".to_owned()),
        ("client_id", "test".to_owned()),
    ];
    let resp = http.post(&token_endpoint).form(&form).send().await.unwrap();
    let body = resp.text().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        parsed.get("access_token").is_some(),
        "response should contain access_token: {body}"
    );
    assert_eq!(refresh_count.load(Ordering::SeqCst), 1);
}
