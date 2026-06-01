# ADR: nab Task Engine — Browser-Mode Dimensions for Rung-3 Execution

**Date:** 2026-06-01
**Ticket:** MIK-5359 (Phase 1)
**Status:** Accepted
**Author:** MikkoParkkola / claude-elite session

## Context

The nab task engine (design: `2026-05-31-nab-task-engine.md`) escalates to an
external browser at rung 3 via the opt-in `chromiumoxide` CDP feature. Before
rung-3 work begins, the operator ratified three orthogonal configuration
dimensions that any rung-3 browser launch must model. This ADR records those
decisions, their CLI/MCP surface, the mapping to `chromiumoxide` primitives, and
the auth-seeding contract from `export.1`.

## Decision

### Dimension 1 — Visibility (`headless` vs `headed`)

| | Detail |
|---|---|
| **Default** | Headless (`--headless` in CDP / `BrowserConfig::builder().headless(true)`) |
| **Override flag** | `--headed` CLI flag; `headed: bool` MCP param (default `false`) |
| **Auto-engage** | Test-mode detection (`NAB_TEST=1` or `--test` flag) and interactive chat-portal sessions engage `--headed` automatically so the operator can observe, solve CAPTCHAs, and approve 2FA |
| **Rationale** | Headless is the default for unattended agents; headed is the safety net for human-in-the-loop flows. Automat detection prevents agents from silently skipping CAPTCHA prompts in CI |

### Dimension 2 — Persistence/Isolation (`persistent` vs `incognito`)

| | Detail |
|---|---|
| **Default** | Persistent — the browser launches with the user's existing Chrome/Brave profile directory (same as today's `--remote-debugging-port=9222` flow) |
| **Override flag** | `--incognito` CLI flag; `incognito: bool` MCP param (default `false`) |
| **Per-site policy** | A TOML list `incognito_sites = ["example.com", …]` in `~/.config/nab/task.toml` forces incognito for those domains regardless of the flag, so operators need not remember to pass `--incognito` for sensitive or third-party domains |
| **chromiumoxide mapping** | Incognito → `BrowserConfig::builder().user_data_dir(tempdir())` for a throwaway profile **or** `Browser::new_incognito_context()` if the CDP target is already running. Nothing is written back to the normal browser profile: no history entry, no cookie persistence, no `localStorage`/`IndexedDB` visible in later sessions |
| **Rationale** | Operator-defined incognito prevents accidental persistent side-effects (logins to third-party services, history pollution) on automation tasks the user did not intend to be durable |

### Dimension 3 — Auth Seeding (`cookieless` vs `seeded-from-export`)

| | Detail |
|---|---|
| **Default** | Cookieless (incognito context starts with an empty cookie jar) |
| **Seeded mode** | Pass a `storage_state` JSON path via `--storage-state <path>` CLI or `storage_state_path: Option<PathBuf>` MCP param. nab injects the cookies (and optionally `localStorage` entries) into the CDP session at launch via `Network.setCookies` + `Runtime.evaluate(localStorage.setItem…)` |
| **Export source** | `nab cookies export --format playwright [--output path]` (MIK-5359 `export.1`, shipped in this PR) produces the seed artifact |
| **Auth-seeded incognito** | The primary use case: `--incognito --storage-state auth.json`. The context starts logged-in (real session cookies injected), but nothing persists after the browser closes. Session is discarded on context teardown |
| **Live-chat pattern** | `--headed --incognito --storage-state auth.json` — logged-in, visible to the operator, fully isolated. This is the canonical configuration for interactive sessions on third-party portals |
| **Rationale** | Separates "what credentials does the agent start with" from "what isolation model does the session use". An incognito context without seeding is a clean anonymous session; with seeding it is a logged-in throwaway session |

## Combined CLI/MCP Surface

```
# CLI
nab task "<goal>" <url> [--headed] [--incognito] [--storage-state path/to/auth.json]

# MCP (nab-mcp tool schema fragment)
{
  "headed":        { "type": "boolean", "default": false },
  "incognito":     { "type": "boolean", "default": false },
  "storage_state": { "type": "string",  "description": "Path to Playwright storage_state JSON" }
}
```

Defaults (`headed=false`, `incognito=false`, no `storage_state`) reproduce the
current `nab login --browser` behavior exactly — no behavior change to existing
callers.

## chromiumoxide Mapping

| Mode | chromiumoxide config |
|---|---|
| Headless (default) | `BrowserConfig::builder().headless(true).build()` |
| Headed | `BrowserConfig::builder().headless(false).build()` |
| Persistent (default) | existing user-data-dir (Chrome/Brave profile path) |
| Incognito (temp dir) | `BrowserConfig::builder().user_data_dir(tempdir()).build()` |
| Incognito (already-running Chrome) | `Browser::new_incognito_context()` CDP call |
| Seeded auth | After context open: `Network.setCookies(cookies)` from storage_state |

## Firefox/Safari Fidelity Note (Best-Effort)

`nab cookies export --format playwright` produces faithful metadata for
Chromium-family browsers (Brave/Chrome/Edge): real `domain`, `path`, `expires`,
`httpOnly`, `secure`, and `sameSite` are read from the `SQLite` cookie database.

For Firefox/Safari/Python-fallback extraction, only `name`/`value` are currently
available. The exported storage_state for these sources synthesizes safe defaults
(`domain` = queried domain, `path = "/"`, session expiry, `secure=true`,
`sameSite="Lax"`). The storage_state is schema-valid and will authenticate on
the queried domain, but may miss cookies on sub-domains not covered by the
synthesized domain. Users on Firefox/Safari should prefer `--cookies brave` or
`--cookies chrome` when exporting for rung-3 seeding.

This limitation is Phase-1 only. Phase 2 will extend `browser_cookie3`-backed
extraction to emit full metadata.

## Consequences

- `nab cookies export --format playwright` (shipped in this PR) is the only
  authoritative way to produce a seed artifact. Do not hand-craft storage_state
  JSON — the Chromium epoch conversion and `samesite` integer mapping are
  non-obvious and are handled automatically by the exporter.
- Per-site `incognito_sites` config is the right place for operator policy;
  ad-hoc `--incognito` flags are for one-off overrides.
- Rung 3 requires the `browser` cargo feature. The default build (`--features
  default`) never compiles or links `chromiumoxide`. All three dimensions are
  no-ops on the default build and return a `delegate_to_browser` response.
- The `storage_state` seed path is passed through nab's `ssrf::validate_url`
  equivalent for the file path (path traversal guard) before being read.

## References

- Design doc: `docs/design/2026-05-31-nab-task-engine.md`
- Cookie export implementation: `src/auth/cookies/storage_state.rs`
- MIK-5359 (Linear)
- Playwright `storageState` schema: https://playwright.dev/docs/api/class-browsercontext#browser-context-storage-state
- chromiumoxide: https://github.com/mattsse/chromiumoxide
