# Security Policy

## Reporting a vulnerability

Do **not** open a public issue for security problems.

Email the maintainers directly (see repository owner profile / `CODEOWNERS`).
Include: affected component, reproduction details, sanitized samples. You will
receive an acknowledgement within 72 hours.

- Please do not include live credentials or private mailbox data in reports.
- Hostile email samples (MIME corpora, link payloads) may be shared privately;
  never attached to public issues (see `docs/threat-model.md` §7).

## Scope

All shipped crates (`kestrel-core`, `-sync`, `-storage`, `-crypto`, `-tui`,
`-gui`) and the release artifacts. The threat model and required mitigations
live in `docs/threat-model.md`; missing or broken mitigations there are
security bugs.

## Handling

1. Triage within 72 h; severity per threat-model asset table.
2. Fix on a private branch; regression test added (named after the report).
3. Coordinated disclosure: advisory published with the patch release.
