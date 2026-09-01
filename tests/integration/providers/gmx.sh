#!/usr/bin/env bash
# Test GMX Mail IMAP connection
# Usage: ./tests/integration/providers/gmx.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=gmx
export KESTREL_PROVIDER_IMAP_HOST=imap.gmx.net
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
