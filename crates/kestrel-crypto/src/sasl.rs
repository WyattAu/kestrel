//! SASL client mechanisms (requirements §2.3): PLAIN, LOGIN, SCRAM-SHA-256
//! (RFC 7677), XOAUTH2 (RFC 7628). Step-wise sessions driven by IMAP
//! AUTHENTICATE continuation rounds.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::{
    credentials::SecretString,
    error::{CryptoError, CryptoResult},
};

type HmacSha256 = Hmac<Sha256>;

/// A step-wise SASL exchange.
pub trait SaslSession {
    /// The mechanism name (IMAP `AUTHENTICATE <name>`).
    fn mechanism(&self) -> SaslMechanism;

    /// Initial response bytes (SASL IR), when the mechanism supports one.
    fn initial_response(&mut self) -> Option<Vec<u8>>;

    /// Feeds a server challenge, producing the next response.
    ///
    /// # Errors
    /// [`CryptoError::Sasl`] on protocol violations.
    fn respond(&mut self, challenge: &[u8]) -> CryptoResult<Vec<u8>>;

    /// `true` once the exchange has reached its final client message.
    fn is_complete(&self) -> bool;
}

/// Supported mechanisms.
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

/// Starts a session for the mechanism with the given credentials.
#[must_use]
pub fn start(
    mechanism: SaslMechanism,
    username: &str,
    password: &SecretString,
) -> Box<dyn SaslSession + Send> {
    match mechanism {
        SaslMechanism::Plain => Box::new(PlainSession {
            initial: plain_ir(username, password.expose()),
            complete: false,
        }),
        SaslMechanism::Login => Box::new(LoginSession {
            stage: LoginStage::Initial,
            username: username.to_owned(),
            password: password.expose().to_owned(),
        }),
        SaslMechanism::ScramSha256 => Box::new(ScramSha256Session::new(username, password)),
        SaslMechanism::Xoauth2 => Box::new(PlainSession {
            initial: xoauth2_ir(username, password.expose()),
            complete: false,
        }),
    }
}

fn plain_ir(user: &str, pass: &str) -> Vec<u8> {
    format!("\x00{user}\x00{pass}").into_bytes()
}

fn xoauth2_ir(user: &str, token: &str) -> Vec<u8> {
    format!("user={user}\x01auth=Bearer {token}\x01\x01").into_bytes()
}

struct PlainSession {
    initial: Vec<u8>,
    complete: bool,
}

impl SaslSession for PlainSession {
    fn mechanism(&self) -> SaslMechanism {
        SaslMechanism::Plain
    }

    fn initial_response(&mut self) -> Option<Vec<u8>> {
        Some(std::mem::take(&mut self.initial))
    }

    fn respond(&mut self, _challenge: &[u8]) -> CryptoResult<Vec<u8>> {
        Err(CryptoError::Sasl("no challenge round expected".into()))
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

struct LoginSession {
    stage: LoginStage,
    username: String,
    password: String,
}

enum LoginStage {
    Initial,
    WaitPassword,
    Done,
}

impl SaslSession for LoginSession {
    fn mechanism(&self) -> SaslMechanism {
        SaslMechanism::Login
    }

    fn initial_response(&mut self) -> Option<Vec<u8>> {
        None
    }

    fn respond(&mut self, challenge: &[u8]) -> CryptoResult<Vec<u8>> {
        match self.stage {
            LoginStage::Initial => {
                // Server asks "Username:" (base64); answer the username.
                self.stage = LoginStage::WaitPassword;
                Ok(self.username.clone().into_bytes())
            }
            LoginStage::WaitPassword => {
                let _ = challenge;
                self.stage = LoginStage::Done;
                Ok(self.password.clone().into_bytes())
            }
            LoginStage::Done => Err(CryptoError::Sasl("exchange already complete".into())),
        }
    }

    fn is_complete(&self) -> bool {
        matches!(self.stage, LoginStage::Done)
    }
}

/// RFC 7677 SCRAM-SHA-256 client.
struct ScramSha256Session {
    password: Vec<u8>,
    client_first_bare: String,
    client_nonce: String,
    state: ScramState,
}

#[derive(Clone)]
enum ScramState {
    Initial,
    SentFirst,
    Complete,
}

impl ScramSha256Session {
    fn new(username: &str, password: &SecretString) -> Self {
        // SASLprep is identity for the common case; escaping per RFC 4013
        // minimally (',' and '=' in usernames).
        let escaped = username.replace('=', "=3D").replace(',', "=2C");
        let mut nonce_bytes = [0u8; 18];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let client_nonce = B64.encode(nonce_bytes);
        let client_first_bare = format!("n={escaped},r={client_nonce}");
        Self {
            password: password.expose().as_bytes().to_vec(),
            client_first_bare,
            client_nonce,
            state: ScramState::Initial,
        }
    }
}

impl SaslSession for ScramSha256Session {
    fn mechanism(&self) -> SaslMechanism {
        SaslMechanism::ScramSha256
    }

    fn initial_response(&mut self) -> Option<Vec<u8>> {
        if matches!(self.state, ScramState::Initial) {
            self.state = ScramState::SentFirst;
            Some(format!("n,,{}", self.client_first_bare).into_bytes())
        } else {
            None
        }
    }

    fn respond(&mut self, challenge: &[u8]) -> CryptoResult<Vec<u8>> {
        match self.state.clone() {
            ScramState::SentFirst => {
                let server_first = String::from_utf8_lossy(challenge).into_owned();
                let mut server_nonce = String::new();
                let mut salt = Vec::new();
                let mut iterations = 0u32;
                for kv in server_first.split(',') {
                    if let Some(v) = kv.strip_prefix("r=") {
                        v.clone_into(&mut server_nonce);
                    } else if let Some(v) = kv.strip_prefix("s=") {
                        salt = B64
                            .decode(v)
                            .map_err(|e| CryptoError::Sasl(format!("salt: {e}")))?;
                    } else if let Some(v) = kv.strip_prefix("i=") {
                        iterations = v
                            .parse()
                            .map_err(|_| CryptoError::Sasl("bad iteration count".into()))?;
                    }
                }
                if server_nonce.len() < self.client_nonce.len()
                    || !server_nonce.starts_with(&self.client_nonce)
                {
                    return Err(CryptoError::Sasl("server nonce invalid".into()));
                }
                if iterations == 0 || iterations > 100_000 {
                    return Err(CryptoError::Sasl(format!(
                        "unreasonable iteration count {iterations}"
                    )));
                }
                let (_salted, client_key) = scram_keys(&self.password, &salt, iterations);
                let mut stored_key_hasher = Sha256::new();
                stored_key_hasher.update(&client_key);
                let stored_key = stored_key_hasher.finalize();

                let client_final_bare = format!("c=biws,r={server_nonce}");
                let auth_message = format!(
                    "{},{},{}",
                    self.client_first_bare, server_first, client_final_bare
                );
                let client_signature = hmac_slice(&stored_key, auth_message.as_bytes());
                let proof: Vec<u8> = client_key
                    .iter()
                    .zip(client_signature.iter())
                    .map(|(a, b)| a ^ b)
                    .collect();
                self.state = ScramState::Complete;
                Ok(format!("{client_final_bare},p={}", B64.encode(proof)).into_bytes())
            }
            _ => Err(CryptoError::Sasl("unexpected challenge".into())),
        }
    }

    fn is_complete(&self) -> bool {
        matches!(self.state, ScramState::Complete)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn scram_keys(password: &[u8], salt: &[u8], iterations: u32) -> (Vec<u8>, Vec<u8>) {
    // SaltedPassword = PBKDF2-HMAC-SHA256(password, salt, i, 32)
    let mut salted_password = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut salted_password);
    // ClientKey = HMAC(SaltedPassword, "Client Key")
    let client_key = hmac_slice(&salted_password, b"Client Key");
    (salted_password.to_vec(), client_key)
}

fn hmac_slice(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key)
        .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn secret(s: &str) -> SecretString {
        SecretString::new(s.into())
    }

    #[test]
    fn plain_initial_response_shape() {
        let mut s = start(SaslMechanism::Plain, "alice", &secret("wonderland"));
        assert_eq!(
            s.initial_response().unwrap(),
            b"\x00alice\x00wonderland".to_vec()
        );
        assert!(s.respond(b"").is_err());
    }

    #[test]
    fn xoauth2_initial_response_shape() {
        let mut s = start(SaslMechanism::Xoauth2, "user", &secret("tok"));
        assert_eq!(
            String::from_utf8(s.initial_response().unwrap()).unwrap(),
            "user=user\x01auth=Bearer tok\x01\x01"
        );
    }

    #[test]
    fn login_two_rounds() {
        let mut s = start(SaslMechanism::Login, "alice", &secret("pw"));
        assert!(s.initial_response().is_none());
        let r1 = s.respond(b"Username:").unwrap();
        assert_eq!(r1, b"alice".to_vec());
        let r2 = s.respond(b"Password:").unwrap();
        assert_eq!(r2, b"pw".to_vec());
        assert!(s.is_complete());
    }

    /// Full SCRAM-SHA-256 exchange against a local server-side model.
    #[test]
    fn scram_round_trip_against_reference() {
        let user = "alice";
        let pass = secret("pencil");
        let mut client = start(SaslMechanism::ScramSha256, user, &pass);

        // Server side (model, RFC 7677 §3):
        let client_first = String::from_utf8(client.initial_response().unwrap()).unwrap();
        assert!(client_first.starts_with("n,,"));
        let client_first_bare = client_first[3..].to_string();
        let c_nonce = client_first_bare
            .split(',')
            .find_map(|k| k.strip_prefix("r="))
            .unwrap()
            .to_string();

        let salt = B64.encode(b"sodium chloride!");
        let iterations = 4096_u32;
        let server_nonce = format!("{c_nonce}server");
        let server_first = format!("r={server_nonce},s={salt},i={iterations}");

        let client_final =
            String::from_utf8(client.respond(server_first.as_bytes()).unwrap()).unwrap();
        assert!(client.is_complete());
        assert!(client_final.starts_with("c=biws,r="));

        // Verify the proof: recompute server-side with the same salted keys.
        let (salted, client_key) = scram_keys(b"pencil", b"sodium chloride!", iterations);
        let mut stored = Sha256::new();
        stored.update(&client_key);
        let stored_key = stored.finalize();
        let auth_message = format!(
            "{},{},{}",
            client_first_bare,
            server_first,
            client_final.split(",p=").next().unwrap()
        );
        let signature = hmac_slice(&stored_key, auth_message.as_bytes());
        let expected_proof: Vec<u8> = client_key
            .iter()
            .zip(signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let got_proof = B64
            .decode(client_final.rsplit(",p=").next().unwrap())
            .unwrap();
        assert_eq!(got_proof, expected_proof);
        let _ = salted;
    }

    #[test]
    fn scram_rejects_nonce_mismatch() {
        let mut client = start(SaslMechanism::ScramSha256, "u", &secret("p"));
        let _ = client.initial_response();
        let bad = "r=otherservers=1".replace('=', ",");
        assert!(client.respond(bad.as_bytes()).is_err());
    }

    #[test]
    fn scram_escapes_specials() {
        let mut client = start(SaslMechanism::ScramSha256, "a,b=c", &secret("p"));
        let first = String::from_utf8(client.initial_response().unwrap()).unwrap();
        assert!(first.contains("n=a=2Cb=3Dc,"), "{first}");
    }
}
