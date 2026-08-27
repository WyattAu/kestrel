# Kestrel: Technical Requirements Specification (v1.0)

This document establishes the architectural, protocol, security, storage, and interface requirements for **Kestrel**—a high-performance, modular email client written in Rust that powers both a Terminal UI (`kestrel-tui`) and a Native Desktop GUI (`kestrel-gui`).

---

## 1. System & Workspace Architecture

```
kestrel/
├── crates/
│   ├── kestrel-core/       # Domain types, config, error taxonomy, traits
│   ├── kestrel-sync/       # IMAP/JMAP/SMTP network engines & state machines
│   ├── kestrel-storage/    # SQLite metadata, Tantivy indexing, blob storage
│   ├── kestrel-crypto/     # Credential storage, GPG/OpenPGP, TLS configuration
│   ├── kestrel-tui/        # Ratatui frontend
│   └── kestrel-gui/        # Native desktop shell + embedded Wry viewport
├── Cargo.toml
└── README.md
```

### 1.1 Concurrency & Execution Model

- **Async Runtime:** The core engine MUST run on `tokio` (multi-threaded scheduler).
- **Decoupled Architecture:** Frontends MUST communicate with the core engine via typed async message-passing (`tokio::sync::mpsc` channels or an event-driven actor model).
- **Non-Blocking Guarantee:** Network synchronization, full-text indexing, and disk I/O MUST NEVER block UI rendering threads (TUI event loop or GUI render frame).

---

## 2. Protocol, Network & Sync Requirements

### 2.1 Supported Standards

- **IMAP4rev1 / IMAP4rev2:** RFC 3501, RFC 9051.
    - Extensions required: `IDLE` (RFC 2177), `CONDSTORE` / `QRESYNC` (RFC 4551/7162) for fast delta syncing, `UIDPLUS` (RFC 4315), `NAMESPACE` (RFC 2342), `MOVE` (RFC 6851).
- **JMAP:** RFC 8620 (Core) and RFC 8621 (Mail).
- **SMTP:** RFC 5321, with `STARTTLS` (RFC 3207) and `AUTH` (RFC 4954).
- **TLS:** Enforce TLS 1.3 as default, TLS 1.2 minimum (via `rustls`). Direct SSL/TLS on port 993/465 and STARTTLS on 587/143.

### 2.2 Sync Engine State Machine

```
[ Disconnected ] <---> [ Connecting / Handshake ]
         |
         v
[ Authenticating (OAuth2/SASL) ]
         |
         v
[ Syncing Folder Hierarchy (LIST) ]
         |
         v
[ Delta Sync (UID FETCH / HIGHESTMODSEQ) ]
         |
         +---> [ Fetch Envelopes/Headers ] ---> [ SQLite & Tantivy Index ]
         |
         +---> [ Lazy / Background Blob Fetch ] ---> [ CAS Blob Store ]
         |
         v
[ IDLE Loop (Wait for Push Notifications) ]
```

- **Offline-First Operation:** All metadata, envelope trees, and downloaded bodies MUST be fully queryable and navigable when `Offline`.
- **Outbox Queue:** Emails composed while offline MUST be stored in an outbox SQLite table and automatically flushed with exponential backoff when a connection is re-established.
- **UID Validity Handling:** The engine MUST detect `UIDVALIDITY` changes on IMAP mailboxes and trigger an automated reconciliation/cache purge for that folder.

### 2.3 Authentication & Credentials

- **SASL Mechanisms:** `PLAIN`, `LOGIN`, `SCRAM-SHA-256`, and `XOAUTH2`.
- **OAuth2 / PKCE (RFC 7636, RFC 8252):**
    - Automated OAuth2 loopback server (`http://127.0.0.1:<port>`) for browser-based sign-in (Google Workspace, Microsoft 365, Fastmail).
    - Automated token refresh background task.
- **Credential Security:** Passwords and OAuth refresh tokens MUST NOT be stored in plaintext. They MUST use the OS secret store via `keyring` (Secret Service API on Linux, Keychain on macOS, Credential Manager on Windows) or a GPG-encrypted file.

---

## 3. Storage, Caching & Search Engine

### 3.1 Metadata & State Storage (SQLite)

- Database connection MUST run with `PRAGMA journal_mode = WAL;`, `PRAGMA synchronous = NORMAL;`, and `PRAGMA foreign_keys = ON;`.
- **Core Schema Entities:**
    - `accounts` (ID, name, email, provider, sync\_state)
    - `folders` (ID, account\_id, remote\_name, attributes, delimiter, uid\_validity, highest\_modseq)
    - `messages` (ID, folder\_id, uid, internal\_date, flags, message\_id, in\_reply\_to, subject, from\_addr, to\_addrs, cc\_addrs, size, is\_read, has\_attachments, thread\_id)
    - `parts` (ID, message\_id, mime\_type, content\_id, disposition, encoding, byte\_size, blob\_sha256)
    - `outbox` (ID, account\_id, raw\_rfc822\_blob, retry\_count, last\_error, created\_at)

### 3.2 Raw Body & Attachment Storage (Content-Addressed Storage)

- Large MIME bodies and attachments MUST NOT be stored as inline blobs in SQLite to prevent database bloat.
- Files MUST be stored on disk using **SHA-256 content-addressing**:
    - Path: `$XDG_DATA_HOME/kestrel/blobs/ab/cd/<sha256_hash>`

### 3.3 Full-Text Search (Tantivy)

- Search engine MUST be powered by **Tantivy** (embedded Rust search engine).
- **Indexed Fields:**
    - `subject` (Indexed, Tokenized with English/standard stemmer)
    - `body_plain` (Indexed, Tokenized, Stored: false)
    - `from` / `to` / `cc` / `bcc` (Indexed, Raw/Exact match + Tokenized)
    - `attachment_names` (Indexed, Tokenized)
    - `date` (Fast field for range filtering)
    - `folder_id` / `account_id` (Facet/Filtering fields)
- **Performance Requirement:** Search queries across **500,000 indexed messages** must return the first 50 results in **under 30 milliseconds**.

---

## 4. MIME Parsing, Rendering & Security

### 4.1 MIME Parsing & Character Sets

- Must parse complex nested `multipart/alternative`, `multipart/mixed`, `multipart/related`, and `message/rfc822` (via `mailparse` or custom zero-copy nom/winnow parser).
- **Legacy Encoding Support:** Robust transcoding of non-UTF8 charsets (ISO-8859-1..15, Windows-1252, Shift-JIS, GB2312) to UTF-8 using `encoding_rs`.
- **Malformed Header Handling:** Un-escaped strings, missing semicolons in MIME headers, and broken RFC 2047 encoded words MUST fail gracefully without crashing.

### 4.2 Security & Sandboxing (The "Zero-Trust Email" Policy)

- **Remote Content Blocking:** External images, web fonts, and stylesheets MUST be blocked by default to prevent tracker beacons.
- **Content Security Policy (CSP):** The HTML viewport MUST inject strict headers:
```http
default-src 'none'; style-src 'unsafe-inline'; img-src cid: data:; script-src 'none';
```
- **JavaScript Execution:** JavaScript MUST be completely disabled in the rendering engine.
- **Phishing Defenses:**
    - Visual indicator if the display text of a hyperlink differs from its actual `href` target.
    - Explicit confirmation prompt when clicking links containing punycode/IDN homograph characters.

---

## 5. Interface Requirements: Terminal UI (`kestrel-tui`)

- **Framework:** `ratatui` + `crossterm`.
- **Display Layout:**
    - 3-Pane Layout: Folders/Accounts (left), Thread/Message List (center), Preview/Reader (right/bottom).
    - 1-Pane Focus Mode (Maximizes current view for low-width terminals).
- **Email Rendering in TUI:**
    - `text/plain`: Direct rendering with syntax highlighting for diffs, blockquotes, and Markdown.
    - `text/html`: Transpiled on-the-fly to formatted terminal text (with ANSI formatting for bold/italics/lists) using a Rust-native HTML-to-text converter.
    - Hyperlink support via **OSC 8** terminal escape sequences.
- **Composition:**
    - Must spawn external `$EDITOR` (Neovim, Helix, Vim, Nano) via a suspended terminal process.
    - Support writing in Markdown and automatically generating a clean `multipart/alternative` payload (Plaintext + basic HTML) upon sending.
- **Navigation:** Vi-style keybindings (`j`/`k` navigation, `d` archive/delete, `r` reply, `a` reply-all, `f` forward, `/` fuzzy search).

---

## 6. Interface Requirements: Native GUI (`kestrel-gui`)

- **App Shell Framework:** **Slint** or **Iced** (pure native UI for navigation panels, sidebars, settings, and message lists).
- **HTML Body Viewport:** **`wry`** (Tauri's cross-platform webview layer).
    - The webview MUST ONLY render the email body payload inside an isolated, sandboxed frame.
    - Local filesystem access (`file://`) MUST be explicitly denied inside the webview.
    - Inline attachments (`cid:...`) MUST be served via a custom in-memory URI protocol handler.
- **Composition in GUI:**
    - Dual mode: Clean Markdown editor with live HTML preview OR minimal WYSIWYG editor.
    - Drag-and-drop attachment handling.
- **OS Integration:** Native system tray icon, OS desktop notifications (via `notify-rust`), and system theme detection (Dark/Light mode).

---

## 7. Configuration & Platform Compliance

- **Config Standard:** Follow the **XDG Base Directory Specification**:
    - Config: `$XDG_CONFIG_HOME/kestrel/config.toml`
    - Cache / SQLite: `$XDG_CACHE_HOME/kestrel/cache.db`
    - Blobs & Indices: `$XDG_DATA_HOME/kestrel/`
- **Configuration Format:** Human-readable `TOML`. All configuration values (keybindings, theme colors, sync intervals, accounts) must be live-reloadable where feasible.

---

## 8. Hard Performance Budgets (SLAs)

Metric

Target

Hard Upper Limit

**Cold Start to Interactive (TUI)**

\< 50 ms

150 ms

**Cold Start to Interactive (GUI)**

\< 200 ms

500 ms

**Idle Memory (TUI)**

\< 25 MB

40 MB

**Idle Memory (GUI with 1 Webview)**

\< 120 MB

200 MB

**Message List Scroll Rate**

Steady 60 FPS / 120 FPS

No frame drops

**Envelope Ingestion Rate**

\> 1,500 msgs/sec

\> 800 msgs/sec

**Search Query Latency (100k msgs)**

\< 15 ms

50 ms

---

## 9. Implementation Roadmap & Milestones

- **Phase 1 (Core Storage & Parsing):** `kestrel-core`, SQLite schema, MIME parser, Tantivy indexing pipeline.
- **Phase 2 (Sync Engine):** IMAP `FETCH`/`IDLE`/`STORE`, SMTP sender, OAuth2 loopback, background queue.
- **Phase 3 (TUI MVP):** `kestrel-tui` built on `ratatui`, `$EDITOR` integration, keyboard-driven navigation.
- **Phase 4 (GUI MVP):** `kestrel-gui` shell, `wry` sandboxed body viewport, system notifications.
- **Phase 5 (Hardening):** Broken MIME stress testing, JMAP provider support, OpenPGP (Sequoia-PGP) signing/encryption.