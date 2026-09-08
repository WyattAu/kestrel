# Release checklist (v0.1.0)

Runbook for cutting a release. The `release.yml` pipeline is proven
end-to-end; this document is the human process around it. Lessons below
come from the `v0.1.0-rc.1` dry-run series (six runs, six real defects).

## 1. Preflight (main must be fully green)

- [ ] `ci.yml` green on the release commit — all 19 jobs, including
      `tls-backend` (ADR 0016) and `sqlx` gates.
- [ ] `release.yml` has run successfully at least once since the last
      workflow change (a workflow edit you have not exercised is a
      release-day risk).
- [ ] `cargo nextest run --workspace` green locally; integration profile
      (`--profile integration`, Docker) for anything touching
      IMAP/JMAP/CalDAV/CardDAV flows.
- [ ] `cargo doc --workspace --no-deps` — broken intra-doc links fail CI.

## 2. Version + tag

- [ ] Bump `version` in workspace `Cargo.toml`; commit `Cargo.lock`
      (release builds use `--locked`).
- [ ] Annotated tag: `git tag -a vX.Y.Z -m "Kestrel vX.Y.Z"` and push it.
- [ ] Create the **GitHub release** (`gh release create vX.Y.Z ...`) —
      this, not the tag push, is what triggers `release.yml` (it listens
      on `release: created`). The release must point at the tag; if the
      tag moved, recreate the release object (`target_commitish` is
      pinned at creation and cannot be edited to a different commit).

## 3. What the pipeline does (per leg)

- x86_64-linux + aarch64-linux (TUI-only, cross) + macOS x64/arm64 +
  Windows x64; vendored OpenSSL on legs without a target-arch toolchain
  (ADR 0012 crypto backend; transport TLS is rustls per ADR 0016).
- Windows builds OpenSSL via Strawberry perl (`OPENSSL_SRC_PERL`).
- **Relocated-binary smoke test** runs on every leg that can execute its
  target: fresh `HOME`, engine boot via `kestrel-tui --help`, before any
  asset upload. A binary that references the build tree at runtime
  (the `CARGO_MANIFEST_DIR` migration-path bug) fails here, not in the
  field.
- SBOM (`sbom.yml`) attaches `kestrel-sbom.json` (CycloneDX).

## 4. Artifact verification (before announcing)

- [ ] Download **all** assets; `tar -tzf` / `zipinfo` each archive and
      confirm the expected binaries (both frontends on host-arch legs,
      TUI-only on aarch64-linux).
- [ ] Validate the SBOM: `bomFormat: CycloneDX`, component count sane.
- [ ] `sha256sum` every asset; publish the checksum list with the release.
- [ ] **Relocated smoke test on non-CI hardware** (this is the step that
      would have caught the rc.1 P0): copy the archive to a scratch
      machine or container, fresh `HOME`, run
      `timeout 60 ./kestrel-tui --help` — must reach engine startup and
      exit 0, with no path from the build machine in the output.
- [ ] Build provenance: run `actions/attest-build-provenance` on the
      assets (or document why not); verify with
      `gh attestation verify <asset> -R WyattAu/kestrel`.

## 5. Notes + announcement

- [ ] Release notes: highlights, fixed regressions (issue refs), known
      gaps (gpg 2.4.4 AEAD interop — upstream Sequoia; mailkit reqwest
      default-features), platform support matrix incl. the aarch64-linux
      TUI-only caveat.
- [ ] Label the release `pre-release` until smoke tests pass on real
      hardware.
- [ ] Close the release-prep issue (#16) with the run link.

## 6. Rollback

A broken release is deleted, never patched in place: delete the release
object, delete the tag remotely and locally, fix forward on `main`, then
re-tag. Do not reuse a version number after publication.
