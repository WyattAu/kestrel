# Kestrel Threat Model

Status: **v1.0** · Expands `requirements.md` §4.2 ("Zero-Trust Email") ·
Method: STRIDE-per-component over the architecture in `docs/architecture.md`.

---

## 1. Assets

| # | Asset | Loss/compromise impact |
|---|-------|------------------------|
| A1 | Mailbox contents (bodies, headers, attachments) | Confidentiality breach |
| A2 | Credentials (passwords, OAuth refresh tokens, GPG keys) | **Critical** — full account takeover |
| A3 | Local database + blob store integrity | Malicious code execution via crafted data paths |
| A4 | Outbox drafts (unsent, possibly sensitive) | Confidentiality + integrity |
| A5 | User attention (phishing targets) | Credential theft, fraud |
| A6 | Metadata (correspondents, subjects, timing) | Privacy/surveillance harm |
| A7 | Device resources (disk, CPU) | DoS (decompression/store bombs) |

## 2. Trust Boundaries

```
[Internet / mail server]      ← T1: untrusted network + untrusted server responses
      │
[Kestrel sync engine]         ← parses hostile bytes (T2)
      │
[Storage / index / CAS]       ← T3: hostile-derived data at rest
      │
[Frontend processes]          ← T4: UI consumes derived data
      │  └── wry webview (T5: executes email HTML, highest-risk component)
[OS: keyring, FS, notifications] ← T6: local attack surface
```

Attacker profiles: (a) network adversary (MITM, DNS), (b) malicious/compromised
mail server, (c) email sender (primary — anyone can send mail),
(d) local malicious process, (e) physical/local file access.

## 3. STRIDE Analysis

| Component | Spoofing | Tampering | Repudiation | Info disclosure | DoS | Elevation |
|-----------|----------|-----------|-------------|-----------------|-----|-----------|
| IMAP/SMTP/JMAP transport | M1 server spoof | M2 MITM tamper | — | M3 plaintext leak | M4 conn floods | — |
| SASL/OAuth2 | M5 token theft | — | — | M6 token in logs | — | — |
| MIME parser (ADR 0002) | — | M7 parse confusion | — | M8 hidden parts | **M9 decompression/store bombs, infinite nesting** | **M10 memory corruption** |
| SQLite/CAS | — | M11 symlink/hardlink swap (T6) | — | M12 cache readable | M13 db bloat | — |
| HTML webview (wry) | — | **M14 script/remote content** | — | **M15 tracker beacons**, M16 local file read | M17 render bomb | M18 JS bridge abuse |
| Link handling | **M19 homograph/punycode**, M20 display-text mismatch | — | — | — | — | — |
| TUI rendering | — | — | — | M21 OSC escape injection in bodies | M22 escape-sequence flood | — |
| Frontend IPC/protocol | — | M23 command forgery | — | — | M24 unbounded queue | — |
| Credential store | M25 keyring bypass | — | — | **M26 plaintext fallback** | — | — |

## 4. Mitigations (binding requirements → traceable tests)

### 4.1 Transport (M1–M4)
- TLS 1.3 default / 1.2 minimum via `rustls` (§2.1); certificate validation
  mandatory; no downgrade path to plaintext; STARTTLS only where mandated.
- SASL credentials injected from `kestrel-crypto`, never from config.
- Connection storms bounded: per-account backoff + jitter (ADR 0004);
  supervisor caps restart rate (DoS self-protection).

### 4.2 MIME parsing (M7–M10)
- `mail-parser` behind `MimeParser` trait (ADR 0002); **no panics on any
  input** — fuzzed continuously (engineering-standards §Fuzzing).
- Hard limits: nesting depth ≤ 64, single part ≤ 128 MiB, total decoded
  message ≤ 512 MiB, max header count/size; violations → typed
  `ParseError::Limit` (ADR 0007), message still listable.
- Decompression ratio cap (zip bombs): decoded-size/streamed-size ≤ 100×.
- `unsafe` banned in parser paths; Miri over fuzz corpus builds.

### 4.3 Storage & CAS (M11–M13)
- Blobs written via `O_NOFOLLOW` + atomic rename + hash verification
  (`docs/schema.md` §4.1); symlinked targets rejected.
- Per-account storage quota (configurable); cache.db wipe is a safe pressure
  valve (rebuildable by design).
- File permissions 0700 on data dirs; never world-readable.

### 4.4 HTML webview — highest risk (M14–M18)
- CSP injected on every load (§4.2 of requirements):
  `default-src 'none'; style-src 'unsafe-inline'; img-src cid: data:; script-src 'none';`
- JavaScript **disabled at engine level**, not by policy alone; no JS bridge
  is ever registered with `wry`.
- `file://` and all custom schemes denied except the engine's in-memory
  `kestrel-cid://` handler; handler serves **only** parts of the currently
  rendered message, with MIME-type allowlist.
- Remote content (images, fonts, CSS) blocked by default; per-sender
  allowlist is user policy; every block emits `RemoteContentBlocked`
  (message-protocol §3) so the UI can show a placeholder + "load remote"
  affordance. Blocking is by construction: the viewport simply has no network
  origin access.
- Webview process is isolated (wry uses the OS webview sandbox); Kestrel
  never passes engine handles into it — only serialized body payloads.

### 4.5 Phishing (M19–M20)
- On click: resolved `href` shown in confirmation for punycode/IDN
  homographs and for display-text/`href` mismatches
  (`SuspiciousLink` event).
- Mixed-script confusables detection on domains; explicit warning badge in
  both UIs.
- Links never auto-open; open via OS handler only after user gesture +
  checks.

### 4.6 TUI (M21–M22)
- All body text passed through an escape-sanitizer before rendering;
  control characters replaced; OSC sequences in mail bodies neutralized.
- Rendering work bounded per frame (windowed lists) — hostile data cannot
  stall the loop (architecture §3.2).

### 4.7 Protocol & IPC (M23–M24)
- In-process only; frontends hold only channel senders (ADR 0004). Bounded
  channels everywhere (message-protocol §4) make queue flooding impossible.

### 4.8 Credentials (M5–M6, M25–M26)
- OS keyring mandatory where available (`keyring` crate); GPG-encrypted file
  fallback with 0600 perms; **plaintext fallback is refused**, not warned
  about.
- Tokens/passwords: never in SQLite, never in config files, never in logs
  (ADR 0008 scrub rules; log assertions tested).
- OAuth loopback server binds `127.0.0.1` only, ephemeral port, single-use
  `state`, PKCE (RFC 7636); shuts down after exchange.
- Memory hygiene: credentials held in `zeroize`-wrapping types; zeroized on
  drop.

## 5. `kestrel-cid://` Protocol Specification (normative)

- Scope: the **single** message currently loaded in the viewport.
- URL form: `kestrel-cid://part/<PartId>` and `kestrel-cid://raw/<PartId>`.
- Handler: stateful per-viewport instance; drops all parts on navigation;
  responses limited to 128 MiB and allowlisted MIME types
  (`image/*`, `text/plain`); returns 404 for everything else.
- No path traversal surface: PartIds are engine-issued opaque ids.

## 6. Privacy Requirements (A6)

- Logs: no headers/addresses/bodies above `debug` without explicit opt-in
  flag (ADR 0008).
- Notifications: subject line optional (config), sender-only mode default on.
- Telemetry: **none**. No crash reporting without explicit future ADR.

## 7. Security Test Matrix (maps to engineering-standards CI)

| Mitigation set | Verification |
|----------------|--------------|
| Parser limits (4.2) | cargo-fuzz corpora + regression corpus in `tests/mime-corpus/` |
| CSP & webview (4.4) | GUI integration test asserting CSP header on every load; attempted `file://`/script loads fail closed |
| Remote content (4.4) | Integration test: no network syscalls from viewport process during render (network namespace sandbox in CI) |
| Link defenses (4.5) | Unit table: punycode/confusable/href-mismatch cases must trigger confirmation |
| Credential storage (4.8) | Unit tests: no plaintext bytes on disk (scan test); keyring fallback matrix |
| Log scrubbing (4.8) | Log-capture tests asserting token/address absence |
| TUI escapes (4.6) | Sanitizer property tests + corpus of hostile sequences |
