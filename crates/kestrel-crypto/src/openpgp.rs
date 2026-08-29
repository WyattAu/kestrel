//! `OpenPGP` via Sequoia (Phase 5, ADR 0012): sign/encrypt/decrypt/verify.
//! Streaming stack per Sequoia's documented pattern: `Message` →
//! `Armorer`/`Encryptor`/`Signer` → `LiteralWriter` → `finalize`.

use std::io::{Read as _, Write as _};

use kestrel_core::secrets::SecretString;
use sequoia_openpgp::{
    Cert, KeyHandle, Result as SqResult, armor,
    cert::prelude::CertBuilder,
    crypto::{Password, SessionKey},
    parse::{
        Parse,
        stream::{DecryptionHelper, DecryptorBuilder, MessageStructure, VerificationHelper},
    },
    policy::{Policy, StandardPolicy},
    serialize::stream::{Armorer, Encryptor, LiteralWriter, Message, Signer},
    types::SymmetricAlgorithm,
};

use crate::error::{CryptoError, CryptoResult};

static POLICY: StandardPolicy<'_> = StandardPolicy::new();

/// Parses an armored cert (public or secret).
///
/// # Errors
/// [`CryptoError::OpenPgp`] when the input is not a valid cert.
pub fn parse_cert(armored: &str) -> CryptoResult<Cert> {
    Cert::from_bytes(armored.as_bytes())
        .map_err(|e| CryptoError::OpenPgp(format!("cert parse: {e}")))
}

/// Generates a cert with signing + encryption subkeys, optionally
/// password-protected.
///
/// # Errors
/// [`CryptoError::OpenPgp`] on generation failure.
pub fn generate_cert(
    userid: &str,
    password: Option<&SecretString>,
) -> CryptoResult<(Cert, sequoia_openpgp::packet::Signature)> {
    let mut builder = CertBuilder::new()
        .add_userid(user_id_shape(userid))
        .add_signing_subkey()
        .add_transport_encryption_subkey();
    if let Some(pw) = password {
        builder = builder.set_password(Some(Password::from(pw.expose().to_owned())));
    }
    builder
        .generate()
        .map_err(|e| CryptoError::OpenPgp(format!("key generation: {e}")))
}

fn user_id_shape(email_or_name: &str) -> String {
    if email_or_name.contains('@') {
        format!("<{email_or_name}>")
    } else {
        format!("{email_or_name} <{email_or_name}@kestrel.example>")
    }
}

/// Unlocks the first usable signing keypair of a secret cert.
fn signing_keypair(
    cert: &Cert,
    password: &SecretString,
) -> CryptoResult<sequoia_openpgp::crypto::KeyPair> {
    let pp = Password::from(password.expose().to_owned());
    if let Some(ka) = cert
        .keys()
        .secret()
        .with_policy(&POLICY, None)
        .for_signing()
        .next()
    {
        let key = ka.key().clone();
        let key = if key.has_unencrypted_secret() {
            key
        } else {
            key.decrypt_secret(&pp)
                .map_err(|e| CryptoError::OpenPgp(format!("key unlock: {e}")))?
        };
        return key
            .into_keypair()
            .map_err(|e| CryptoError::OpenPgp(e.to_string()));
    }
    Err(CryptoError::OpenPgp("no usable signing key".into()))
}

/// Signs `data` into an armored, compressed one-pass signed message.
///
/// # Errors
/// [`CryptoError::OpenPgp`] on key/algorithm failures.
pub fn sign(secret_cert: &Cert, password: &SecretString, data: &[u8]) -> CryptoResult<Vec<u8>> {
    let keypair = signing_keypair(secret_cert, password)?;
    let mut sink = Vec::new();
    let message = Message::new(&mut sink);
    let armored = Armorer::new(message)
        .kind(armor::Kind::Message)
        .build()
        .map_err(err("armorer"))?;
    let signer = Signer::new(armored, keypair).map_err(err("signer"))?;
    let signer = signer.build().map_err(err("signer"))?;
    let mut literal = LiteralWriter::new(signer).build().map_err(err("literal"))?;
    literal.write_all(data).map_err(io_err("write"))?;
    literal.finalize().map_err(err("finalize"))?;
    Ok(sink)
}

/// Encrypts `data` to the recipient certs, optionally signing with the
/// unlocked sender key.
///
/// # Errors
/// [`CryptoError::OpenPgp`] on failures.
pub fn encrypt(
    recipients: &[Cert],
    sign_with: Option<(&Cert, &SecretString)>,
    data: &[u8],
) -> CryptoResult<Vec<u8>> {
    let mut sink = Vec::new();
    let message = Message::new(&mut sink);
    let armored = Armorer::new(message)
        .kind(armor::Kind::Message)
        .build()
        .map_err(err("armorer"))?;
    let encryptor = Encryptor::for_recipients(
        armored,
        recipients.iter().flat_map(|c| {
            c.keys()
                .with_policy(&POLICY, None)
                .supported()
                .alive()
                .revoked(false)
                .for_transport_encryption()
        }),
    )
    .build()
    .map_err(err("encryptor"))?;

    if let Some((cert, password)) = sign_with {
        let keypair = signing_keypair(cert, password)?;
        let signer = Signer::new(encryptor, keypair).map_err(err("signer"))?;
        let signer = signer.build().map_err(err("signer"))?;
        let mut literal = LiteralWriter::new(signer).build().map_err(err("literal"))?;
        literal.write_all(data).map_err(io_err("write"))?;
        literal.finalize().map_err(err("finalize"))?;
        return Ok(sink);
    }
    let mut literal = LiteralWriter::new(encryptor)
        .build()
        .map_err(err("literal"))?;
    literal.write_all(data).map_err(io_err("write"))?;
    literal.finalize().map_err(err("finalize"))?;
    Ok(sink)
}

/// Helper feeding secret keys + a trust-any-known-certs verification
/// policy to the streaming decryptor.
struct DecryptHelper<'a> {
    secret: &'a Cert,
    password: Password,
    policy: &'a dyn Policy,
    verify_certs: &'a [Cert],
    signed_by: Option<String>,
}

impl VerificationHelper for DecryptHelper<'_> {
    fn get_certs(&mut self, handles: &[KeyHandle]) -> SqResult<Vec<Cert>> {
        Ok(self
            .verify_certs
            .iter()
            .filter(|c| {
                handles.iter().any(|h| {
                    c.key_handle().aliases(h)
                        || c.keys()
                            .any(|k| KeyHandle::KeyID(k.key().keyid().clone()).aliases(h))
                })
            })
            .cloned()
            .collect())
    }

    fn check(&mut self, structure: MessageStructure<'_>) -> SqResult<()> {
        for layer in structure {
            if let sequoia_openpgp::parse::stream::MessageLayer::SignatureGroup { results } = layer
            {
                for r in results {
                    let Ok(good) = r else { continue };
                    let issuers: Vec<sequoia_openpgp::KeyID> =
                        good.sig.issuers().cloned().collect();
                    if let Some(cert) = self.verify_certs.iter().find(|c| {
                        issuers
                            .iter()
                            .any(|id| c.keys().any(|k| k.key().keyid().to_hex() == id.to_hex()))
                    }) {
                        self.signed_by = Some(cert.fingerprint().to_hex());
                    }
                }
            }
        }
        Ok(())
    }
}

impl DecryptionHelper for DecryptHelper<'_> {
    fn decrypt(
        &mut self,
        pkesks: &[sequoia_openpgp::packet::PKESK],
        _skesks: &[sequoia_openpgp::packet::SKESK],
        sym_algo: Option<SymmetricAlgorithm>,
        decrypt: &mut dyn FnMut(Option<SymmetricAlgorithm>, &SessionKey) -> bool,
    ) -> SqResult<Option<Cert>> {
        for ka in self
            .secret
            .keys()
            .secret()
            .with_policy(self.policy, None)
            .for_transport_encryption()
        {
            let key = ka.key().clone();
            let key = if key.has_unencrypted_secret() {
                key
            } else {
                let Ok(unlocked) = key.decrypt_secret(&self.password) else {
                    continue;
                };
                unlocked
            };
            let Ok(mut pair) = key.into_keypair() else {
                continue;
            };
            for pkesk in pkesks {
                if pkesk
                    .decrypt(&mut pair, sym_algo)
                    .is_some_and(|(algo, session_key)| decrypt(algo, &session_key))
                {
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }
}

/// Decrypts (and verifies, when signed) an armored message. Returns the
/// plaintext plus the signing cert's fingerprint when a valid signature by
/// one of `verify_certs` is present.
///
/// # Errors
/// [`CryptoError::OpenPgp`] on structure or key failures.
pub fn decrypt(
    secret_cert: &Cert,
    password: &SecretString,
    armored: &[u8],
    verify_certs: &[Cert],
) -> CryptoResult<(Vec<u8>, Option<String>)> {
    let helper = DecryptHelper {
        secret: secret_cert,
        password: Password::from(password.expose().to_owned()),
        policy: &POLICY,
        verify_certs,
        signed_by: None,
    };
    let mut decryptor = DecryptorBuilder::from_bytes(armored)
        .map_err(err("parse"))?
        .with_policy(&POLICY, None, helper)
        .map_err(err("decryptor"))?;
    let mut plaintext = Vec::new();
    decryptor
        .read_to_end(&mut plaintext)
        .map_err(io_err("read"))?;
    let verified = decryptor.helper_ref().signed_by.clone();
    drop(decryptor);
    Ok((plaintext, verified))
}

/// Exports a cert in armored form.
///
/// # Errors
/// [`CryptoError::OpenPgp`] on serialization failure.
pub fn armor_cert(cert: &Cert) -> CryptoResult<String> {
    use sequoia_openpgp::serialize::Serialize as _;
    let mut sink = Vec::new();
    let mut writer =
        sequoia_openpgp::armor::Writer::new(&mut sink, sequoia_openpgp::armor::Kind::PublicKey)
            .map_err(io_err("armor"))?;
    cert.serialize(&mut writer).map_err(err("serialize"))?;
    writer.finalize().map_err(io_err("armor finalize"))?;
    Ok(String::from_utf8_lossy(&sink).into_owned())
}

fn err(prefix: &'static str) -> impl Fn(sequoia_openpgp::anyhow::Error) -> CryptoError {
    move |e| CryptoError::OpenPgp(format!("{prefix}: {e}"))
}

fn io_err(prefix: &'static str) -> impl Fn(std::io::Error) -> CryptoError {
    move |e| CryptoError::OpenPgp(format!("{prefix}: {e}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn empty_pw() -> SecretString {
        SecretString::new(String::new())
    }

    #[test]
    fn armor_roundtrip() {
        let (cert, _rev) = generate_cert("alice@wonderland.example", None).unwrap();
        let armored = armor_cert(&cert).unwrap();
        assert!(armored.starts_with("-----BEGIN PGP"));
        let parsed = parse_cert(&armored).unwrap();
        assert_eq!(parsed.fingerprint(), cert.fingerprint());
    }

    #[test]
    fn encrypt_decrypt_roundtrip_unsigned() {
        let (recipient, _rev) = generate_cert("bob@example.org", None).unwrap();
        let plaintext = b"meet at the usual place";
        let ciphertext = encrypt(std::slice::from_ref(&recipient), None, plaintext).unwrap();
        assert!(String::from_utf8_lossy(&ciphertext).contains("BEGIN PGP MESSAGE"));
        let (decrypted, signed_by) = decrypt(&recipient, &empty_pw(), &ciphertext, &[]).unwrap();
        assert_eq!(decrypted, plaintext);
        assert!(signed_by.is_none());
    }

    #[test]
    fn signed_encrypt_reports_signer() {
        let (sender, _rev) = generate_cert("alice@example.org", None).unwrap();
        let (recipient, _rev2) = generate_cert("bob@example.org", None).unwrap();
        let ciphertext = encrypt(
            std::slice::from_ref(&recipient),
            Some((&sender, &empty_pw())),
            b"signed hello",
        )
        .unwrap();
        let (plaintext, signed_by) = decrypt(
            &recipient,
            &empty_pw(),
            &ciphertext,
            std::slice::from_ref(&sender),
        )
        .unwrap();
        assert_eq!(plaintext, b"signed hello");
        let fp = signed_by.unwrap();
        assert_eq!(fp, sender.fingerprint().to_hex());
    }

    #[test]
    fn sign_produces_armored_message() {
        let (sender, _rev) = generate_cert("carol@example.org", None).unwrap();
        let sig = sign(&sender, &empty_pw(), b"payload").unwrap();
        let text = String::from_utf8_lossy(&sig);
        assert!(text.contains("BEGIN PGP MESSAGE"), "{text}");
    }

    #[test]
    fn password_protected_keys_roundtrip() {
        let pw = SecretString::new("correct horse".into());
        let (secret, _rev) = generate_cert("dave@example.org", Some(&pw)).unwrap();
        let plaintext = b"secret payload";
        let ciphertext = encrypt(std::slice::from_ref(&secret), None, plaintext).unwrap();
        let (decrypted, _) = decrypt(&secret, &pw, &ciphertext, &[]).unwrap();
        assert_eq!(decrypted, plaintext);
        // Wrong password fails cleanly.
        let wrong = decrypt(
            &secret,
            &SecretString::new("wrong".into()),
            &ciphertext,
            &[],
        );
        assert!(wrong.is_err());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_cert("not a cert at all").is_err());
    }
}
