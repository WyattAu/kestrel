name: Bug
description: Something is broken
labels: ["bug"]
body:
  - type: textarea
    id: what
    attributes:
      label: What happened vs. what was expected
    validations:
      required: true
  - type: textarea
    id: repro
    attributes:
      label: Steps to reproduce
      description: Include sample email (sanitized!) or fuzz input where possible
    validations:
      required: true
  - type: input
    id: version
    attributes:
      label: Version / commit
    validations:
      required: true
  - type: textarea
    id: logs
    attributes:
      label: Relevant logs (scrubbed — no addresses/subjects/tokens)
  - type: checkboxes
    id: class
    attributes:
      label: Class
      options:
        - label: Security relevant (follow threat-model disclosure rules — do not attach hostile payloads publicly)
        - label: Performance regression (attach benchmark numbers)
