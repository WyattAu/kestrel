#!/usr/bin/env bash
# ADR 0016 enforcement: rustls is the sole transport-TLS backend.
#
# 1. reqwest::Client::new() is banned in workspace source: once cargo
#    unifies reqwest features across the workspace (they currently include
#    native-tls via mailkit's ungated `resend` feature), Client::new()
#    silently selects native-tls.
# 2. Every reqwest::Client::builder() site must pin .use_rustls_tls().
#
# The two documented openssl-sys exceptions (Sequoia crypto-openssl,
# mailkit transitive reqwest) live in the dependency tree, not in call
# sites, so they are unaffected by this scan.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

# 1. No Client::new() in workspace source (tests included: policy is
#    uniform and loopback tests are not exempt).
while IFS= read -r hit; do
  echo "FAIL: reqwest::Client::new() (native-tls under unification) — ADR 0016"
  echo "      ${hit}"
  fail=1
done < <(grep -rn "reqwest::Client::new()" crates/ --include='*.rs' || true)

# 2. Every Client::builder() file must pin the rustls connector.
while IFS= read -r file; do
  if ! grep -q "use_rustls_tls" "${file}"; then
    echo "FAIL: reqwest::Client::builder() without .use_rustls_tls() — ADR 0016"
    echo "      ${file}"
    fail=1
  fi
done < <(grep -rl "reqwest::Client::builder()" crates/ --include='*.rs' || true)

if [[ "${fail}" -ne 0 ]]; then
  echo
  echo "See docs/adr/0016-rustls-only-transport-tls.md for the policy and"
  echo "the two documented openssl-sys exceptions."
  exit 1
fi
echo "tls-backend check: OK (rustls-only, ADR 0016)"
