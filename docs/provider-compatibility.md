# Provider Compatibility Matrix

Status: **v1.0** · Last updated: 2026-08-31

---

## Compatibility Matrix

| # | Provider | IMAP | SMTP | JMAP | OAuth2 | App Password | Plain Auth | Auto-Detect | Preset | Test Script | Status |
|---|----------|------|------|------|--------|--------------|------------|-------------|--------|-------------|--------|
| 1 | Gmail | ✅ | ✅ | — | ✅ | ✅ | ✅ | ✅ | ✅ | `gmail.sh` | Ready |
| 2 | Outlook / Microsoft 365 | ✅ | ✅ | — | ✅ | ✅ | ✅ | ✅ | ✅ | `outlook.sh` | Ready |
| 3 | Yahoo Mail | ✅ | ✅ | — | ✅ | ✅ | ✅ | ✅ | ✅ | `yahoo.sh` | Ready |
| 4 | iCloud Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `icloud.sh` | Ready |
| 5 | AOL Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `aol.sh` | Ready |
| 6 | Proton Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `proton.sh` | Ready |
| 7 | Fastmail | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | `fastmail.sh` | Ready |
| 8 | Zoho Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `zoho.sh` | Ready |
| 9 | GMX Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `gmx.sh` | Ready |
| 10 | Web.de | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `webde.sh` | Ready |
| 11 | Mail.ru / VK Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `mailru.sh` | Ready |
| 12 | Yandex Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `yandex.sh` | Ready |
| 13 | Comcast Xfinity Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `comcast.sh` | Ready |
| 14 | AT&T Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `att.sh` | Ready |
| 15 | Verizon Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `verizon.sh` | Ready |
| 16 | T-Online Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `tonline.sh` | Ready |
| 17 | IONOS / 1&1 Mail | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `ionos.sh` | Ready |
| 18 | Rackspace Email | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `rackspace.sh` | Ready |
| 19 | Mailbox.org | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | ✅ | `mailbox.sh` | Ready |
| 20 | Migadu | ✅ | ✅ | — | — | ✅ | ✅ | — | — | `migadu.sh` | Ready |

### Key

- **✅** — Supported and verified
- **—** — Not applicable or not supported by provider
- **Preset** — Auto-detected IMAP/SMTP host, port, and security settings from `provider_preset()`
- **Auto-Detect** — Domain-based detection via `detect_provider()`
- **Test Script** — Shell wrapper in `tests/integration/providers/` that invokes `provider_real` integration test

---

## Preset Configuration Details

| Provider | IMAP Host | IMAP Port | IMAP Security | SMTP Host | SMTP Port | SMTP Security |
|----------|-----------|-----------|---------------|-----------|-----------|---------------|
| Gmail | `imap.gmail.com` | 993 | TLS | `smtp.gmail.com` | 465 | TLS |
| Outlook | `outlook.office365.com` | 993 | TLS | `smtp.office365.com` | 587 | STARTTLS |
| Yahoo | `imap.mail.yahoo.com` | 993 | TLS | `smtp.mail.yahoo.com` | 465 | TLS |
| iCloud | `imap.mail.me.com` | 993 | TLS | `smtp.mail.me.com` | 587 | STARTTLS |
| AOL | `imap.aol.com` | 993 | TLS | `smtp.aol.com` | 465 | TLS |
| Proton | `127.0.0.1` | 1143 | STARTTLS | `127.0.0.1` | 1025 | STARTTLS |
| Fastmail | `imap.fastmail.com` | 993 | TLS | `smtp.fastmail.com` | 465 | TLS |
| Zoho | `imap.zoho.com` | 993 | TLS | `smtp.zoho.com` | 465 | TLS |
| GMX | `imap.gmx.net` | 993 | TLS | `smtp.gmx.net` | 465 | TLS |
| Web.de | `imap.web.de` | 993 | TLS | `smtp.web.de` | 587 | STARTTLS |
| Mail.ru | `imap.mail.ru` | 993 | TLS | `smtp.mail.ru` | 465 | TLS |
| Yandex | `imap.yandex.com` | 993 | TLS | `smtp.yandex.com` | 465 | TLS |
| Comcast | `imap.comcast.net` | 993 | TLS | `smtp.comcast.net` | 587 | STARTTLS |
| AT&T | `imap.mail.att.net` | 993 | TLS | `smtp.mail.att.net` | 465 | TLS |
| Verizon | `imap.verizon.net` | 993 | TLS | `smtp.verizon.net` | 465 | TLS |
| T-Online | `secureimap.t-online.de` | 993 | TLS | `securesmtp.t-online.de` | 465 | TLS |
| IONOS | `imap.ionos.com` | 993 | TLS | `smtp.ionos.com` | 587 | STARTTLS |
| Rackspace | `secure.emailsrvr.com` | 993 | TLS | `secure.emailsrvr.com` | 587 | STARTTLS |
| Mailbox.org | `imap.mailbox.org` | 993 | TLS | `smtp.mailbox.org` | 465 | TLS |
| Migadu | `imap.migadu.com` | 993 | TLS | `smtp.migadu.com` | 465 | TLS |

---

## Domain Detection Mapping

| Domain(s) | Provider |
|-----------|----------|
| `gmail.com`, `googlemail.com` | Gmail |
| `outlook.com`, `hotmail.com`, `live.com`, `msn.com`, `passport.com` | Outlook |
| `yahoo.com`, `yahoo.co.uk`, `yahoo.ca`, `yahoo.com.au`, `yahoo.co.in`, `yahoo.co.jp` | Yahoo |
| `icloud.com`, `me.com`, `mac.com` | iCloud |
| `aol.com`, `aim.com` | AOL |
| `proton.me`, `protonmail.com`, `pm.me` | Proton |
| `fastmail.com`, `fastmail.fm` | Fastmail |
| `zoho.com`, `zohomail.com`, `zoho.eu` | Zoho |
| `gmx.com`, `gmx.de`, `gmx.net`, `gmx.at`, `gmx.ch` | GMX |
| `web.de` | Web.de |
| `mail.ru`, `inbox.ru`, `list.ru`, `bk.ru` | Mail.ru |
| `yandex.ru`, `yandex.com`, `ya.ru`, `yandex.ua`, `yandex.by`, `yandex.kz` | Yandex |
| `comcast.net` | Comcast |
| `att.net`, `sbcglobal.net`, `bellsouth.net` | AT&T |
| `verizon.net`, `verizon.com` | Verizon |
| `t-online.de` | T-Online |
| `1and1.com`, `1und1.de`, `ionos.com` | IONOS |
| `rackspace.com` | Rackspace |
| `mailbox.org` | Mailbox.org |

---

## Known Quirks and Limitations

| Provider | Quirk |
|----------|-------|
| Gmail | Requires App Password (2FA enforced). OAuth2 supported but not yet wired in GUI. IMAP must be enabled in Google account settings. |
| Outlook | OAuth2 is the recommended auth method. App passwords require "less secure apps" workaround or admin policy. |
| Yahoo | Requires App Password or OAuth2. IMAP access must be enabled in account security settings. |
| iCloud | Requires App-Specific Password generated at appleid.apple.com. No OAuth2 support for IMAP. |
| AOL | Requires App Password. IMAP access must be enabled in account security settings. |
| Proton | **Requires Proton Mail Bridge** running locally. Preset points to `127.0.0.1:1143` (Bridge IMAP). Bridge must be downloaded from proton.me/mail/bridge. |
| Fastmail | JMAP-native provider; IMAP/SMTP also supported. OAuth2 supported. |
| Zoho | App passwords required for IMAP/SMTP. IMAP must be enabled in portal settings. |
| GMX | IMAP must be explicitly enabled in account settings (Settings > POP3 & IMAP). |
| Web.de | IMAP must be explicitly enabled in account settings (Settings > POP3 & IMAP). |
| Mail.ru | App passwords required. IMAP enabled by default for most accounts. |
| Yandex | App passwords required. IMAP enabled by default. |
| Comcast | Limited IMAP support; some accounts may require "Xfinity Mail" setup. |
| AT&T | AT&T has migrated many legacy accounts; some may not support IMAP. |
| Verizon | Verizon email migrated to AOL; legacy `verizon.net` addresses redirect through AOL servers. |
| T-Online | German provider; IMAP access requires current T-Online credentials. |
| IONOS | Supports IMAP/SMTP natively; no special setup required. |
| Rackspace | Business-oriented; uses shared `secure.emailsrvr.com` for both IMAP and SMTP. |
| Mailbox.org | German privacy-focused provider; supports IMAP/SMTP natively. |
| Migadu | No auto-detection preset; manual host/port configuration required. Privacy-focused Swiss provider. |
