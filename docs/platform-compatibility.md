# Platform Compatibility

This document describes the platform-specific notes for building and running
Kestrel across Linux, macOS, and Windows.

## Platform Matrix

| Feature | Linux | macOS | Windows |
|---------|-------|-------|---------|
| Build (cargo) | ✅ | ✅ | ✅ |
| Keyring backend | `secret-service` (D-Bus) | `security-framework` | `win Credential Manager` |
| WebView renderer | wry/webkit2gtk | wry/WebKit | wry WebView2 |
| Tray icon | ✅ `tray-icon` | ✅ `tray-icon` | ✅ `tray-icon` |
| Native file dialogs | rfd/gtk3 | rfd/mac-dialog | rfd/win-dialog |
| TLS | rustls (ring) | rustls (ring) | rustls (ring) |

## Keyring Backend Details

### Linux (`secret-service`)

- Requires D-Bus session bus (standard on GNOME/KDE; may need `dbus-launch` on
  headless systems).
- Flatpak/Snap: must declare `org.freedesktop.secrets` portal access.
- Fallback: if D-Bus is unavailable, `KeyringUnavailable` is surfaced.

### macOS (`security-framework`)

- Uses the macOS Keychain. No additional setup required.
- First access prompts a Keychain unlock dialog (standard macOS UX).

### Windows (`win Credential Manager`)

- Uses the Windows Credential Manager API via the `winapi` crate.
- No additional setup required; credentials appear in Credential Manager
  under the `Kestrel` target name.

## WebView Rendering

Kestrel uses [`wry`](https://docs.rs/wry) for the optional GUI/webview layer:

| Platform | Backend | Notes |
|----------|---------|-------|
| Linux | webkit2gtk | Requires `libwebkit2gtk-4.1-dev` (Ubuntu/Debian) |
| macOS | WebKit | Ships with macOS; no extra deps |
| Windows | WebView2 | Ships with Windows 10+; evergreen runtime on older |

Build dependencies (Linux):

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev
```

## Tray Icon Support

All three platforms are supported via the `tray-icon` crate:

- **Linux**: AppIndicator (Ubuntu/Debian) or `libappindicator-gtk3`.
- **macOS**: NSStatusItem (native).
- **Windows**: `Shell_NotifyIcon` (native).

## Testing Checklist

### Build

- [ ] `cargo build --workspace` succeeds
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo +nightly fmt --all --check` passes

### Unit Tests

- [ ] `cargo nextest run --workspace` (all non-ignored tests pass)
- [ ] `cargo test --workspace` (fallback runner)

### Integration Tests (Docker)

- [ ] `cargo nextest run --profile integration` (Dovecot + Greenmail fixtures)
- [ ] `KESTREL_INTEGRATION=1 cargo test --package kestrel-sync --test integration`

### Real-World Integration Tests

- [ ] Gmail (OAuth2 + IMAP):
  ```bash
  export KESTREL_GMAIL_INTEGRATION=1
  export KESTREL_GMAIL_REFRESH_TOKEN="1//0..."
  export KESTREL_GMAIL_CLIENT_ID="..."
  export KESTREL_GMAIL_CLIENT_SECRET="..."
  export KESTREL_GMAIL_EMAIL="user@gmail.com"
  cargo test --package kestrel-sync --test gmail_real -- --ignored
  ```

- [ ] Fastmail (JMAP):
  ```bash
  export KESTREL_JMAP_INTEGRATION=1
  export KESTREL_JMAP_API_TOKEN="..."
  cargo test --package kestrel-sync --test jmap_real -- --ignored
  ```

### Platform-Specific

- [ ] Keyring round-trip (set + get + purge)
- [ ] Tray icon appears and responds to clicks
- [ ] WebView renders content (if GUI crate is enabled)
- [ ] TLS connections to Gmail/Fastmail succeed
