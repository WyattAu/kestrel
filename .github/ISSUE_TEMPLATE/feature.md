name: Feature
description: A unit of user- or engineer-facing work
labels: ["feature"]
body:
  - type: dropdown
    id: phase
    attributes:
      label: Phase
      options:
        - "1 — Core storage & parsing"
        - "2 — Sync engine"
        - "3 — TUI MVP"
        - "4 — GUI MVP"
        - "5 — Hardening"
        - Cross-phase
    validations:
      required: true
  - type: textarea
    id: problem
    attributes:
      label: Problem / motivation
      description: What cannot be done today; link requirements.md section if applicable
    validations:
      required: true
  - type: textarea
    id: proposal
    attributes:
      label: Proposed approach
      description: Note required ADRs, protocol/doc impact, threat-model surface
  - type: textarea
    id: acceptance
    attributes:
      label: Acceptance criteria
      description: Testable statements; note benchmarks (engineering-standards §5) if a hot path is touched
    validations:
      required: true
