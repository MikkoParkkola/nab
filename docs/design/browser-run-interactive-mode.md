# Browser Run Interactive Mode Spike

Status: research spike + follow-up issue filed
Date: 2026-05-12
Phase: MIK-2924 Phase 1 / Phase 2 scoping
Novelty: opt-in remote browser runtime for JS-heavy pages, without changing nab's HTTP-first default

## Thesis

`nab fetch` should stay a fast, local-first HTTP microfetch path. Browser rendering is still useful for the narrow class of pages that require live JavaScript execution, DOM interaction, or login/CAPTCHA handoff. The right shape is provider-neutral and explicit: `nab browser <url>` or `nab fetch --render/--interactive` should connect to a configured CDP browser endpoint, while default `nab fetch` only suggests that path when thin-content/SPA heuristics fire.

## Source Check

Cloudflare's Browser Run docs now expose two integration categories:

- Quick Actions for simple screenshot/PDF/scrape/markdown/json/crawl jobs.
- Browser Sessions for direct control through Puppeteer, Playwright, CDP, Stagehand, and MCP clients.

Current Cloudflare pricing, checked 2026-05-12:

- Free: 10 browser minutes per day.
- Workers Paid: 10 browser hours per month included, then $0.09 per additional browser hour.
- Browser Sessions also charge for concurrency: 10 concurrent browsers averaged monthly included, then $2.00 per additional browser.
- Paid plan limits list 120 concurrent Browser Sessions, one new Browser Session per second, and 10 Quick Actions per second.

Browserbase pricing, checked 2026-05-12:

- Free: 1 browser hour and 3 concurrent browsers.
- Developer: $20/month, 100 browser hours, 25 concurrent browsers, then $0.12/browser hour.
- Startup: $99/month, 500 browser hours, 100 concurrent browsers, then $0.10/browser hour.
- Browserbase documents a one-minute minimum for browser sessions.

Anchor Browser did not expose comparable public pricing during this spike, so it stays out of the cost table until quote-backed numbers exist.

## Live Browser Run Trial

Environment already had `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_BROWSER_RUN_TOKEN`, and `wrangler` installed. No secret values were printed or recorded.

Representative task:

1. Connect to Browser Run over CDP WebSocket.
2. Navigate to `https://developers.cloudflare.com/browser-run/`.
3. Click the `Pricing` navigation link.
4. Extract the resulting page URL, heading, and body text marker.

10-trial result:

| Metric | Result |
| --- | ---: |
| Success rate | 10/10 |
| Average latency | 707 ms |
| Median latency | 566 ms |
| p95 latency | 1361 ms |
| Total browser time measured locally | 7401 ms |
| Marginal browser-hour cost at $0.09/hour | $0.000185 |

This is comfortably below the ticket's fail-fast threshold of $0.50 for a 3-step browsing task. The measurement is a controlled docs-site workflow, not a guarantee for CAPTCHA-heavy or authenticated SaaS workflows.

## Decision

Build the `nab` interface as a provider-neutral external CDP path, not a Cloudflare-specific integration.

Recommended first implementation:

```text
nab browser <url> [--cdp-url <wss://...>] [--headers-env <ENV_NAME>]
nab fetch <url> --render
nab fetch <url> --interactive
```

Default behavior:

- `nab fetch` never auto-launches a local browser or remote browser provider.
- Thin-content/SPA heuristics may print a hint that suggests `nab browser`.
- Remote browser providers are disabled unless explicitly configured.
- Cookie/session handoff must be documented as provider-boundary-sensitive. Local browser cookies do not automatically become remote browser cookies.

Configuration should prefer env or existing config extension:

```text
NAB_BROWSER_CDP_WS=wss://...
NAB_BROWSER_CDP_HEADERS='{"Authorization":"Bearer ..."}'
NAB_BROWSER_PROVIDER=cdp
```

The implemented CLI keeps this provider-neutral contract: `--cdp-url` overrides `NAB_BROWSER_CDP_WS`, and `--headers-env` selects the environment variable that holds either a JSON object or newline-delimited `Name: value` headers. Header values are consumed by the WebSocket handshake but are not printed in command output.

The browser implementation should accept Cloudflare Browser Run, Browserbase, and a local Chrome `--remote-debugging-port` session through the same CDP adapter. Provider-specific setup belongs in docs, not in the core execution path.

## Why Not Bundle Chromium

The ticket's kill criterion is correct: a 100+ MB bundled Chromium fallback would undermine nab's single-binary, fast-install story. Keep `browser` feature-gated and external-provider-based. A local Chromium path is acceptable only when the user already has a browser running with CDP enabled.

## Public Follow-Up

Filed public GitHub issue:

- https://github.com/MikkoParkkola/nab/issues/93

Issue #93 carries the implementation acceptance criteria for the `nab`-owned slice. MIK-2924 should remain the cross-product decision record.

## Product Recommendation

- Botnaut interactive web browsing: buy before build. Browser Run is viable and cheap for early usage when Cloudflare is already acceptable infrastructure. Browserbase remains a credible alternative when built-in identity/CAPTCHA/session inspector features outweigh the higher base plan price. Native Playwright infrastructure should require a concrete sovereignty or high-volume cost reason.
- nab: implement only explicit render/interactive mode. Do not reposition nab as a browser.
- WebMCP markup: continue tracking, but treat it as early-preview browser-agent surface area. For nab itself, Secure Ingestion should detect WebMCP manifests defensively before nab encourages public docs to advertise them.

## References

- Cloudflare Browser Run overview: https://developers.cloudflare.com/browser-run/
- Cloudflare Browser Run MCP/CDP client docs: https://developers.cloudflare.com/browser-run/cdp/mcp-clients/
- Cloudflare Browser Run pricing: https://developers.cloudflare.com/browser-run/pricing/
- Cloudflare Browser Run limits: https://developers.cloudflare.com/browser-run/limits/
- Cloudflare launch post: https://blog.cloudflare.com/browser-run-for-ai-agents/
- Chrome WebMCP early preview: https://developer.chrome.com/blog/webmcp-epp
- Chrome DevTools MCP: https://github.com/ChromeDevTools/chrome-devtools-mcp
- Browserbase pricing: https://www.browserbase.com/pricing/
- Browserbase concurrency docs: https://docs.browserbase.com/optimizations/concurrency/overview
