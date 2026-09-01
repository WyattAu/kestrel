# Provider Integration Tests

These tests validate IMAP/JMAP connectivity against real email providers.

## Setup

Each provider requires specific credentials. Set the following environment variables:

| Provider | Required Env Vars |
|----------|-------------------|
| Gmail | `KESTREL_GMAIL_INTEGRATION=1`, `KESTREL_GMAIL_EMAIL`, `KESTREL_GMAIL_PASSWORD` |
| Outlook | `KESTREL_OUTLOOK_INTEGRATION=1`, `KESTREL_OUTLOOK_EMAIL`, `KESTREL_OUTLOOK_PASSWORD` |
| Yahoo | `KESTREL_YAHOO_INTEGRATION=1`, `KESTREL_YAHOO_EMAIL`, `KESTREL_YAHOO_PASSWORD` |
| iCloud | `KESTREL_ICLOUD_INTEGRATION=1`, `KESTREL_ICLOUD_EMAIL`, `KESTREL_ICLOUD_PASSWORD` |
| ... | ... |

## Running Tests

### Individual provider
```bash
KESTREL_GMAIL_INTEGRATION=1 \
KESTREL_GMAIL_EMAIL="your@email.com" \
KESTREL_GMAIL_PASSWORD="your-app-password" \
./tests/integration/providers/gmail.sh
```

### All providers
```bash
for script in tests/integration/providers/*.sh; do
    echo "Testing $(basename $script)..."
    bash "$script" || echo "FAILED: $script"
done
```

## Credentials

- **Never commit credentials** to the repository
- Use App Passwords for providers that require them
- Store credentials in environment variables or a local `.env` file (gitignored)
