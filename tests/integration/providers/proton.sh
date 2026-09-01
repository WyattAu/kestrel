#!/usr/bin/env bash
# Test Proton Mail (Bridge) IMAP connection
# Usage: ./tests/integration/providers/proton.sh
# Requires: Proton Mail Bridge running locally, KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=proton
export KESTREL_PROVIDER_IMAP_HOST=127.0.0.1
export KESTREL_PROVIDER_IMAP_PORT=1143
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
