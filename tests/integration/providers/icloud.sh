#!/usr/bin/env bash
# Test iCloud Mail IMAP connection
# Usage: ./tests/integration/providers/icloud.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars (use app-specific password)
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=icloud
export KESTREL_PROVIDER_IMAP_HOST=imap.mail.me.com
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
