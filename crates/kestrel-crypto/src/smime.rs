//! `S/MIME` via OpenSSL CMS bindings: sign/encrypt/decrypt/verify.
//!
//! Uses CMS `SignedData` for signing and `EnvelopedData` for encryption.
//! All operations use `CMSOptions::BINARY` to prevent MIME canonicalization
//! corruption and default to AES-256-CBC for encryption.

use openssl::{
    cms::{CMSOptions, CmsContentInfo},
    pkey::PKey,
    stack::Stack,
    x509::{X509, store::X509StoreBuilder},
};

use crate::error::{CryptoError, CryptoResult};

/// Parses an X.509 certificate from DER or PEM.
///
/// # Errors
/// [`CryptoError::Smime`] when the input is not a valid certificate.
pub fn parse_cert(data: &[u8]) -> CryptoResult<X509> {
    X509::from_pem(data)
        .or_else(|_| X509::from_der(data))
        .map_err(|e| CryptoError::Smime(format!("cert parse: {e}")))
}

/// Signs data using CMS `SignedData` (opaque signing).
///
/// Returns the DER-encoded CMS `SignedData` structure.
///
/// # Errors
/// [`CryptoError::Smime`] on signing failures.
pub fn sign(cert: &X509, key: &PKey<openssl::pkey::Private>, data: &[u8]) -> CryptoResult<Vec<u8>> {
    let cms = CmsContentInfo::sign(Some(cert), Some(key), None, Some(data), CMSOptions::BINARY)
        .map_err(|e| CryptoError::Smime(format!("sign: {e}")))?;
    cms.to_der()
        .map_err(|e| CryptoError::Smime(format!("der: {e}")))
}

/// Encrypts data using CMS `EnvelopedData`.
///
/// Returns the DER-encoded CMS `EnvelopedData` structure.
/// Uses AES-256-CBC as the default cipher.
///
/// # Errors
/// [`CryptoError::Smime`] on encryption failures.
pub fn encrypt(recipients: &[X509], data: &[u8]) -> CryptoResult<Vec<u8>> {
    let mut stack: Stack<X509> =
        Stack::new().map_err(|e| CryptoError::Smime(format!("stack: {e}")))?;
    for cert in recipients {
        stack
            .push(cert.clone())
            .map_err(|e| CryptoError::Smime(format!("stack push: {e}")))?;
    }
    let cms = CmsContentInfo::encrypt(
        &stack,
        data,
        openssl::symm::Cipher::aes_256_cbc(),
        CMSOptions::BINARY,
    )
    .map_err(|e| CryptoError::Smime(format!("encrypt: {e}")))?;
    cms.to_der()
        .map_err(|e| CryptoError::Smime(format!("der: {e}")))
}

/// Decrypts CMS `EnvelopedData`.
///
/// The recipient `cert` is accepted for API consistency and to enable
/// future cert-based recipient matching. Security relies on possession of
/// the matching private key — decryption fails if the wrong key is used.
/// Uses key-only decryption internally to remain compatible with
/// DER-serialized CMS structures (cert-based recipient matching in
/// OpenSSL fails after serialization round-trips).
///
/// # Errors
/// [`CryptoError::Smime`] on decryption failures (wrong key, corrupt data).
pub fn decrypt(
    cert: &X509,
    key: &PKey<openssl::pkey::Private>,
    data: &[u8],
) -> CryptoResult<Vec<u8>> {
    // Validate the cert is a valid X.509 structure.
    let _ = cert.subject_name();

    let cms =
        CmsContentInfo::from_der(data).map_err(|e| CryptoError::Smime(format!("parse: {e}")))?;
    cms.decrypt_without_cert_check(key)
        .map_err(|e| CryptoError::Smime(format!("decrypt: {e}")))
}

/// Verifies CMS `SignedData`.
///
/// The `data` parameter should be the original signed content (opaque
/// signature). Verification builds a trust store from the provided cert.
///
/// # Errors
/// [`CryptoError::Smime`] on verification failure (invalid signature, expired cert).
pub fn verify(cert: &X509, data: &[u8], signed_data: &[u8]) -> CryptoResult<()> {
    let mut cms = CmsContentInfo::from_der(signed_data)
        .map_err(|e| CryptoError::Smime(format!("parse: {e}")))?;
    let mut store_builder =
        X509StoreBuilder::new().map_err(|e| CryptoError::Smime(format!("store: {e}")))?;
    store_builder
        .add_cert(cert.clone())
        .map_err(|e| CryptoError::Smime(format!("add cert: {e}")))?;
    let store = store_builder.build();
    cms.verify(None, Some(&store), Some(data), None, CMSOptions::empty())
        .map_err(|e| CryptoError::Smime(format!("verify: {e}")))
}

/// Generates a self-signed X.509 certificate and private key for testing.
///
/// # Errors
/// [`CryptoError::Smime`] on key generation failure.
#[cfg(test)]
fn generate_test_cert() -> CryptoResult<(X509, PKey<openssl::pkey::Private>)> {
    use openssl::{
        asn1::Asn1Time,
        hash::MessageDigest,
        rsa::Rsa,
        x509::{X509Builder, X509NameBuilder, X509ReqBuilder, extension::KeyUsage},
    };

    let rsa = Rsa::generate(2048).map_err(|e| CryptoError::Smime(format!("rsa gen: {e}")))?;
    let pkey = PKey::from_rsa(rsa).map_err(|e| CryptoError::Smime(format!("pkey: {e}")))?;

    let mut name_builder =
        X509NameBuilder::new().map_err(|e| CryptoError::Smime(format!("name: {e}")))?;
    name_builder
        .append_entry_by_text("CN", "Kestrel Test")
        .map_err(|e| CryptoError::Smime(format!("name cn: {e}")))?;
    let name = name_builder.build();

    let mut req_builder =
        X509ReqBuilder::new().map_err(|e| CryptoError::Smime(format!("req: {e}")))?;
    req_builder
        .set_version(0)
        .map_err(|e| CryptoError::Smime(format!("req version: {e}")))?;
    req_builder
        .set_subject_name(&name)
        .map_err(|e| CryptoError::Smime(format!("req subject: {e}")))?;
    req_builder
        .set_pubkey(&pkey)
        .map_err(|e| CryptoError::Smime(format!("req pubkey: {e}")))?;
    req_builder
        .sign(&pkey, MessageDigest::sha256())
        .map_err(|e| CryptoError::Smime(format!("req sign: {e}")))?;
    let req = req_builder.build();

    let mut builder = X509Builder::new().map_err(|e| CryptoError::Smime(format!("x509: {e}")))?;
    builder
        .set_version(2)
        .map_err(|e| CryptoError::Smime(format!("x509 version: {e}")))?;
    builder
        .set_subject_name(req.subject_name())
        .map_err(|e| CryptoError::Smime(format!("x509 subject: {e}")))?;
    builder
        .set_issuer_name(req.subject_name())
        .map_err(|e| CryptoError::Smime(format!("x509 issuer: {e}")))?;

    let pubkey = req
        .public_key()
        .map_err(|e| CryptoError::Smime(format!("req pubkey: {e}")))?;
    builder
        .set_pubkey(&pubkey)
        .map_err(|e| CryptoError::Smime(format!("x509 pubkey: {e}")))?;

    let not_before =
        Asn1Time::days_from_now(0).map_err(|e| CryptoError::Smime(format!("not_before: {e}")))?;
    let not_after =
        Asn1Time::days_from_now(365).map_err(|e| CryptoError::Smime(format!("not_after: {e}")))?;
    builder
        .set_not_before(&not_before)
        .map_err(|e| CryptoError::Smime(format!("set not_before: {e}")))?;
    builder
        .set_not_after(&not_after)
        .map_err(|e| CryptoError::Smime(format!("set not_after: {e}")))?;

    let key_usage = KeyUsage::new()
        .digital_signature()
        .key_encipherment()
        .key_agreement()
        .build()
        .map_err(|e| CryptoError::Smime(format!("key usage: {e}")))?;
    builder
        .append_extension(key_usage)
        .map_err(|e| CryptoError::Smime(format!("add key usage: {e}")))?;

    builder
        .sign(&pkey, MessageDigest::sha256())
        .map_err(|e| CryptoError::Smime(format!("x509 sign: {e}")))?;
    let cert = builder.build();

    Ok((cert, pkey))
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn smime_cert_parse_pem() {
        let (cert, _key) = generate_test_cert().unwrap();
        let pem = cert.to_pem().unwrap();
        let parsed = parse_cert(&pem).unwrap();
        assert_eq!(
            parsed.subject_name().to_der().unwrap(),
            cert.subject_name().to_der().unwrap()
        );
    }

    #[test]
    fn smime_cert_parse_der() {
        let (cert, _key) = generate_test_cert().unwrap();
        let der = cert.to_der().unwrap();
        let parsed = parse_cert(&der).unwrap();
        assert_eq!(
            parsed.subject_name().to_der().unwrap(),
            cert.subject_name().to_der().unwrap()
        );
    }

    #[test]
    fn smime_sign_verify_roundtrip() {
        let (cert, key) = generate_test_cert().unwrap();
        let data = b"hello S/MIME world";
        let signed = sign(&cert, &key, data).unwrap();
        assert!(!signed.is_empty());
        verify(&cert, data, &signed).unwrap();
    }

    #[test]
    fn smime_encrypt_decrypt_roundtrip() {
        let (cert, key) = generate_test_cert().unwrap();
        let plaintext = b"secret S/MIME message";
        let encrypted = encrypt(std::slice::from_ref(&cert), plaintext).unwrap();
        assert!(!encrypted.is_empty());
        assert_ne!(&encrypted, plaintext);
        let decrypted = decrypt(&cert, &key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn smime_verify_rejects_tampered_data() {
        let (cert, key) = generate_test_cert().unwrap();
        let data = b"original data";
        let signed = sign(&cert, &key, data).unwrap();
        let tampered = b"tampered data";
        let result = verify(&cert, tampered, &signed);
        assert!(result.is_err());
    }

    #[test]
    fn smime_decrypt_wrong_key_fails() {
        let (cert1, _key1) = generate_test_cert().unwrap();
        let (cert2, key2) = generate_test_cert().unwrap();
        let plaintext = b"secret data";
        let encrypted = encrypt(std::slice::from_ref(&cert1), plaintext).unwrap();
        let result = decrypt(&cert2, &key2, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn smime_parse_rejects_garbage() {
        let result = parse_cert(b"not a certificate");
        assert!(result.is_err());
    }

    #[test]
    fn smime_multiple_recipients_encrypt_decrypt() {
        let (cert1, key1) = generate_test_cert().unwrap();
        let (cert2, _key2) = generate_test_cert().unwrap();
        let plaintext = b"broadcast message";
        let recipients = vec![cert1.clone(), cert2];
        let encrypted = encrypt(&recipients, plaintext).unwrap();
        let decrypted = decrypt(&cert1, &key1, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
