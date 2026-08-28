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

# sqlx offline metadata freshness
sqlx-check:
    cargo sqlx prepare --check --workspace

# Doc build (broken intra-doc links fail)
doc:
    cargo doc --workspace --no-deps

# Supply chain
audit:
    cargo audit
    cargo deny check
