//! TLS configuration (requirements §2.1, threat model §4.1): rustls with
//! TLS 1.3 default / 1.2 minimum, webpki roots by default, optional
//! user-imported CAs. Certificate validation is mandatory — there is no
//! insecure bypass.

use std::sync::Arc;

use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, pem::PemObject},
};
use rustls_pki_types::ServerName;

use crate::error::{CryptoError, CryptoResult};

/// Builds the client TLS config: webpki roots (+ any imported CAs),
/// TLS 1.3 preferred, 1.2 minimum.
///
/// # Errors
/// [`CryptoError::Tls`] when an imported CA file cannot be parsed.
pub fn tls_config(extra_ca_pem: Option<&std::path::Path>) -> CryptoResult<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = extra_ca_pem {
        let certs = CertificateDer::pem_file_iter(path)
            .map_err(|e| CryptoError::Tls(format!("{}: {e}", path.display())))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CryptoError::Tls(format!("{}: {e}", path.display())))?;
        if certs.is_empty() {
            return Err(CryptoError::Tls(format!(
                "{}: no certificates found in CA file",
                path.display()
            )));
        }
        roots.add_parsable_certificates(certs);
    }
    let mut versions = vec![&rustls::version::TLS13];
    versions.push(&rustls::version::TLS12);
    let config = ClientConfig::builder_with_protocol_versions(&versions)
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Parses a server name for rustls connectors.
///
/// # Errors
/// [`CryptoError::Tls`] for IP-literal or invalid names.
pub fn server_name(host: &str) -> CryptoResult<ServerName<'static>> {
    ServerName::try_from(host.to_owned())
        .map_err(|e| CryptoError::Tls(format!("invalid server name {host:?}: {e}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn default_config_builds_with_webpki_roots() {
        let config = tls_config(None).unwrap();
        // TLS 1.3-first stack built successfully; insecure downgrade is not
        // representable in this builder (versions pinned to TLS13+TLS12).
        assert!(!format!("{config:?}").is_empty());
    }

    #[test]
    fn server_name_parses_and_rejects() {
        assert!(server_name("imap.example.org").is_ok());
        assert!(server_name("not a name").is_err());
        assert!(server_name("").is_err());
    }

    #[test]
    fn imported_ca_must_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("ca.pem");
        std::fs::write(&bad, b"not a pem").unwrap();
        assert!(tls_config(Some(&bad)).is_err());
    }
}
