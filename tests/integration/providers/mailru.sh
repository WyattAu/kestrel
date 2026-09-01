#!/usr/bin/env bash
# Test Mail.ru IMAP connection
# Usage: ./tests/integration/providers/mailru.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=mailru
export KESTREL_PROVIDER_IMAP_HOST=imap.mail.ru
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
