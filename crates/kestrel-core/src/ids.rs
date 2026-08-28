//! Typed identifiers (architecture §8: no bare `String`/`u64` crossing crate
//! boundaries).
//!
//! All entity IDs are UUID v7 (time-ordered, index-friendly) stored as text
//! in `SQLite` and as UUIDs in memory. `BlobHash` is a SHA-256 digest. ID
//! *generation* goes through [`crate::ids::IdGenerator`] so tests can inject
//! deterministic sequences.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Wraps an existing UUID.
            #[must_use]
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// Parses a textual (UUID) form.
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                s.parse::<Uuid>().ok().map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0.hyphenated())
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }
    };
}

typed_id!(
    /// Account identity (`accounts.id`).
    AccountId
);
typed_id!(
    /// Folder identity (`folders.id`).
    FolderId
);
typed_id!(
    /// Message identity (`messages.id`).
    MessageId
);
typed_id!(
    /// MIME part identity (`parts.id`).
    PartId
);
typed_id!(
    /// Thread identity (`threads.id`).
    ThreadId
);
typed_id!(
    /// Outbox entry identity (`outbox.id`).
    OutboxId
);
typed_id!(
    /// Protocol request correlation id (`Command.id`).
    RequestId
);

/// SHA-256 content hash of a blob in the CAS (`docs/schema.md` §4).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobHash([u8; 32]);

impl BlobHash {
    /// Wraps a raw 32-byte digest.
    #[must_use]
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_digest(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex form (the on-disk/CAS representation).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parses a 64-char lowercase-or-uppercase hex digest.
    #[must_use]
    pub fn parse_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s).ok()?;
        let digest: [u8; 32] = bytes.try_into().ok()?;
        Some(Self(digest))
    }

    /// Two-level shard prefix (`ab/cd`) used by the CAS layout.
    #[must_use]
    pub fn shard_prefix(&self) -> String {
        let hex_str = self.to_hex();
        format!("{}/{}", &hex_str[0..2], &hex_str[2..4])
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobHash({})", self.to_hex())
    }
}

impl Serialize for BlobHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for BlobHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        BlobHash::parse_hex(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid sha256 hex digest: {s:?}")))
    }
}

/// Deterministic, injectable ID source (architecture §8; engineering-standards
/// §1 "determinism first").
pub trait IdGenerator: Send + Sync {
    /// Generates a fresh [`AccountId`]-class identifier (UUID v7).
    fn next_id(&self) -> Uuid;
}

/// Production generator: wall-clock UUID v7. The only audited site that
/// touches wall time for IDs.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemIdGenerator;

impl IdGenerator for SystemIdGenerator {
    fn next_id(&self) -> Uuid {
        Uuid::now_v7()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn ids_roundtrip_through_text() {
        let u = Uuid::now_v7();
        let id = MessageId::from_uuid(u);
        assert_eq!(MessageId::parse(&id.to_string()), Some(id));
        assert_eq!(id.as_uuid(), u);
    }

    #[test]
    fn blob_hash_hex_roundtrip_and_shard() {
        let digest = [0xab_u8; 32];
        let h = BlobHash::from_digest(digest);
        let hex = h.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(BlobHash::parse_hex(&hex), Some(h.clone()));
        assert_eq!(h.shard_prefix(), "ab/ab");
        assert_eq!(BlobHash::parse_hex("zz"), None);
    }

    #[test]
    fn distinct_id_types_do_not_mix() {
        fn assert_distinct(_: &AccountId, _: &MessageId) {}
        let u = Uuid::now_v7();
        assert_distinct(&AccountId::from_uuid(u), &MessageId::from_uuid(u));
    }
}
