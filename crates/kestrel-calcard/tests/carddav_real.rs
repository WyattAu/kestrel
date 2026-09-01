//! Integration tests for the `CardDAV` client against real servers.
//!
//! These tests require a running Radicale (or compatible) CardDAV server.
//! Gate on the `KESTREL_INTEGRATION` env var:
//!
//! ```sh
//! KESTREL_INTEGRATION=1 \
//! CARDDAV_URL=http://localhost:5232/kestrel/default/ \
//! CARDDAV_USER=kestrel \
//! CARDDAV_PASS=testpass \
//! cargo nextest run --test carddav_real -p kestrel-calcard
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::doc_markdown
)]

use kestrel_calcard::CardDavClient;

fn integration_enabled() -> bool {
    std::env::var("KESTREL_INTEGRATION").ok().as_deref() == Some("1")
}

fn carddav_client() -> Option<CardDavClient> {
    if !integration_enabled() {
        eprintln!("KESTREL_INTEGRATION not set, skipping");
        return None;
    }
    let url = std::env::var("CARDDAV_URL").ok()?;
    let user = std::env::var("CARDDAV_USER").unwrap_or_else(|_| "kestrel".into());
    let pass = std::env::var("CARDDAV_PASS").unwrap_or_else(|_| "testpass".into());
    Some(CardDavClient::new_basic(url, user, pass))
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_list_address_books_round_trip() {
    let Some(client) = carddav_client() else {
        return;
    };

    let books = client
        .list_address_books()
        .await
        .expect("list_address_books should succeed");

    assert!(
        !books.is_empty(),
        "expected at least one address book from the server"
    );

    for book in &books {
        assert!(!book.href.is_empty(), "address book href must not be empty");
        assert!(
            !book.display_name.is_empty(),
            "address book display_name must not be empty"
        );
    }
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_sync_contacts_full_sync() {
    let Some(client) = carddav_client() else {
        return;
    };

    let books = client
        .list_address_books()
        .await
        .expect("list_address_books should succeed");

    let book = books
        .first()
        .expect("need at least one address book for sync test");

    let result = client
        .sync_contacts(&book.href, None)
        .await
        .expect("sync_contacts should succeed");

    assert!(
        result.new_sync_token.is_some(),
        "full sync should return a sync token"
    );

    for contact in &result.contacts {
        assert!(!contact.uid.is_empty(), "contact UID must not be empty");
    }
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_put_and_delete_contact() {
    let Some(client) = carddav_client() else {
        return;
    };

    let books = client
        .list_address_books()
        .await
        .expect("list_address_books should succeed");

    let book = books
        .first()
        .expect("need at least one address book for put/delete test");

    let uid = format!("test-contact-{}@kestrel", uuid::Uuid::now_v7());
    let vcard = format!(
        "BEGIN:VCARD\r\n\
         VERSION:4.0\r\n\
         UID:{uid}\r\n\
         FN:Test Contact\r\n\
         N:Contact;Test;;;\r\n\
         EMAIL:test@example.com\r\n\
         TEL:+1-555-0000\r\n\
         END:VCARD\r\n"
    );

    let contact_url = format!("{}{uid}.vcf", book.href);

    // PUT the contact
    client
        .put_contact(&contact_url, &vcard, None)
        .await
        .expect("put_contact should succeed");

    // Sync to confirm it exists
    let result = client
        .sync_contacts(&book.href, None)
        .await
        .expect("sync after put should succeed");

    let found = result.contacts.iter().any(|c| c.uid == uid);
    assert!(found, "just-created contact should appear in sync");

    // DELETE the contact
    client
        .delete_contact(&contact_url, None)
        .await
        .expect("delete_contact should succeed");

    // Sync again to confirm deletion
    let result2 = client
        .sync_contacts(&book.href, result.new_sync_token.as_deref())
        .await
        .expect("sync after delete should succeed");

    let still_present = result2.contacts.iter().any(|c| c.uid == uid);
    let deleted = result2.deleted_urls.iter().any(|u| u.contains(&uid));
    assert!(
        !still_present || deleted,
        "deleted contact should not appear or should be in deleted_urls"
    );
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_discover_well_known() {
    let Ok(url) = std::env::var("CARDDAV_DISCOVER_HOST") else {
        eprintln!("CARDDAV_DISCOVER_HOST not set, skipping");
        return;
    };

    let _client = CardDavClient::discover(&url)
        .await
        .expect("discover should succeed");
}
