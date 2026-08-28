//! JWZ-lite threading (schema.md §3.4): group by normalized `Message-ID`
//! chain (`in_reply_to` + `references`), fall back to normalized-subject
//! grouping within a ±7-day window.
//!
//! The algorithm is pure and table-driven → property-tested with generated
//! reply graphs (`docs/testing-strategy.md` §3). Threading runs inside the
//! ingestion transaction; a message's thread assignment is immutable once
//! written (re-threading only on `UIDVALIDITY` reconciliation).

use std::collections::HashMap;

use crate::{clock::UnixMillis, ids::MessageId};

/// Subject-window for fallback grouping (schema.md §3.4).
pub const SUBJECT_WINDOW_MS: UnixMillis = 7 * 24 * 60 * 60 * 1000;

/// Input row for the threading algorithm.
#[derive(Clone, Debug)]
pub struct ThreadInput {
    /// Message being threaded.
    pub id: MessageId,
    /// Normalized RFC 5322 `Message-ID` (angle brackets stripped), if any.
    pub message_id: Option<String>,
    /// `In-Reply-To` (normalized), if any.
    pub in_reply_to: Option<String>,
    /// Full `References` chain (normalized), oldest first, if captured.
    pub references: Vec<String>,
    /// Subject.
    pub subject: Option<String>,
    /// Sent/received timestamp (unix ms).
    pub date: UnixMillis,
}

/// Assignment of each message to a thread, keyed by the thread's storage key
/// (`threads.id` candidate): either the root message's normalized id or
/// `subject:<normalized-subject>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadAssignment {
    /// Threaded message.
    pub id: MessageId,
    /// Thread storage key.
    pub thread_key: String,
}

/// Threads a batch of messages. Union-find over message-id links; subject
/// fallback within the time window. Deterministic: result order follows
/// input order; roots are the oldest member of each group.
#[must_use]
pub fn thread_messages(inputs: &[ThreadInput]) -> Vec<ThreadAssignment> {
    if inputs.is_empty() {
        return Vec::new();
    }

    // message-id -> index in `inputs`
    let mut by_message_id: HashMap<&str, usize> = HashMap::with_capacity(inputs.len());
    for (i, input) in inputs.iter().enumerate() {
        if let Some(mid) = input.message_id.as_deref()
            && !mid.is_empty()
        {
            by_message_id.insert(mid, i);
        }
    }

    let mut uf = UnionFind::new(inputs.len());

    // 1) Link by reply chain: prefer the nearest resolvable reference
    //    (references last → most recent ancestor, then in_reply_to).
    for (i, input) in inputs.iter().enumerate() {
        let mut linked = false;
        if let Some(irt) = input.in_reply_to.as_deref()
            && let Some(&j) = by_message_id.get(irt)
        {
            uf.union(i, j);
            linked = true;
        }
        if !linked {
            for r in input.references.iter().rev() {
                if let Some(&j) = by_message_id.get(r.as_str()) {
                    uf.union(i, j);
                    break;
                }
            }
        }
    }

    // 2) Subject fallback: group messages with the same normalized subject
    //    within ± SUBJECT_WINDOW_MS that are not already linked into a
    //    different message-id chain... per schema §3.4 the fallback applies
    //    to messages without a resolvable chain. To keep assignments
    //    stable (immutability), subject grouping only merges groups that
    //    have no message-id anchor.
    let mut subject_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, input) in inputs.iter().enumerate() {
        let has_chain = input.in_reply_to.is_some() || !input.references.is_empty();
        if !has_chain
            && let Some(norm) = input.subject.as_deref().map(normalize_subject)
            && !norm.is_empty()
        {
            subject_groups.entry(norm).or_default().push(i);
        }
    }
    let mut by_root: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..inputs.len() {
        by_root.entry(uf.find(i)).or_default().push(i);
    }
    for (_, members) in subject_groups {
        // Only merge when each member's group is a singleton without a
        // message-id anchor (pure subject thread), and members are within
        // the window of the group's earliest message.
        let eligible: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| {
                inputs[i].message_id.is_none()
                    && by_root.get(&uf.find(i)).is_some_and(|g| g.len() == 1)
            })
            .collect();
        if eligible.len() < 2 {
            continue;
        }
        let earliest = eligible
            .iter()
            .map(|&i| inputs[i].date)
            .min()
            .unwrap_or(i64::MAX);
        for &i in &eligible {
            if (inputs[i].date - earliest).abs() <= SUBJECT_WINDOW_MS {
                uf.union(eligible[0], i);
            }
        }
    }

    // 3) Assign keys: root = oldest member of each set.
    let mut root_key: HashMap<usize, String> = HashMap::new();
    let mut assignments = Vec::with_capacity(inputs.len());
    for (i, input) in inputs.iter().enumerate() {
        let root = uf.find(i);
        let key = root_key
            .entry(root)
            .or_insert_with(|| root_thread_key(inputs, root, &mut uf))
            .clone();
        assignments.push(ThreadAssignment {
            id: input.id,
            thread_key: key,
        });
    }
    assignments
}

fn root_thread_key(inputs: &[ThreadInput], root: usize, uf: &mut UnionFind) -> String {
    // Members of this thread's group; the key is derived from the oldest.
    let members: Vec<usize> = (0..inputs.len())
        .filter(|&i| uf.find(i) == uf.find(root))
        .collect();
    let mut oldest = root;
    for &i in &members {
        if inputs[i].date < inputs[oldest].date {
            oldest = i;
        }
    }
    if let Some(mid) = inputs[oldest].message_id.as_deref()
        && !mid.is_empty()
    {
        return format!("mid:{mid}");
    }
    // Subject-derived keys are only valid for real subject-thread groups
    // (≥2 members); singletons get a unique key so same-subject messages in
    // different time windows never collide.
    if members.len() >= 2
        && let Some(sub) = inputs[oldest].subject.as_deref()
    {
        let norm = normalize_subject(sub);
        if !norm.is_empty() {
            return format!("subject:{norm}");
        }
    }
    format!("solo:{}", inputs[oldest].id)
}

/// Strips `re:`/`re[N]:`/`fw:`/`fwd:` prefixes and normalizes
/// whitespace/case (schema.md §3.4 `subject_norm`).
#[must_use]
pub fn normalize_subject(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let lower = s.to_lowercase();
        if lower.starts_with("re:") {
            s = s[3.min(s.len())..].trim_start();
        } else if let Some(close) = s.find(']')
            && matches!(s.get(..close), Some(p) if {
                let p2 = p.to_lowercase();
                p2.starts_with("re[") && p2[3..].chars().all(|c| c.is_ascii_digit())
            })
        {
            s = s[(close + 1).min(s.len())..].trim_start();
            s = s.strip_prefix(':').map_or(s, str::trim_start);
        } else if lower.strip_prefix("fw:").is_some() {
            s = s[3.min(s.len())..].trim_start();
        } else if lower.strip_prefix("fwd:").is_some() {
            s = s[4.min(s.len())..].trim_start();
        } else {
            break;
        }
    }
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]]; // path halving
            i = self.parent[i];
        }
        i
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use proptest::prelude::*;
    use uuid::Uuid;

    use super::*;

    fn input(mid: Option<&str>, irt: Option<&str>, subject: &str, date: i64) -> ThreadInput {
        ThreadInput {
            id: MessageId::from_uuid(Uuid::now_v7()),
            message_id: mid.map(str::to_owned),
            in_reply_to: irt.map(str::to_owned),
            references: Vec::new(),
            subject: Some(subject.to_owned()),
            date,
        }
    }

    #[test]
    fn reply_chain_groups() {
        let a = input(Some("a@x"), None, "Original", 1000);
        let b = input(Some("b@x"), Some("a@x"), "Re: Original", 2000);
        let c = input(Some("c@x"), Some("b@x"), "Re: Original", 3000);
        let out = thread_messages(&[a, b, c]);
        assert_eq!(out[0].thread_key, out[1].thread_key);
        assert_eq!(out[1].thread_key, out[2].thread_key);
        assert_eq!(out[0].thread_key, "mid:a@x");
    }

    #[test]
    fn subject_fallback_within_window() {
        let a = input(None, None, "Standup notes", 1000);
        let b = input(None, None, "Standup notes", 2000);
        let out = thread_messages(&[a, b]);
        assert_eq!(out[0].thread_key, out[1].thread_key);
        assert!(out[0].thread_key.starts_with("subject:standup notes"));
    }

    #[test]
    fn subject_fallback_expires_after_window() {
        let a = input(None, None, "Standup notes", 0);
        let b = input(None, None, "Standup notes", SUBJECT_WINDOW_MS + 1);
        let out = thread_messages(&[a, b]);
        assert_ne!(out[0].thread_key, out[1].thread_key);
    }

    #[test]
    fn re_prefixes_normalized() {
        assert_eq!(
            normalize_subject("Re: Re: FWD: Hello   World"),
            "hello world"
        );
        assert_eq!(normalize_subject("fw: bug"), "bug");
        assert_eq!(normalize_subject("Re[2]: again"), "again");
        assert_eq!(normalize_subject(""), "");
    }

    #[test]
    fn unanchored_messages_get_unique_threads() {
        let a = input(None, None, "", 1000);
        let b = input(None, None, "", 1000);
        let out = thread_messages(&[a, b]);
        assert_ne!(out[0].thread_key, out[1].thread_key);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Threading is deterministic and stable: two runs over the same
        /// input produce the identical assignment (idempotence for the
        /// storage layer's immutable thread keys).
        #[test]
        fn threading_is_deterministic(
            messages in prop::collection::vec(
                (prop::option::of("[a-z]{1,3}@[a-z]{2}"), prop::option::of("[a-z]{1,3}@[a-z]{2}"), "[a-z ]{0,12}", 0i64..20_000_000_000),
                0..32
            ),
        ) {
            let inputs: Vec<ThreadInput> = messages.iter().map(|(mid, irt, subj, date)| {
                ThreadInput {
                    id: MessageId::from_uuid(Uuid::now_v7()),
                    message_id: mid.clone(),
                    in_reply_to: irt.clone(),
                    references: vec![],
                    subject: if subj.is_empty() { None } else { Some(subj.clone()) },
                    date: *date,
                }
            }).collect();
            let first = thread_messages(&inputs);
            let second = thread_messages(&inputs);
            for i in 0..inputs.len() {
                prop_assert_eq!(&first[i].thread_key, &second[i].thread_key);
                prop_assert_eq!(first[i].id, inputs[i].id);
            }
        }
    }
}
