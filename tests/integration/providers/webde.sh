#!/usr/bin/env bash
# Test WEB.DE Mail IMAP connection
# Usage: ./tests/integration/providers/webde.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=webde
export KESTREL_PROVIDER_IMAP_HOST=imap.web.de
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
