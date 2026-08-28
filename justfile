# Common developer commands (see AGENTS.md / CONTRIBUTING.md).
default:
    @just --list

# Build everything
build:
    cargo build --workspace

# Run unit + local integration tests (docker tests excluded)
test:
    cargo nextest run --workspace

# Run docker-gated integration tests (Dovecot/Greenmail)
test-integration:
    cargo nextest run --profile integration

# Lint gates
lint:
    cargo clippy --workspace --all-targets -- -D warnings

fmt:
    cargo +nightly fmt --all

fmt-check:
    cargo +nightly fmt --all --check

# sqlx offline metadata freshness (builds the combined prepare DB first)
sqlx-check:
    #!/usr/bin/env bash
    set -euo pipefail
    export DATABASE_URL=$(./scripts/sqlx-prepare-db.sh | grep DATABASE_URL | cut -d= -f2)
    cargo sqlx prepare --check --workspace

# Doc build (broken intra-doc links fail)
doc:
    cargo doc --workspace --no-deps

# Supply chain
audit:
    cargo audit
    cargo deny check
