# Enterprise Edition License (PolyForm Noncommercial)

**Status**: Active as of v0.9.0 (2026-04-25). See MIK-3035, MIK-3036.

This file describes the license terms that apply to designated **Enterprise Edition (EE)** files within the `nab` repository. The Path C dual-license refactor landed in v0.9.0.

## Scope

Files marked with the SPDX header

```rust
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
```

are licensed under the **PolyForm Noncommercial** license (version 1.0.0). Enterprise Edition files in formats that cannot carry comments, such as JSON, are also PolyForm Noncommercial when they live under the active EE-designated paths below. All other files remain under the MIT License (see `LICENSE-MIT`).

Required Notice: Includes nab Enterprise Edition components by Mikko Parkkola, https://github.com/MikkoParkkola/nab. Commercial use requires a separate license.

## Enterprise Edition coverage (planned, per ADR-001 / MIK-3036)

### EE-designated paths (active):

- `src/auth/` — 1Password, WebAuthn, cookie injection (premium authenticated reach)
- `src/fingerprint/` — fingerprint spoofing (anti-bot, WAF evasion)
- `src/waf/` — WAF evasion patterns
- `src/site/` — per-site provider integrations (proprietary domain knowledge)
- `src/security/` — Secure Ingestion guard (machine-targeted directive detection + sanitisation, provenance reporting). Shipped in v0.10.0. Detects AI-addressed HTML comments, machine-only `data-*` attribute payloads, machine-class elements, `display:none` text, and `aria-hidden="true"` text. See `examples/scan_html.rs`.
- `crates/nab-yara-engine/` — fetch-time YARA-X signature guard for prompt injection, exfiltration, secrets, and obfuscation.
- `examples/scan_html.rs` — Secure Ingestion demonstration and sanitizer example.

## Attribution and derivative works

Copies, modified versions, forks, distributions, and derivative works that include any EE-designated component must preserve the Required Notice above and the attribution notices in `NOTICE`.

The Required Notice is not a commercial-use grant. Commercial use of EE-designated files, features, or derivative works still requires a separate commercial license.

## Summary of PolyForm Noncommercial terms

- **Free** for noncommercial use, modification, redistribution
- **Commercial use requires a separate commercial license** — contact the maintainer
- All other rights reserved

Full license text reference: see polyformproject.org for the canonical license document.

## Commercial licensing

For commercial use of EE-designated files, contact: mikko.parkkola@iki.fi

## Existing MIT-licensed releases

Versions of `nab` released **before** v0.9 are entirely MIT-licensed and remain so forever. The PolyForm Noncommercial license applies only to v0.9+ EE-designated files.

## References

- ADR-001: `claude-elite/docs/adr/ADR-001-ip-strategy.md` (Path C decision)
- Linear: MIK-3024 (umbrella), MIK-3034, MIK-3035, MIK-3036
- PolyForm Project: search "PolyForm Noncommercial 1.0.0"
