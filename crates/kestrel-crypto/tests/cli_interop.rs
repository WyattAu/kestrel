//! CLI interop matrix (backlog #15, Phase-5 exit criteria): round-trips
//! between our crypto stacks and the reference CLIs — `gnupg` for
//! `openpgp`, and `openssl smime` for S/MIME.
//!
//! Every test is `#[ignore]`d so plain `cargo test` stays green on machines
//! without the CLIs; the dedicated `cli-interop` CI job installs `gnupg` +
//! `openssl` and runs them with `--run-ignored ignored-only`. All tests are
//! hermetic: keys live in per-test `tempfile` GNUPGHOME/working dirs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use kestrel_core::secrets::SecretString;
use kestrel_crypto::{openpgp, smime};

const PLAINTEXT: &[u8] = b"interop round-trip payload: the quick brown fox 0123456789";

// ---------------------------------------------------------------- helpers

fn empty_pw() -> SecretString {
    SecretString::new(String::new())
}

fn run(bin: &str, args: &[&str], cwd: &Path) -> Output {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {bin}: {e}"));
    assert!(
        out.status.success(),
        "{bin} {args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn gpg(home: &Path, args: &[&str]) -> Output {
    let mut all: Vec<&str> = vec![
        "--batch",
        "--yes",
        "--homedir",
        home.to_str().expect("homedir utf8"),
        "--pinentry-mode",
        "loopback",
    ];
    all.extend_from_slice(args);
    // Homedir is absolute via --homedir, so run from the temp root: relative
    // --output paths land next to the test's input files.
    run("gpg", &all, home.parent().expect("temp root"))
}

/// Generates an unprotected ed25519/cv25519 keypair inside `home` with the
/// given user id (signing primary + encryption subkey).
fn gpg_gen_key(home: &Path, userid: &str, name: &str) {
    let params = format!(
        "%no-protection\nKey-Type: eddsa\nKey-Curve: ed25519\nSubkey-Type: ecdh\n\
         Subkey-Curve: cv25519\nName-Real: {name}\nName-Email: {userid}\n\
         Expire-Date: 0\n%commit\n"
    );
    let param_file = home.join("params.txt");
    std::fs::write(&param_file, params).unwrap();
    gpg(home, &["--gen-key", param_file.to_str().unwrap()]);
}

fn gpg_export_public(home: &Path, userid: &str) -> String {
    String::from_utf8(gpg(home, &["--armor", "--export", userid]).stdout)
        .expect("export stdout utf8")
}

fn gpg_export_secret(home: &Path, userid: &str) -> String {
    String::from_utf8(gpg(home, &["--armor", "--export-secret-keys", userid]).stdout)
        .expect("export-secret stdout utf8")
}

fn gpg_import(home: &Path, armored: &str) {
    let keyfile = home.join("import.asc");
    std::fs::write(&keyfile, armored).unwrap();
    gpg(home, &["--import", keyfile.to_str().unwrap()]);
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

// ---------------------------------------------------------------- OpenPGP x gpg

/// Direction 1: gpg encrypts to a *kestrel-generated* key; kestrel decrypts.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn gpg_encrypts_to_kestrel_key_kestrel_decrypts() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gnupghome");
    std::fs::create_dir_all(&home).unwrap();

    let (recipient, _rev) = openpgp::generate_cert("krecip@example.org", None).unwrap();
    let pub_armored = openpgp::armor_cert(&recipient).unwrap();
    gpg_import(&home, &pub_armored);

    let doc = write(dir.path(), "doc.txt", PLAINTEXT);
    gpg(
        &home,
        &[
            "--trust-model",
            "always",
            "--armor",
            // --aead-algo none: gpg >= 2.4 defaults to AEAD (tag 20), which
            // Sequoia 2.4.1 cannot decrypt; pin the classic SEIP form.
            "--aead-algo",
            "none",
            "--encrypt",
            "--recipient",
            "krecip@example.org",
            "--output",
            "doc.asc",
            doc.to_str().unwrap(),
        ],
    );
    let ciphertext = std::fs::read(dir.path().join("doc.asc")).unwrap();
    let (plaintext, signed_by) =
        openpgp::decrypt(&recipient, &empty_pw(), &ciphertext, &[]).unwrap();
    assert_eq!(plaintext, PLAINTEXT);
    assert!(signed_by.is_none());
}

/// Direction 2: kestrel encrypts to a *gpg-generated* key; gpg decrypts.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn kestrel_encrypts_to_gpg_key_gpg_decrypts() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gnupghome");
    std::fs::create_dir_all(&home).unwrap();

    gpg_gen_key(&home, "grecip@example.org", "GPG Recipient");
    let pub_armored = gpg_export_public(&home, "grecip@example.org");
    let recipient = openpgp::parse_cert(&pub_armored).unwrap();

    let ciphertext = openpgp::encrypt(std::slice::from_ref(&recipient), None, PLAINTEXT).unwrap();
    let enc = write(dir.path(), "from-kestrel.asc", &ciphertext);
    let out = gpg(&home, &["--decrypt", enc.to_str().unwrap()]);
    assert_eq!(out.stdout, PLAINTEXT);
}

/// Direction 3: a kestrel-generated signature verifies under gpg.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn kestrel_signature_verified_by_gpg() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gnupghome");
    std::fs::create_dir_all(&home).unwrap();

    let (signer, _rev) = openpgp::generate_cert("ksigner@example.org", None).unwrap();
    gpg_import(&home, &openpgp::armor_cert(&signer).unwrap());

    let armored_sig = openpgp::sign(&signer, &empty_pw(), PLAINTEXT).unwrap();
    let sig_file = write(dir.path(), "signed.asc", &armored_sig);

    let out = gpg(
        &home,
        &["--status-fd", "1", "--verify", sig_file.to_str().unwrap()],
    );
    let status = String::from_utf8_lossy(&out.stdout);
    assert!(
        status.contains("[GNUPG:] GOODSIG"),
        "expected GOODSIG, got: {status}"
    );
}

/// Direction 4: gpg signs *and* encrypts to a kestrel-held key; kestrel
/// decrypts and reports the gpg signer's fingerprint.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn gpg_signs_encrypts_kestrel_decrypts_and_reports_signer() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gnupghome");
    std::fs::create_dir_all(&home).unwrap();

    // gpg owns both keys: R (recipient) and S (signer).
    gpg_gen_key(&home, "recip@example.org", "Recipient");
    gpg_gen_key(&home, "signer@example.org", "Signer");

    // Hand kestrel the recipient's SECRET key and the signer's PUBLIC key.
    let recipient = openpgp::parse_cert(&gpg_export_secret(&home, "recip@example.org")).unwrap();
    let signer_pub = openpgp::parse_cert(&gpg_export_public(&home, "signer@example.org")).unwrap();
    let signer_fp = signer_pub.fingerprint().to_hex();

    let doc = write(dir.path(), "doc.txt", PLAINTEXT);
    gpg(
        &home,
        &[
            "--trust-model",
            "always",
            "--armor",
            // --aead-algo none: pin the classic SEIP form (see direction 1).
            // --rfc4880 does NOT disable AEAD in gpg >= 2.4.
            "--aead-algo",
            "none",
            "--sign",
            "--local-user",
            "signer@example.org",
            "--encrypt",
            "--recipient",
            "recip@example.org",
            "--output",
            "signed-enc.asc",
            doc.to_str().unwrap(),
        ],
    );
    let ciphertext = std::fs::read(dir.path().join("signed-enc.asc")).unwrap();
    let (plaintext, signed_by) = openpgp::decrypt(
        &recipient,
        &empty_pw(),
        &ciphertext,
        std::slice::from_ref(&signer_pub),
    )
    .unwrap();
    assert_eq!(plaintext, PLAINTEXT);
    let reported = signed_by.expect("signer must be reported");
    assert_eq!(reported, signer_fp);
}

// ------------------------------------------------------- S/MIME x openssl CLI

/// Generates a self-signed RSA cert/key via the openssl CLI (so `parse_cert`
/// and key parsing are exercised against real-world output).
fn openssl_make_cert(dir: &Path) -> (PathBuf, PathBuf) {
    let key_pem = dir.join("key.pem");
    let cert_pem = dir.join("cert.pem");
    run(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key_pem.to_str().unwrap(),
            "-out",
            cert_pem.to_str().unwrap(),
            "-days",
            "2",
            "-nodes",
            "-subj",
            "/CN=Kestrel Interop/emailAddress=smime@example.org",
            "-addext",
            "keyUsage=critical,digitalSignature,keyEncipherment",
            "-addext",
            "extendedKeyUsage=emailProtection",
        ],
        dir,
    );
    (cert_pem, key_pem)
}

fn read_key(key_pem: &Path) -> openssl::pkey::PKey<openssl::pkey::Private> {
    let pem = std::fs::read(key_pem).unwrap();
    openssl::pkey::PKey::private_key_from_pem(&pem).unwrap()
}

/// Direction A: openssl CLI verifies a kestrel CMS signature.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn openssl_cli_verifies_kestrel_signature() {
    let dir = tempfile::tempdir().unwrap();
    let (cert_pem, key_pem) = openssl_make_cert(dir.path());
    let cert = smime::parse_cert(&std::fs::read(&cert_pem).unwrap()).unwrap();
    let key = read_key(&key_pem);
    let data = b"signed by kestrel, verified by openssl";

    let signed = smime::sign(&cert, &key, data).unwrap();
    let sig_der = write(dir.path(), "sig.der", &signed);

    run(
        "openssl",
        &[
            "smime",
            "-verify",
            "-inform",
            "DER",
            "-in",
            sig_der.to_str().unwrap(),
            "-CAfile",
            cert_pem.to_str().unwrap(),
            "-out",
            "verified.txt",
        ],
        dir.path(),
    );
    let verified = std::fs::read(dir.path().join("verified.txt")).unwrap();
    assert_eq!(verified, data);
}

/// Direction B: openssl CLI decrypts a kestrel CMS envelope.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn openssl_cli_decrypts_kestrel_ciphertext() {
    let dir = tempfile::tempdir().unwrap();
    let (cert_pem, key_pem) = openssl_make_cert(dir.path());
    let cert = smime::parse_cert(&std::fs::read(&cert_pem).unwrap()).unwrap();

    let ciphertext = smime::encrypt(std::slice::from_ref(&cert), PLAINTEXT).unwrap();
    let enc_der = write(dir.path(), "enc.der", &ciphertext);

    run(
        "openssl",
        &[
            "smime",
            "-decrypt",
            "-inform",
            "DER",
            "-in",
            enc_der.to_str().unwrap(),
            "-inkey",
            key_pem.to_str().unwrap(),
            "-recip",
            cert_pem.to_str().unwrap(),
            "-out",
            "decrypted.txt",
        ],
        dir.path(),
    );
    let decrypted = std::fs::read(dir.path().join("decrypted.txt")).unwrap();
    assert_eq!(decrypted, PLAINTEXT);
}

/// Direction C: kestrel verifies an openssl CLI CMS signature.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn kestrel_verifies_openssl_cli_signature() {
    let dir = tempfile::tempdir().unwrap();
    let (cert_pem, key_pem) = openssl_make_cert(dir.path());
    let cert = smime::parse_cert(&std::fs::read(&cert_pem).unwrap()).unwrap();

    let doc = write(dir.path(), "doc.txt", PLAINTEXT);
    run(
        "openssl",
        &[
            "smime",
            "-sign",
            "-outform",
            "DER",
            "-in",
            doc.to_str().unwrap(),
            "-signer",
            cert_pem.to_str().unwrap(),
            "-inkey",
            key_pem.to_str().unwrap(),
            "-out",
            "cli-sig.der",
        ],
        dir.path(),
    );
    let sig = std::fs::read(dir.path().join("cli-sig.der")).unwrap();
    smime::verify(&cert, PLAINTEXT, &sig).expect("openssl signature must verify in kestrel");
}

/// Direction D: kestrel decrypts an openssl CLI CMS envelope.
#[test]
#[ignore = "requires gpg + openssl CLIs (run by the cli-interop CI job)"]
fn kestrel_decrypts_openssl_cli_ciphertext() {
    let dir = tempfile::tempdir().unwrap();
    let (cert_pem, key_pem) = openssl_make_cert(dir.path());
    let cert = smime::parse_cert(&std::fs::read(&cert_pem).unwrap()).unwrap();
    let key = read_key(&key_pem);

    let doc = write(dir.path(), "doc.txt", PLAINTEXT);
    run(
        "openssl",
        &[
            "smime",
            "-encrypt",
            "-outform",
            "DER",
            "-in",
            doc.to_str().unwrap(),
            "-out",
            "cli-enc.der",
            cert_pem.to_str().unwrap(),
        ],
        dir.path(),
    );
    let ciphertext = std::fs::read(dir.path().join("cli-enc.der")).unwrap();
    let plaintext = smime::decrypt(&cert, &key, &ciphertext).unwrap();
    assert_eq!(plaintext, PLAINTEXT);
}
