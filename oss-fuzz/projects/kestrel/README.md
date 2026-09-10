# OSS-Fuzz integration (issue #14)

This directory holds the OSS-Fuzz project config for kestrel, ready to be
filed upstream: copy `projects/kestrel/` into a checkout of
<https://github.com/google/oss-fuzz> and open a PR there. Until the upstream
PR merges, the weekly corpus-regression run (`.github/workflows/fuzz-weekly.yml`)
guards the same five targets in this repo's CI.

## Files

- `projects/kestrel/project.yaml` — engine/sanitizer/format declaration
  (libFuzzer + ASan, Rust).
- `projects/kestrel/build.sh` — builds the five cargo-fuzz targets the way
  OSS-Fuzz expects (`$SRC`/`$OUT` contract), zips seed corpora from
  `fuzz/corpus/` into `$OUT/<target>_seed_corpus.zip`, and honors
  `FUZZ_TARGET_ARCH` for future aarch64 coverage.

## Fuzz targets

| Target | Surface |
|--------|---------|
| `fuzz_mime_adapter` | `kestrel-core` MIME parser adapter (ADR 0002) |
| `fuzz_html_sanitizer` | HTML sanitizer (viewport render path) |
| `fuzz_terminal_sanitizer` | HTML→terminal transpiler (TUI render path) |
| `fuzz_link_classifier` | phishing link classifier (display/href mismatch, punycode) |
| `fuzz_imap_response` | IMAP response decoder (ADR 0005) |

## Regression protocol

Any crash found by OSS-Fuzz lands in this repo as
`tests/mime-corpus/regression-<issue>.eml` (or the matching corpus dir) plus
a named regression test, per `tests/mime-corpus/README.md`. The weekly
workflow then replays the corpus against every target before the deep fuzz.
