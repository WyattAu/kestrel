//! Benchmark: MIME parsing throughput for various message complexities.
//! Validates the parser meets the ingestion SLA (> 800 msgs/sec floor,
//! > 1,500 msgs/sec target).

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kestrel_core::mime::{MimeParser, StalwartParser};

fn bench_mime_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("mime_parse");

    let simple = simple_message();
    let multipart = multipart_message();
    let complex = complex_multipart();

    for (name, raw) in [
        ("simple_1k", &simple),
        ("multipart_4k", &multipart),
        ("complex_20k", &complex),
    ] {
        group.bench_with_input(BenchmarkId::from_parameter(name), raw, |b, data| {
            b.iter(|| {
                StalwartParser::parse(data.as_bytes()).unwrap();
            });
        });
    }

    group.finish();
}

fn simple_message() -> String {
    format!(
        "From: alice@example.com\r\nTo: bob@example.com\r\n\
         Subject: Simple message\r\nMessage-ID: <simple@test>\r\n\
         Date: Fri, 28 Aug 2026 10:00:00 +0000\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         {}",
        "Hello Bob,\nThis is a simple test message.\n\n\
         The quarterly budget report is attached for review.\n\
         Please let me know if you have questions.\n\n\
         Best regards,\nAlice"
    )
}

fn multipart_message() -> String {
    let boundary = "----=_bench_boundary";
    format!(
        "From: sender@example.com\r\nTo: rcpt@example.com\r\n\
         Subject: Multipart message\r\nMessage-ID: <multipart@test>\r\n\
         Date: Fri, 28 Aug 2026 10:00:00 +0000\r\n\
         Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n\
         --{boundary}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         This is the plaintext body.\nWith two lines.\n\
         --{boundary}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n\
         <!DOCTYPE html><html><body><p>This is the <b>HTML</b> body.</p>\
         <p>Second paragraph with <a href=\"http://example.com\">a link</a>.</p>\
         <img src=\"cid:img1@example.com\" alt=\"inline image\"></body></html>\
         --{boundary}--\r\n"
    )
}

fn complex_multipart() -> String {
    let outer = "----=_outer_boundary";
    let inner = "----=_inner_boundary";
    let mut long_body = String::with_capacity(200 * 64);
    for i in 0..200 {
        use std::fmt::Write as _;
        let _ = write!(
            long_body,
            "Line {i}: The quick brown fox jumps over the lazy dog. "
        );
    }
    format!(
        "From: complex@example.com\r\nTo: team@example.com\r\n\
         Cc: manager@example.com\r\nSubject: Complex multipart\r\n\
         Message-ID: <complex@test>\r\n\
         Date: Fri, 28 Aug 2026 10:00:00 +0000\r\n\
         X-Custom-Header: value\r\n\
         Content-Type: multipart/mixed; boundary=\"{outer}\"\r\n\r\n\
         --{outer}\r\n\
         Content-Type: multipart/alternative; boundary=\"{inner}\"\r\n\r\n\
         --{inner}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Transfer-Encoding: quoted-printable\r\n\r\n\
         {long_body}\r\n\
         --{inner}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n\
         <html><body><pre>{long_body}</pre></body></html>\r\n\
         --{inner}--\r\n\
         --{outer}\r\n\
         Content-Type: text/plain; name=\"readme.txt\"\r\n\
         Content-Disposition: attachment; filename=\"readme.txt\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n\
        VGhpcyBpcyBhIHRlc3QgYXR0YWNobWVudCBmaWxlIHdpdGggc29tZSBjb250ZW50Lg==\r\n\
         --{outer}--\r\n"
    )
}

criterion_group!(benches, bench_mime_parse);
criterion_main!(benches);
