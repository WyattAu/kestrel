#!/usr/bin/env bash
# Test Yahoo Mail IMAP connection
# Usage: ./tests/integration/providers/yahoo.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=yahoo
export KESTREL_PROVIDER_IMAP_HOST=imap.mail.yahoo.com
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
