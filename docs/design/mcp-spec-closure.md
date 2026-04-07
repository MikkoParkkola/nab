# nab MCP — full 2025-11-25 spec closure

**Status**: design
**Date**: 2026-04-07
**Phase**: 2 (after analyze v2 + URL watch)
**Goal**: bring nab from ~80% spec compliance to 100%

## Current state (audited 2026-04-07)

nab implements MCP 2025-11-25 via `rust-mcp-sdk = "0.9"`. It already has:

- ✅ Protocol version `2025-11-25` (LATEST_PROTOCOL_VERSION constant)
- ✅ stdio transport
- ✅ 8 tools with structured output schemas + annotations
- ✅ task-augmented execution (fetch_batch, soon analyze)
- ✅ 2 resources (`nab://guide/quickstart`, `nab://status`)
- ✅ 3 prompts (fetch-and-extract, multi-page-research, authenticated-fetch)
- ✅ Elicitation: form mode + URL mode (login.rs, elicitation.rs)
- ✅ Server icons (light/dark SVG)
- ✅ Implementation metadata (name, version, title, description, instructions)
- ✅ Tool input validation errors as `isError: true` (SEP-1303 compliant)

## Gaps to close

### 1. Streamable HTTP transport (~250 lines)

Currently nab is stdio-only. The MCP 2025-11-25 spec defines two standard transports; Streamable HTTP enables:
- Network access (run nab on a server, use it from any client)
- Multi-client connections to one nab process (saves init cost)
- Resumable streams with `Last-Event-ID`

**Implementation**:

In `Cargo.toml` enable the streamable-http feature:
```toml
rust-mcp-sdk = { version = "0.9", default-features = false, features = ["server", "macros", "stdio", "streamable-http"] }
```

Add a `--http <bind>` CLI arg to nab-mcp:
```bash
nab-mcp                          # stdio (default)
nab-mcp --http 127.0.0.1:8765    # Streamable HTTP on local port
nab-mcp --http 0.0.0.0:8765      # all interfaces (with origin check warning)
```

Mandatory security per spec (basic/transports):
- ✅ `Origin` header check (HTTP 403 on mismatch — MUST per SEP-1439)
- ✅ Bind to localhost when no `--http-allow-origin` supplied
- ✅ `MCP-Protocol-Version` header check on all post-init requests (HTTP 400 if invalid)
- ✅ Session ID via `MCP-Session-Id` (cryptographically random)
- ✅ SSE resumability via `Last-Event-ID` (rust-mcp-sdk handles this)
- ✅ DELETE to terminate session

### 2. Sampling support (~150 lines)

Server-side: nab needs to *call* sampling on the client (not advertise it as a server capability — sampling is a client capability per spec). nab uses sampling for:

- **Active reading** (Phase 1.5b — see active-reading.md design)
- **Smart fetch focus**: when `nab fetch URL --focus "what was decided"` is called, nab can sample to identify which DOM sections to keep
- **Form field auto-fill**: when `nab login` finds a form, nab can sample for "what's the right value for this field?"

The pattern: check `runtime.peer_capabilities().sampling` at request time; if Some, call `runtime.create_message(...)`. Tests pass via mock client.

### 3. Roots support (~80 lines)

`roots/list` is a client-side capability that lets the client tell the server which file:// URIs it has access to. nab uses this for:

- **Workspace-aware downloads**: `nab fetch URL --save` writes to a path under one of the client's roots, never outside
- **`nab fetch file://path`**: validate the path is under an advertised root before reading
- **MCP-driven save targets**: future "save to user's project" flows

Implementation: query `runtime.list_roots()` on first use, cache for the session lifetime, refresh on `notifications/roots/list_changed`.

### 4. MCP logging (`notifications/message`) (~100 lines)

Currently nab logs via `tracing` to stderr. The MCP spec defines structured logging via `notifications/message` with RFC 5424 levels. Benefits:

- LLM clients can show nab's logs inline in their UI
- Levels: debug, info, notice, warning, error, critical, alert, emergency
- Structured `data` field — much better than parsing stderr

Implementation:
```rust
// Add capability
capabilities: ServerCapabilities {
    logging: Some(serde_json::Map::new()),
    // ...
}

// Handle setLevel
async fn handle_set_level_request(&self, params: SetLevelRequestParams, runtime: Arc<dyn McpServer>) -> Result<(), RpcError> {
    LOGGER.set_level(params.level);
    Ok(())
}

// Replace tracing::info! with mcp_log::info! (a thin wrapper that emits both tracing AND notifications/message)
mcp_log::info!(runtime, "fetch", "Fetching {} with cookies={:?}", url, cookies);
```

The wrapper:
```rust
pub struct McpLogger {
    runtime: OnceLock<Arc<dyn McpServer>>,
    level: AtomicU8,
}

impl McpLogger {
    pub fn log(&self, level: LogLevel, logger: &str, msg: &str) {
        if (level as u8) < self.level.load(Ordering::Relaxed) { return; }
        if let Some(rt) = self.runtime.get() {
            rt.send_notification(LoggingMessageNotification {
                level: level.into(),
                logger: Some(logger.into()),
                data: serde_json::Value::String(msg.into()),
            }).ok();
        }
        // Also emit to tracing for stderr/file logging
        tracing::event!(level.into(), msg);
    }
}
```

### 5. Resource subscriptions (~100 lines)

Currently `subscribe: None` in advertised capabilities. Switch to `subscribe: Some(Map::new())` after URL watch resources land (Phase 1.5a).

Beyond URL watch, also subscribe-able:
- `nab://status` — push updates when version changes (auto-update detection)
- `nab://session/<id>` — push updates when a session's cookie store changes
- `nab://watch/<id>` — push updates when a watch detects a change (the URL watch case)

### 6. Argument completion (`completion/complete`) (~150 lines)

The 2025-11-25 spec defines `completion/complete` for prompt arguments and resource template parameters. nab can auto-complete:

- `cookies` argument → list installed browsers (brave, chrome, firefox, safari, edge)
- `session` argument → list existing session names from `~/.local/state/nab/sessions/`
- `browser` argument (fingerprint) → list known fingerprint profiles
- prompt URLs → recent URLs from history
- file paths in `analyze input` → tab completion via file system

Implementation:
```rust
async fn handle_complete_request(&self, params: CompleteRequestParams, _runtime: Arc<dyn McpServer>) -> Result<CompleteResult, RpcError> {
    match params.ref_ {
        CompletionReference::Prompt(prompt_ref) => self.complete_prompt_arg(&prompt_ref, &params.argument),
        CompletionReference::Resource(resource_ref) => self.complete_resource_template(&resource_ref, &params.argument),
    }
}
```

Add capability: `completions: Some(Map::new())`.

### 7. URL-mode elicitation already done (verify)

Audit confirmed `elicitation.rs` has both form mode AND URL mode (the OAuth flow at line 296-327). No work needed beyond verifying it advertises `elicitation.url` capability — currently advertises just `elicitation` (which the spec accepts but is less specific).

### 8. Tool list_changed notifications (~50 lines)

Currently `tools.list_changed: None`. Switch to `Some({})` and emit `notifications/tools/list_changed` when:

- A new tool is dynamically registered (e.g., when `analyze` becomes available after model download)
- A tool's annotations change

Same for `prompts.list_changed` and `resources.list_changed`.

### 9. Pagination cursors

Currently nab returns small lists (8 tools, 3 prompts, 2 resources). No pagination needed today. If watch resources grow into hundreds, add cursor support:

```rust
async fn handle_list_resources_request(&self, params: Option<PaginatedRequestParams>, _runtime: Arc<dyn McpServer>) -> Result<ListResourcesResult, RpcError> {
    let cursor = params.and_then(|p| p.cursor);
    let (page, next_cursor) = self.resources_paginate(cursor.as_deref(), 50);
    Ok(ListResourcesResult { resources: page, next_cursor, ..Default::default() })
}
```

`rust-mcp-sdk` handles the wire format; nab just needs to slice.

## Cargo deps

New (for Streamable HTTP transport):
- enable `streamable-http` feature on `rust-mcp-sdk`
- (rust-mcp-sdk pulls in axum + hyper + tower transitively — already in nab's tree via reqwest)

No new top-level deps.

## Tests

For each gap, add a unit test:
- Streamable HTTP: spawn server, POST initialize, assert response shape
- Sampling: mock client, call from nab tool, assert request shape
- Roots: mock client with roots, assert nab respects them
- MCP logging: capture notifications, assert level filtering works
- Resource subscriptions: subscribe, trigger, assert notification delivered
- Completions: each argument source produces expected suggestions
- list_changed: dynamically register tool, assert notification fires

Total ~400 lines of new tests.

## Ship plan

Single PR after Phase 1.5 lands. ~1500 lines core + 400 lines tests + docs.

Order of implementation:
1. MCP logging (foundational — used by everything else)
2. Resource subscriptions (foundational — used by URL watch already shipped)
3. list_changed notifications (cheap, high value)
4. Argument completion (cheap, high DX value)
5. Sampling support (used by active reading already shipped)
6. Roots support (used by analyze/fetch save flows)
7. Streamable HTTP (the big one — separate sub-PR if it grows)

## Verification

After all changes:
```bash
cd /Users/mikko/github/nab
cargo check --features cli 2>&1 | tail -30
cargo test --features cli mcp_server:: 2>&1 | tail -30
```

Then run the official MCP conformance test suite (if/when it becomes available — currently `mcp-validator` is the closest thing) against the running server:

```bash
cargo run --bin nab-mcp -- --http 127.0.0.1:8765 &
mcp-validator http://127.0.0.1:8765/mcp --spec 2025-11-25
```

Target: 100% pass rate on the 2025-11-25 conformance suite.

## Out of scope

- OAuth 2.1 server: nab is a tool server, not an auth server. If exposed over HTTP for multi-user, the deployment platform handles auth (reverse proxy with nginx/caddy + OIDC). Document this in README.
- Experimental tasks features beyond what fetch_batch + analyze use: deferred until a real use case emerges.
- Custom transport layers (WebSocket, MQTT, etc.): not in spec, not in nab's mission.
