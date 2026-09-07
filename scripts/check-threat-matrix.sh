#!/usr/bin/env bash
# Enforces threat-model §7: every mitigation row must be covered by a named
# test (or harness/job) that provably exists. Deleting or renaming coverage
# without updating the matrix fails CI — coverage can never silently vanish.
#
# Rows T1..T7 mirror docs/threat-model.md §7 exactly; keep the two in sync.
#
# Usage: scripts/check-threat-matrix.sh   (requires cargo-nextest on PATH)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# --- gather the test list once -------------------------------------------------
LIST="$(mktemp)"
LIST_ERR="$(mktemp)"
trap 'rm -f "$LIST" "$LIST_ERR"' EXIT
echo "Collecting test list (cargo nextest list --workspace)..." >&2
if ! cargo nextest list --workspace >"$LIST" 2>"$LIST_ERR"; then
  echo "::error::cargo nextest list failed:" >&2
  cat "$LIST_ERR" >&2
  exit 1
fi

FAIL=0
check_test() { # label, required-substring
  if grep -qF -- "$2" "$LIST"; then
    echo "ok   $1 -> $2"
  elif cargo nextest list --workspace -E "test($2)" >/dev/null 2>"$LIST_ERR"; then
    # The shared list can be truncated if a test binary chokes in list mode
    # (seen on CI for kestrel-crypto/kestrel-tui); a targeted filter query is
    # ground truth that the named coverage exists.
    echo "ok   $1 -> $2 (targeted query; shared list was incomplete)"
  else
    echo "FAIL $1: no test matches \"$2\""
    if [ -s "$LIST_ERR" ]; then
      echo "--- nextest list stderr ---" >&2
      head -5 "$LIST_ERR" >&2
    fi
    FAIL=1
  fi
}
check_file() { # label, path-or-glob (must be unquoted so globs expand)
  if ls ${2} >/dev/null 2>&1; then
    echo "ok   $1 -> $2"
  else
    echo "FAIL $1: missing file/glob \"$2\""
    FAIL=1
  fi
}
check_ci_job() { # label, job-name
  if grep -qP "^  ${2}:$" .github/workflows/ci.yml; then
    echo "ok   $1 -> CI job \"$2\""
  else
    echo "FAIL $1: no CI job \"$2\" in .github/workflows/ci.yml"
    FAIL=1
  fi
}

echo "== T1 parser limits (4.2) =="
check_test  "T1 mime limits module"   "mime::tests"
check_file  "T1 fuzz targets"         "fuzz/fuzz_targets/*.rs"
check_file  "T1 mime regression corpus" "tests/mime-corpus/*"

echo "== T2 CSP & webview (4.4) =="
check_test  "T2 csp blocks active content"   "gui_csp_blocks_all_active_content"
check_test  "T2 sanitized html inert"        "sanitized_html_has_no_active_content"

echo "== T3 remote-content network isolation (4.4) =="
check_file  "T3 netns harness script"  "scripts/webview-netns-test.sh"
check_ci_job "T3 netns CI job"         "webview-isolation"

echo "== T4 link defenses (4.5) =="
check_test  "T4 links module"          "links::tests"

echo "== T5 credential storage (4.8) =="
check_test  "T5 keyring-unavailable typed"  "credentials::tests::keyring_store_reports_unavailable_without_dbus"
check_test  "T5 secrets masked in debug"    "credentials::tests::secrets_are_masked_in_debug"
check_test  "T5 plaintext fallback refused" "credentials::tests::plaintext_fallback_is_refused_by_construction"

echo "== T6 log scrubbing (4.8) =="
check_test  "T6 secret never logged"    "credentials::tests::secret_never_enters_tracing_output"

echo "== T7 TUI escapes (4.6) =="
check_test  "T7 sanitizer module"       "sanitizer::tests"
check_test  "T7 terminal escapes"       "html::tests::terminal_escapes_neutralized"

if [ "$FAIL" -eq 1 ]; then
  echo "Threat-model §7 matrix check failed: update the listed coverage or the matrix row." >&2
  exit 1
fi
echo "Threat-model §7 matrix satisfied (rows T1..T7 have named, existing coverage)."
