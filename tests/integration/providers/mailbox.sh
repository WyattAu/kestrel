#!/usr/bin/env bash
# Test Mailbox.org IMAP connection
# Usage: ./tests/integration/providers/mailbox.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=mailbox
export KESTREL_PROVIDER_IMAP_HOST=imap.mailbox.org
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
