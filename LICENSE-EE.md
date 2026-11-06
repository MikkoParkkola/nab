# Enterprise Edition License (PolyForm Noncommercial)

**Status**: Active as of v0.9.0 (2026-04-25). See MIK-3035, MIK-3036.

This file describes the license terms that apply to designated **Enterprise Edition (EE)** files within the `nab` repository. The Path C dual-license refactor landed in v0.9.0.

## Required Notice

Distributions, modified versions, forks, and derivative works that include Enterprise Edition components must preserve the following attribution (see `NOTICE`):

> Required Notice: Includes nab Enterprise Edition components by Mikko Parkkola, https://github.com/MikkoParkkola/nab. Commercial use requires a separate license.

## Scope

Files marked with the SPDX header

```rust
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
```

are licensed under the **PolyForm Noncommercial** license (version 1.0.0). All other files remain under the existing MIT License (see `LICENSE`).

## Enterprise Edition coverage (planned, per ADR-001 / MIK-3036)

### EE-designated paths (active):

- `src/auth/` — 1Password, WebAuthn, cookie injection (premium authenticated reach)
- `src/fingerprint/` — fingerprint spoofing (anti-bot, WAF evasion)
- `src/waf/` — WAF evasion patterns
- `src/site/` — per-site provider integrations (proprietary domain knowledge)
- `src/security/` — Secure Ingestion guard (machine-targeted directive detection + sanitisation, provenance reporting). Shipped in v0.10.0. Detects AI-addressed HTML comments, machine-only `data-*` attribute payloads, machine-class elements, `display:none` text, and `aria-hidden="true"` text. See `examples/scan_html.rs`.

## Summary of PolyForm Noncommercial terms

- **Free** for noncommercial use, modification, redistribution, and the public-benefit uses permitted by the license
- **Commercial use outside those permissions requires a separate commercial license** — see [COMMERCIAL.md](COMMERCIAL.md)
- All other rights reserved

Full license text: [PolyForm Noncommercial 1.0.0](https://polyformproject.org/licenses/noncommercial/1.0.0).

## Commercial licensing

For commercial use of EE-designated files that requires additional permission,
the standard license covers **one organization and one named Mikko Parkkola
product** (`nab`), not each customer application or repository:

| License | GitHub Sponsors | Separate direct-invoice offer |
| --- | --- | --- |
| Small business: up to five people in the whole organization | USD 100/month | EUR 100/month |
| Organization: no headcount limit | USD 500/month | EUR 500/month |

Count working owners, employees, and regular contractors for the small-business
limit. Both tiers include internal use of the same published features without
per-tool, server, agent, or call charges. Support, SLA, unpublished repository
access, resale, and third-party hosted/managed service rights are not included.
MIT permissions for core files and historical releases remain unchanged.

Read [COMMERCIAL.md](COMMERCIAL.md) for the complete scope, exclusions, paid
period, registration, growth, and custom-payment rules, existing commitments,
and the additional free nonprofit permission. Sponsors charges in USD; EUR
invoice prices are separate offers, not currency conversions. Arrange direct
invoicing or custom terms by email: **mikko.parkkola@iki.fi**.

The Coffee (USD 5 one-time), Supporter (USD 5/month), and Sustainer
(USD 25/month) tiers are voluntary support, not commercial licenses. Nonprofit
and other permitted free use does not require sponsorship.

Sponsor link: https://github.com/sponsors/MikkoParkkola?metadata_project=nab

## Existing MIT-licensed releases

Versions of `nab` released **before** v0.9 are entirely MIT-licensed and remain so forever. The PolyForm Noncommercial license applies only to v0.9+ EE-designated files.

## References

- ADR-001: `claude-elite/docs/adr/ADR-001-ip-strategy.md` (Path C decision)
- Linear: MIK-3024 (umbrella), MIK-3034, MIK-3035, MIK-3036
- PolyForm Project: search "PolyForm Noncommercial 1.0.0"
