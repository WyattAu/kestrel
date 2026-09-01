# ADR 0013: Mobile Client — Pure Rust with Slint Mobile

- **Status:** Proposed
- **Date:** 2026-08-31
- **Deciders:** Kestrel team

## Context

Kestrel needs mobile clients (iOS/Android) to reach users on mobile devices.
The core engine is already separated from UI via the message protocol (ADR 0004).
Pure Rust maximizes code sharing with desktop — the same `kestrel-engine` crate
runs in-process on mobile just as it does on desktop.

## Decision

Use **Slint mobile** for the UI framework, with a Rust FFI bridge for platform
integration (push notifications, background execution, credential storage).
Reuse `kestrel-engine` directly in the mobile app process, following the same
assembly pattern established by `kestrel-gui` and `kestrel-tui` (ADR 0011).

Mobile-specific concerns are isolated in a dedicated `kestrel-mobile` crate:

- **Engine adapter** — configures storage quotas, cache limits, and background
  sync intervals appropriate for mobile constraints.
- **Background tasks** — stubs for sync, outbox flush, snooze checks, and
  filter evaluation that will be dispatched via platform background APIs.
- **Push notifications** — stubs for APNs (iOS) and FCM (Android) token
  management and notification routing.

## Consequences

- Single UI framework (Slint) across desktop and mobile; `.slint` files are
  shared where possible.
- Rust business logic shared via `kestrel-engine` — no duplication.
- Platform-specific code is isolated: push notifications (APNs/FCM), background
  execution (iOS BGTaskScheduler / Android WorkManager), credential storage
  (iOS Keychain / Android Keystore).
- `FrontendKind::Mobile` added to the protocol so the engine can identify
  mobile-originated commands.
- Slint mobile is experimental; platform-specific workarounds may be needed.
- Mobile crates depend on `kestrel-engine` (frontend rule from ADR 0011); code
  beyond the entry point uses only `kestrel-core` types.
