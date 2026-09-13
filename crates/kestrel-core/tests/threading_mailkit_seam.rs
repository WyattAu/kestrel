//! Integration tests for the `kestrel-core` ↔ `mailkit` threading seam
//! (`threading.rs` re-exports `mailkit::threading`; schema.md §3.4).
//!
//! These lock the seam contract kestrel ingests through: reply chains group
//! by `Message-ID` links, subject fallback stays inside the ±7-day window,
//! and assignment is deterministic across runs.

use kestrel_core::threading::{
    SUBJECT_WINDOW_MS, ThreadAssignment, ThreadInput, normalize_subject, thread_messages,
};

fn input(
    id: &str,
    message_id: Option<&str>,
    in_reply_to: Option<&str>,
    subject: Option<&str>,
    timestamp: i64,
) -> ThreadInput {
    ThreadInput {
        id: id.to_string(),
        message_id: message_id.map(str::to_string),
        in_reply_to: in_reply_to.map(str::to_string),
        references: Vec::new(),
        subject: subject.map(str::to_string),
        timestamp,
    }
}

#[test]
fn reply_chain_groups_into_one_thread() {
    let base: i64 = 1_700_000_000_000;
    let msgs = vec![
        input("root", Some("a@x"), None, Some("Launch plan"), base),
        input(
            "reply",
            Some("b@x"),
            Some("a@x"),
            Some("Re: Launch plan"),
            base + 1,
        ),
        input(
            "nested",
            Some("c@x"),
            Some("b@x"),
            Some("Re: Launch plan"),
            base + 2,
        ),
    ];

    let assignments = thread_messages(&msgs);
    assert_eq!(assignments.len(), 3);
    let keys: Vec<&str> = assignments
        .iter()
        .map(|a: &ThreadAssignment| a.thread_key.as_str())
        .collect();
    assert!(
        keys.iter().all(|k| keys[0] == *k),
        "all three messages share a thread key"
    );
}

#[test]
fn unrelated_messages_stay_in_separate_threads() {
    let base: i64 = 1_700_000_000_000;
    let msgs = vec![
        input("m1", Some("a@x"), None, Some("Invoice"), base),
        input("m2", Some("b@x"), None, Some("Lunch?"), base + 1),
    ];

    let assignments = thread_messages(&msgs);
    assert_eq!(assignments.len(), 2);
    assert_ne!(assignments[0].thread_key, assignments[1].thread_key);
}

#[test]
fn subject_fallback_respects_seven_day_window() {
    let base: i64 = 1_700_000_000_000;
    let inside_window = input(
        "inside",
        None,
        None,
        Some("Standup notes"),
        base + SUBJECT_WINDOW_MS - 1_000,
    );
    let outside_window = input(
        "outside",
        None,
        None,
        Some("Standup notes"),
        base + SUBJECT_WINDOW_MS + 1_000,
    );
    let anchor = input("anchor", None, None, Some("Standup notes"), base);

    let inside = thread_messages(&[anchor.clone(), inside_window]);
    assert_eq!(inside[0].thread_key, inside[1].thread_key);

    let outside = thread_messages(&[anchor, outside_window]);
    assert_ne!(outside[0].thread_key, outside[1].thread_key);
}

#[test]
fn normalization_strips_reply_prefixes_and_whitespace() {
    assert_eq!(
        normalize_subject("Re: Re: Standup notes  "),
        normalize_subject("standup notes")
    );
    assert_eq!(
        normalize_subject("Re: Launch plan"),
        normalize_subject("RE: launch plan")
    );
}

#[test]
fn assignment_order_follows_input_order() {
    let base: i64 = 1_700_000_000_000;
    let msgs = vec![
        input("z", Some("z@x"), None, Some("Thread A"), base),
        input(
            "a",
            Some("a@x"),
            Some("z@x"),
            Some("Re: Thread A"),
            base + 1,
        ),
        input("m", Some("m@x"), None, Some("Thread B"), base + 2),
    ];

    let assignments = thread_messages(&msgs);
    let ids: Vec<&str> = assignments.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["z", "a", "m"]);
}

#[test]
fn seam_is_deterministic_across_runs() {
    let base: i64 = 1_700_000_000_000;
    let msgs = vec![
        input("root", Some("r@x"), None, Some("Digest"), base),
        input(
            "r1",
            Some("1@x"),
            Some("r@x"),
            Some("Re: Digest"),
            base + 10,
        ),
        input(
            "r2",
            Some("2@x"),
            Some("1@x"),
            Some("Re: Digest"),
            base + 20,
        ),
        input("other", Some("o@x"), None, Some("Digest"), base + 30),
    ];

    let first = thread_messages(&msgs);
    let second = thread_messages(&msgs);
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.thread_key, b.thread_key);
    }
}
