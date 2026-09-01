//! Benchmark: `OpenPGP` key generation, sign, encrypt, decrypt operations.
//! Validates that Sequoia 2.x meets performance expectations for
//! interactive compose/sign/encrypt workflows.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use kestrel_core::secrets::SecretString;
use kestrel_crypto::openpgp;

fn bench_openpgp(c: &mut Criterion) {
    let mut group = c.benchmark_group("openpgp");

    group.bench_function("keygen_2048", |b| {
        b.iter(|| {
            openpgp::generate_cert("bench@kestrel.example", None).unwrap();
        });
    });

    let (cert, _) = openpgp::generate_cert("signer@kestrel.example", None).unwrap();
    let pw = SecretString::new(String::new());
    let payload_1k = vec![b'x'; 1024];
    let payload_64k = vec![b'x'; 65_536];

    group.bench_function("sign_1k", |b| {
        b.iter(|| {
            openpgp::sign(&cert, &pw, &payload_1k).unwrap();
        });
    });

    group.bench_function("sign_64k", |b| {
        b.iter(|| {
            openpgp::sign(&cert, &pw, &payload_64k).unwrap();
        });
    });

    let (recipient, _) = openpgp::generate_cert("recipient@kestrel.example", None).unwrap();

    group.bench_function("encrypt_1k_unsigned", |b| {
        b.iter(|| {
            openpgp::encrypt(std::slice::from_ref(&recipient), None, &payload_1k).unwrap();
        });
    });

    group.bench_function("encrypt_1k_signed", |b| {
        b.iter(|| {
            openpgp::encrypt(
                std::slice::from_ref(&recipient),
                Some((&cert, &pw)),
                &payload_1k,
            )
            .unwrap();
        });
    });

    let ciphertext = openpgp::encrypt(std::slice::from_ref(&recipient), None, &payload_1k).unwrap();

    group.bench_function("decrypt_1k_unsigned", |b| {
        b.iter(|| {
            openpgp::decrypt(&recipient, &pw, &ciphertext, &[]).unwrap();
        });
    });

    let signed_ciphertext = openpgp::encrypt(
        std::slice::from_ref(&recipient),
        Some((&cert, &pw)),
        &payload_1k,
    )
    .unwrap();

    group.bench_function("decrypt_1k_signed", |b| {
        b.iter(|| {
            openpgp::decrypt(
                &recipient,
                &pw,
                &signed_ciphertext,
                std::slice::from_ref(&cert),
            )
            .unwrap();
        });
    });

    group.finish();
}

criterion_group!(benches, bench_openpgp);
criterion_main!(benches);
