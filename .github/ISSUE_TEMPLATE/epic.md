name: Epic
description: Large body of work tracking child tasks (one per phase theme)
labels: ["epic"]
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
    id: outcome
    attributes:
      label: Outcome
      description: What is true when this epic closes (map to docs/roadmap.md exit criteria)
    validations:
      required: true
  - type: textarea
    id: children
    attributes:
      label: Child tasks
      description: Checklist of #-referenced issues
    validations:
      required: true
  - type: textarea
    id: docs
    attributes:
      label: Docs & ADRs touched
