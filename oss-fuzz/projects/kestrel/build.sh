#!/usr/bin/env bash
# Builds the cargo-fuzz targets the way OSS-Fuzz builds Rust projects:
# $SRC/kestrel (this repo) + $OUT for artifacts, honoring the standard
# env vars so the same script works locally and in the OSS-Fuzz infra
# (issue #14). Falls back to sensible defaults outside OSS-Fuzz.
set -euo pipefail

SRC_DIR="${SRC:-$(pwd)/..}"
OUT_DIR="${OUT:-$(pwd)/../out}"
REPO_DIR="$SRC_DIR/kestrel"

cd "$REPO_DIR/fuzz"

# OSS-Fuzz Rust images default to nightly; keep it pinned here so local
# runs behave the same. RUSTFLAGS is managed by cargo-fuzz (sanitizer
# coverage flags); drop any repo-level overrides.
export RUSTFLAGS=""
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-nightly}"

# The fuzz graph pulls imap-next -> native-tls -> openssl-sys; OSS-Fuzz
# images ship libssl-dev, so the GNU target links system openssl.
TARGET="${FUZZ_TARGET_ARCH:-x86_64-unknown-linux-gnu}"

TARGETS=(fuzz_mime_adapter fuzz_link_classifier fuzz_html_sanitizer fuzz_terminal_sanitizer fuzz_imap_response)

for target in "${TARGETS[@]}"; do
  cargo fuzz build -O --sanitizer address --target "$TARGET" "$target"
  # cargo-fuzz lays artifacts out as target/<target>/release/<target-name>
  cp "target/$TARGET/release/$target" "$OUT_DIR/$target"
done

# Seed corpora: zip each target's seeds into $OUT the way OSS-Fuzz expects
# (<target>_seed_corpus.zip). fuzz/corpus/ keeps one dir per target.
for target in "${TARGETS[@]}"; do
  seed_dir="$REPO_DIR/fuzz/corpus/$target"
  if [ -d "$seed_dir" ]; then
    (cd "$seed_dir" && zip -q -r "$OUT_DIR/${target}_seed_corpus.zip" .)
  fi
done

echo "OSS-Fuzz artifacts written to $OUT_DIR:"
ls -la "$OUT_DIR"
