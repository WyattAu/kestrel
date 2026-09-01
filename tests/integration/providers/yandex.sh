#!/usr/bin/env bash
# Test Yandex Mail IMAP connection
# Usage: ./tests/integration/providers/yandex.sh
# Requires: KESTREL_PROVIDER_EMAIL and KESTREL_PROVIDER_PASSWORD env vars (use app password)
set -euo pipefail
export KESTREL_PROVIDER_INTEGRATION=1
export KESTREL_PROVIDER_NAME=yandex
export KESTREL_PROVIDER_IMAP_HOST=imap.yandex.com
export KESTREL_PROVIDER_IMAP_PORT=993
cargo nextest run --package kestrel-sync --test provider_real -- --ignored --nocapture
