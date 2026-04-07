# nab MCP — URL watch resources

**Status**: design
**Date**: 2026-04-07
**Phase**: 1.5 (after analyze v2, before MCP spec closure)
**Novelty**: no current MCP server does this; turns the entire web into a notification source

## The unlock

MCP 2025-11-25 lets servers expose **subscribable resources** with `notifications/resources/updated` events when the underlying resource changes. nab already exposes 2 static resources (`nab://guide/quickstart`, `nab://status`) but currently advertises `subscribe: None`.

By switching `subscribe: Some({})` and adding a watch subsystem, nab becomes **RSS for the entire web** — for any URL, including pages without RSS feeds. This is genuinely novel.

## User-facing behavior

```bash
# CLI
nab watch add https://news.ycombinator.com --interval 10m
nab watch add https://example.com/pricing --interval 1h --selector "table.pricing"
nab watch add https://api.openai.com/status --interval 5m --notify-on regression
nab watch list
nab watch remove <id>
nab watch logs <id>
```

In MCP clients, watches appear as resources:

```jsonrpc
// Client subscribes
> {"method": "resources/subscribe", "params": {"uri": "nab://watch/<id>"}}

// nab pushes when content changes
< {"method": "notifications/resources/updated", "params": {"uri": "nab://watch/<id>"}}

// Client reads to get the diff
> {"method": "resources/read", "params": {"uri": "nab://watch/<id>"}}
< {"contents": [{"uri": "nab://watch/<id>", "text": "## Changed since 2026-04-07T03:00Z\n+ New product: Foo\n- Removed: Bar"}]}
```

## Use cases (illustrative, not exhaustive)

| Use case | Example URL | Selector / heuristic |
|---|---|---|
| Price tracking | amazon.com/dp/B0XXX | `span#priceblock_ourprice` |
| Status pages | status.openai.com | `[data-component-status]` |
| Job boards | careers.example.com | full page diff, semantic |
| Docs changes | docs.anthropic.com/en/api | `main article` |
| Competitor pricing | competitor.com/pricing | `table.pricing` |
| News tracking | news.ycombinator.com | `.athing .titleline` |
| GitHub releases | github.com/foo/bar/releases | full page |
| HF model updates | huggingface.co/google/gemma-3 | `[data-target=updated]` |
| Government regs | regulations.gov/document/X | full page |
| Court filings | courtlistener.com/docket/X | full page |
| Calendar/event pages | example.com/events | full page |
| Stock pages | finance.yahoo.com/quote/X | `fin-streamer` |

## Architecture

### Storage

Watches live in `~/.local/share/nab/watches/` as one JSON file per watch:

```json
{
  "id": "abc123de",
  "url": "https://example.com/pricing",
  "selector": "table.pricing",
  "interval_secs": 3600,
  "created_at": "2026-04-07T03:30:00Z",
  "last_check_at": "2026-04-07T04:30:00Z",
  "last_change_at": "2026-04-07T03:30:00Z",
  "last_etag": "W/\"123abc\"",
  "last_last_modified": "Mon, 07 Apr 2026 03:30:00 GMT",
  "snapshots": [
    {"sha256": "...", "captured_at": "2026-04-07T03:30:00Z", "size": 12345}
  ],
  "subscribers": ["mcp-session-abc"],
  "options": {
    "notify_on": "any",  // any | regression | semantic
    "diff_kind": "semantic",  // text | semantic | dom
    "max_snapshots": 10
  }
}
```

The actual snapshot bodies live in `~/.local/share/nab/watches/snapshots/<sha256>` so duplicate snapshots dedupe by content hash.

### Background poller

A single `tokio::task` loop iterates all watches every minute, picks the ones whose `last_check_at + interval_secs < now()`, and fetches them in parallel (capped at 8 concurrent). Uses `nab::AcceleratedClient` with conditional GETs (`If-None-Match`, `If-Modified-Since`) — 304 responses don't even count as a check, much less a change.

```rust
pub struct WatchManager {
    storage_dir: PathBuf,
    client: Arc<AcceleratedClient>,
    watches: Arc<RwLock<HashMap<WatchId, Watch>>>,
    notification_tx: tokio::sync::broadcast::Sender<WatchEvent>,
}

impl WatchManager {
    pub async fn poll_loop(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            self.poll_due_watches().await;
        }
    }
    
    async fn poll_due_watches(&self) {
        let due: Vec<_> = self.watches.read().values()
            .filter(|w| w.is_due())
            .cloned()
            .collect();
        let semaphore = Arc::new(Semaphore::new(8));
        let futures = due.into_iter().map(|w| {
            let sem = semaphore.clone();
            async move {
                let _permit = sem.acquire().await;
                self.poll_one(w).await
            }
        });
        futures::future::join_all(futures).await;
    }
}
```

### Diff strategies

| Kind | How | When to use |
|---|---|---|
| `text` | Levenshtein on plain markdown after html2md | Default for general pages |
| `semantic` | nab fetches → markdown → strips noise (nav/footer) → diff stable sections | News, docs, blog posts |
| `dom` | CSS selector → element subtree diff | Targeted: pricing, status, specific cards |

For `selector`-based watches, only the selected element's DOM subtree is hashed and diffed. This eliminates false positives from rotating ads, timestamp-in-footer, A/B tests, etc.

### MCP integration

In `nab/src/bin/mcp_server/main.rs`:

1. **Capability change**: `resources.subscribe = Some(Map::new())` (currently `None`)
2. **Add watch resources** to `all_resources()`: dynamically enumerate all watches as `nab://watch/<id>` resources
3. **Handle `resources/subscribe`** in `ServerHandler` — record the session id against the watch
4. **Handle `resources/unsubscribe`** — remove the session
5. **Push notifications** — when the poller detects a change, fan out to all subscribed sessions via `notifications/resources/updated`
6. **Add `watch_create`, `watch_remove`, `watch_list` MCP tools** so the LLM can manage watches dynamically

### Adaptive backoff

If a URL returns:
- 304 Not Modified → check counted, no change recorded
- 429 / 503 → multiply interval by 2 (capped at 24h), reset on next 200
- 200 with no content change → no event
- 200 with content change → emit event, notify subscribers
- 4xx other / connection error → log warning, count toward 5-failure mute threshold

### Polling cost

Default interval = 1 hour. 100 watches = 100 conditional GETs per hour = ~2.4K requests/day. With ETag caching, the server load is negligible. nab respects `Cache-Control: max-age` as a *minimum* interval — never polls faster than the upstream allows.

### Privacy

- All watches and snapshots are local. Nothing uploaded.
- nab respects `robots.txt` for the polling interval (`Crawl-delay`).
- User-Agent identifies as `nab-watch/0.7 (https://github.com/MikkoParkkola/nab)` so site owners can block if they object.

## Cargo deps

Already present:
- `reqwest` (with conditional GET support)
- `serde_json`
- `sha2`
- `tokio` (sync primitives, broadcast channel)
- `dirs` (XDG paths)

New:
- nothing — this is pure plumbing on top of existing primitives

## Tests

- Unit: snapshot dedup by content hash
- Unit: 304 conditional GET path (mocked HTTP server via `mockito`)
- Unit: selector-based diff isolates target element
- Integration: end-to-end watch lifecycle with a local HTTP server changing content
- MCP integration: subscribe → trigger change → assert notification fires

## Future extensions (deferred)

- Webhook delivery (`notify_url`) for non-MCP consumers
- LLM-summarized change descriptions ("Price increased from $19 to $29")
- Multi-watch composition (e.g., "alert me when both X and Y change within 1 hour")
- Sharing watch templates as `.nabwatch` files

## Ship plan

Single PR after analyze v2 lands:
- ~600 lines Rust core
- ~150 lines tests
- Docs update (CHANGELOG, README)
- Cargo.toml — no new deps
