# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.6.x   | :white_check_mark: |
| 0.5.x   | :white_check_mark: |
| < 0.5   | :x: |

## Reporting a Vulnerability

If you discover a security vulnerability in nab, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, please email **mikko.parkkola@iki.fi** with:

1. A description of the vulnerability
2. Steps to reproduce
3. Impact assessment
4. Any suggested fix (optional)

You will receive an acknowledgment within 48 hours and a detailed response within 7 days.

## Security Scope

nab handles sensitive data including:

- **Browser cookies**: Extracted from local browser databases (Brave, Chrome, Firefox, Safari, Edge)
- **1Password credentials**: Retrieved via the 1Password CLI (`op`)
- **TLS connections**: HTTP/2, HTTP/3 (QUIC), TLS 1.3
- **Authentication tokens**: Session cookies, OAuth tokens, CSRF tokens

### Security Measures

- **No cloud**: All processing is local. No data is sent to third-party servers.
- **No telemetry**: nab does not collect or transmit usage data.
- **SSRF protection**: URL validation rejects private/internal IP ranges by default.
- **Cookie isolation**: Browser cookie databases are read-only; nab never writes to them.
- **Session isolation**: Named MCP sessions use independent cookie jars.
- **TLS verification**: Certificate validation is always enabled (no `--insecure` flag).
- **Dependency auditing**: `cargo-audit` runs in CI on every push via `rustsec/audit-check`.

## Disclosure Policy

We follow coordinated disclosure:

1. Reporter submits vulnerability privately
2. We confirm and assess within 7 days
3. We develop and test a fix
4. We release the fix and publish a security advisory
5. Reporter is credited (unless they prefer anonymity)
