//! SASL vocabulary (ADR 0005: mechanisms live in `kestrel-crypto`,
//! consumed here as the trait seam so `kestrel-sync` needs no lateral
//! import). Step-wise sessions driven by IMAP AUTHENTICATE rounds.

/// Supported mechanisms (requirements §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaslMechanism {
    /// RFC 4616.
    Plain,
    /// Legacy LOGIN.
    Login,
    /// RFC 7677.
    ScramSha256,
    /// RFC 7628 (`OAuth2`).
    Xoauth2,
}

impl SaslMechanism {
    /// Wire name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "PLAIN",
            Self::Login => "LOGIN",
            Self::ScramSha256 => "SCRAM-SHA-256",
            Self::Xoauth2 => "XOAUTH2",
        }
    }
}

/// A step-wise SASL exchange (implementation in `kestrel-crypto`).
pub trait SaslSession {
    /// The mechanism name (IMAP `AUTHENTICATE <name>`).
    fn mechanism(&self) -> SaslMechanism;

    /// Initial response bytes (SASL IR), when the mechanism supports one.
    fn initial_response(&mut self) -> Option<Vec<u8>>;

    /// Feeds a server challenge, producing the next response.
    ///
    /// # Errors
    /// Mechanism-specific failure (malformed challenge).
    fn respond(&mut self, challenge: &[u8]) -> Result<Vec<u8>, crate::error::KestrelError>;

    /// `true` once the exchange has reached its final client message.
    fn is_complete(&self) -> bool;
}
