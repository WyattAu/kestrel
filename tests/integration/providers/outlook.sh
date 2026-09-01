#!/usr/bin/env bash
# Test Outlook / Microsoft 365 IMAP connection
# Usage: ./tests/integration/providers/outlook.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=outlook
export KESTREL_PROVIDER_IMAP_HOST=outlook.office365.com
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
