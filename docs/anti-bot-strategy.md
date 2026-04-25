# Anti-Bot Strategy

Status: 2026-04-25 · production fetch ladder + three-track innovation roadmap.
Trigger: LinkedIn activity-feed dead-end (HTTP 999 + behavioural fingerprinting at the application layer).

## TL;DR

- **Production now**: Chrome-137 emulation in `impersonate_client.rs` (commit ca7616e). Voyager XHR scaffold in `src/site/linkedin/voyager.rs` (commit b302a64). Both ship today.
- **Production next**: a five-tier escalation ladder (HTTP → site-provider → embedded-blob → rquickjs → bundled-chromium). MIK-3060.
- **Innovation tracks**: F (data export) ships first. A (browser-fingerprint clone) ships second. C (MV3 extension piggyback) ships third. B (static GraphQL queryId extraction) is dead.

## Production fetch ladder (target: MIK-3060)

```
Tier 1.  HTTP fetch  — wreq Chrome-137 emulation (impersonate_client.rs)
Tier 2.  Site-provider extraction — linkedin/twitter/youtube/spotify/...
Tier 3.  Embedded-blob extractor — __NEXT_DATA__, __NUXT__, __INITIAL_STATE__, __PRELOADED_STATE__
Tier 4.  rquickjs JS engine — inline-script execution; today's `nab spa` engine
Tier 5.  chromiumoxide CDP — full Chrome; gated --features browser; opt-in via flag and site-rule
```

**Auto-elect to Tier 5** when ALL of the following heuristics match the Tier-1 response:

- Body < 5 KB of meaningful text (after stripping `<script>` and `<style>`).
- Hydration root is empty: `<div id="root"></div>`. `<div id="app"></div>`. `<noscript>` block says JS required.
- No embedded blob (`__NEXT_DATA__`, `__NUXT__`, `__INITIAL_STATE__`, `__PRELOADED_STATE__`).
- HTML contains `data-react-helmet`, `<script type="module">`, `data-sveltekit-` markers — strong SPA signal.

**Force Tier 5** via site rule (`engine = "browser"` in `linkedin.toml`). MIK-3061 adds the schema field.

## Three-track innovation roadmap

### Track F — official data export (ships first; ~1 day)

LinkedIn Settings → Data Privacy → Get a copy of your data. Authoritative. ~24 h latency.

- New subcommand: `nab linkedin export`.
- Automates the request via the user's authenticated session (cookies path).
- Polls the export-ready endpoint. Downloads the ZIP. Ingests into the same `SiteContent` shape used elsewhere.
- Acceptable for monthly post-engagement tracking. Not acceptable for real-time feed scraping.
- Why first: zero anti-bot exposure, deterministic, no behavioural fingerprinting concerns.

### Track A — fingerprint clone from running real browser (ships second; ~3 days)

Detect the user's running Brave/Chrome via process list. Read its runtime fingerprint:

- UA, plugins, WebGL renderer, Canvas hash, Audio fingerprint.
- Screen geometry, timezone, Sec-CH-UA-* claims.

Two paths to read the fingerprint:

1. CDP read-only attach (when the user's browser has remote-debugging enabled).
2. Parse the browser's `Local State` and `Preferences` files on disk.

nab's HTTP fetcher then replays this exact fingerprint via wreq custom emulation. Indistinguishable from the real browser at the wire level. Limit: still HTTP-only; cannot fire JS-XHR-only endpoints. Combine with Track C for full coverage.

### Track C — MV3 browser extension piggyback (ships third; ~3 days)

User installs a small Manifest V3 extension. Extension exposes `localhost:NNNN` API. nab calls the localhost API. Extension fetches from inside the user's authenticated tab context. Response streams back to nab.

- Inherits the trusted profile's behavioural history. The user's browser does the work.
- Cross-browser: Chrome, Brave, Edge.
- One-time install, signed.
- Cleanest pure-architecture answer to behavioural fingerprinting. No external browser launched by nab.

## Killed track REVIVED — B (static GraphQL queryId extraction)

Original verdict (2026-04-25): killed because "chunks not reachable from SSR via BFS". That framing was wrong. **Re-tested 2026-04-26 and it works.**

The 14 `<script src="https://static.licdn.com/aero-v1/sc/h/...">` chunks loaded by `/in/{handle}/recent-activity/all/` are reachable via direct GET (no auth on the static CDN). Downloading them and grepping for `voyagerFeedDashProfileUpdates\.[a-f0-9]+` surfaces 5 queryId hashes today; all 5 hit the GraphQL endpoint with HTTP 200 and return the typed activity-feed response (300+ KB each).

Wired into `src/site/linkedin/voyager.rs` ahead of the historical REST fallbacks. End-to-end test confirmed: `nab fetch https://www.linkedin.com/in/mikko-parkkola/recent-activity/all/ --cookies brave` now returns the live activity feed as markdown — no browser, no DevTools capture.

When all 5 hashes start failing simultaneously (LinkedIn rotates queryIds every ~2-3 weeks), re-discover by repeating the chunk-grep procedure. A future iteration auto-discovers on demand and caches.

## Why behavioural fingerprinting beats feature spoofing

Fresh `--user-data-dir`. No scroll/click history. No warmup browsing. Unfamiliar TLS+JS combination. LinkedIn classifies the session as suspicious **at the application layer** (server-side risk score). Bypassing browser-level checks (webdriver flag, headless UA) is necessary but not sufficient. Tracks A and C address the application layer by either (1) cloning the trusted browser's wire-level fingerprint exactly, (2) running the fetch from inside the trusted browser itself.

## Cross-references

- MIK-3057: LinkedIn marketing tracking (downstream consumer).
- MIK-3058: nab fingerprint auto-maintenance.
- MIK-3059: LinkedIn activity-feed parser (Track F + Voyager scaffold satisfy this).
- MIK-3060: auto-SPA-escalation ladder (Tier 5 entry).
- MIK-3061: site-rules `engine` field schema.
- MIK-3068: cookies export `www.` host_key bug.
- MIK-3069: no-browser innovation research (parent of A/C/F).

## Build order

1. F — `nab linkedin export` subcommand. ✅ **Infrastructure shipped 2026-04-26**. CLI, csrf extraction, cookies resolution, JSON POST + GET via Chrome-137 impersonation, exponential-backoff polling state machine, ZIP download path, 12 unit tests. Endpoint pinned to `/mysettings-api/settingsApiDataExport/` (discovered 2026-04-25 by grepping the SPA bundle). The current default JSON body is rejected with HTTP 400; the field name has rotated. Pin via Chrome DevTools → `--body-override` flag. Block: live body capture; the rest of the pipeline is stable.
2. A — fingerprint-clone wired into `impersonate_client.rs`.
3. C — MV3 extension + localhost bridge.

## Rollback

Each track ships independently behind its own feature flag. Tier 5 stays gated behind `--features browser` so default builds remain ~20 MB. Reverting any single track does not affect the others.

---

v2026.04.25 · nab v0.10.1 · ladder target MIK-3060 · innovation parent MIK-3069
