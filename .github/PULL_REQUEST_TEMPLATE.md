## Summary

<!-- What changed and why. Reference the issue: Closes #N. -->

## Decision trail

<!-- If this touches a recorded decision: ADR added/updated? Which one? -->

- [ ] No accepted ADR contradicted (or superseding ADR included in this PR)

## Verification

- [ ] `cargo +nightly fmt --all --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo nextest run --workspace` green
- [ ] `cargo sqlx prepare --check` clean (queries/migrations changed)
- [ ] Tests added with new logic; regression test named after issue for fixes

## Impact checks

- [ ] Message protocol unchanged, or change follows ADR process (2 reviews)
- [ ] `docs/threat-model.md` updated if attack surface changed
- [ ] `docs/` updated in this PR where behavior is described there
- [ ] Benchmarks attached if a hot path changed (engineering-standards §5)
- [ ] New dependencies listed here with justification (standards §6)
