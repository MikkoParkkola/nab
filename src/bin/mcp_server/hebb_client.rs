//! Minimal hebb MCP client for in-process composition.
//!
//! Spawns `hebb-mcp` as a child process and speaks the MCP 2025-11-25 protocol
//! over stdio using plain `serde_json` and `tokio::process::Command`. Not a full
//! MCP client — just enough to call `voice_match`, `voice_remember`, `kv_set`,
//! and `kv_get` from nab's own tool implementations.
//!
//! ## Why not a proper MCP client crate?
//!
//! `rust-mcp-sdk` 0.9 ships a client feature but enabling it pulls in more
//! dependencies than we need for a handful of tool calls. The subprocess pattern
//! matches how nab already shells out to `ffmpeg` and `fluidaudiocli`.
//!
//! ## Lifecycle
//!
//! The client is lazily initialized on first call, held in a static
//! `OnceLock<Arc<Mutex<HebbClient>>>`, and reused across subsequent calls.  The
//! `hebb-mcp` subprocess stays alive for the lifetime of the `nab-mcp` parent.
//!
//! ## Graceful degradation
//!
//! When `hebb-mcp` is not installed, [`HebbClient::is_available`] returns
//! `false` and callers skip enrichment silently.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

// ─── Global singleton ─────────────────────────────────────────────────────────

static GLOBAL: OnceLock<Arc<Mutex<HebbClient>>> = OnceLock::new();

// ─── Public types ─────────────────────────────────────────────────────────────

/// A matched voice from the hebb voiceprint database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceMatch {
    /// Opaque identifier assigned by hebb when the voice was enrolled.
    pub voice_id: String,
    /// Human-readable name, if set during enrollment.
    pub name: Option<String>,
    /// Cosine similarity in `[0.0, 1.0]` — higher is more similar.
    pub similarity: f32,
}

// ─── Client ───────────────────────────────────────────────────────────────────

/// Subprocess-backed hebb MCP client.
///
/// Maintains an open `hebb-mcp` child process and communicates over its
/// stdin/stdout using newline-delimited JSON-RPC 2.0.
pub struct HebbClient {
    /// Running hebb-mcp child process (kept alive for reuse).
    _child: Child,
    /// Write end — we send JSON-RPC requests here.
    stdin: ChildStdin,
    /// Line-by-line reader over hebb-mcp's stdout.
    stdout: BufReader<ChildStdout>,
    /// Monotonically increasing request id.
    next_id: u64,
}

impl HebbClient {
    // ── Construction ────────────────────────────────────────────────────────

    /// Spawn `hebb-mcp` and perform the MCP initialization handshake.
    ///
    /// Returns `Err` when the binary cannot be found or the handshake fails.
    async fn spawn() -> Result<Self> {
        let binary = locate_hebb_binary()
            .ok_or_else(|| anyhow!("hebb-mcp not found in PATH"))?;

        let mut child = tokio::process::Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()) // suppress hebb's own tracing output
            .spawn()
            .with_context(|| format!("failed to spawn hebb-mcp at {}", binary.display()))?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

        let mut client = Self {
            _child: child,
            stdin,
            stdout,
            next_id: 0,
        };

        client.handshake().await?;
        Ok(client)
    }

    /// Send `initialize` + `initialized` to complete the MCP handshake.
    async fn handshake(&mut self) -> Result<()> {
        let init_response = self
            .send_request("initialize", json!({
                "protocolVersion": "2025-11-25",
                "capabilities": { "sampling": {} },
                "clientInfo": { "name": "nab-mcp", "version": env!("CARGO_PKG_VERSION") }
            }))
            .await
            .context("MCP initialize handshake failed")?;

        if init_response.get("error").is_some() {
            bail!("hebb-mcp initialize returned error: {init_response}");
        }

        // Send `notifications/initialized` (no response expected).
        self.send_notification("notifications/initialized", json!({}))
            .await
            .context("hebb-mcp initialized notification failed")?;

        Ok(())
    }

    // ── Global accessor ─────────────────────────────────────────────────────

    /// Return (or lazily initialize) the global `HebbClient`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `hebb-mcp` is not installed or the MCP handshake fails.
    pub async fn global() -> Result<Arc<Mutex<Self>>> {
        // Fast path: already initialized.
        if let Some(client) = GLOBAL.get() {
            return Ok(Arc::clone(client));
        }

        // Slow path: spawn and initialize.
        let client = Self::spawn().await?;
        let arc = Arc::new(Mutex::new(client));

        // Another task may have raced us; ignore the duplicate.
        let _ = GLOBAL.set(Arc::clone(&arc));

        // Return whichever value won the race.
        Ok(Arc::clone(GLOBAL.get().expect("just set")))
    }

    /// Return `true` when `hebb-mcp` can be located on `$PATH`.
    ///
    /// Does **not** spawn a subprocess — useful as a cheap pre-check before
    /// paying the async initialization cost.
    pub async fn is_available() -> bool {
        locate_hebb_binary().is_some()
    }

    // ── Low-level JSON-RPC ──────────────────────────────────────────────────

    /// Send a JSON-RPC 2.0 request and return the parsed `result` object.
    ///
    /// Blocks until a response line is received from the subprocess.
    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        self.write_line(&msg).await?;

        // Read lines until we find one matching our request id.
        loop {
            let response = self.read_response_line().await?;
            if response.get("id") == Some(&json!(id)) {
                return Ok(response);
            }
            // Notifications or other ids — skip.
        }
    }

    /// Send a JSON-RPC 2.0 notification (no response expected).
    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_line(&msg).await
    }

    /// Serialize `value` as a single JSON line and write it to hebb's stdin.
    async fn write_line(&mut self, value: &Value) -> Result<()> {
        let line = serde_json::to_string(value)
            .context("failed to serialize JSON-RPC message")?;
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .context("write to hebb-mcp stdin failed")?;
        self.stdin.flush().await.context("flush hebb-mcp stdin")?;
        Ok(())
    }

    /// Read one non-empty line from hebb's stdout and parse it as JSON.
    async fn read_response_line(&mut self) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .stdout
                .read_line(&mut line)
                .await
                .context("read from hebb-mcp stdout failed")?;
            if n == 0 {
                bail!("hebb-mcp subprocess closed stdout unexpectedly");
            }
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return serde_json::from_str(trimmed)
                    .with_context(|| format!("hebb-mcp response is not valid JSON: {trimmed}"));
            }
        }
    }

    // ── `tools/call` helper ─────────────────────────────────────────────────

    /// Call a hebb tool by name and return the first text/structured content.
    ///
    /// # Errors
    ///
    /// Returns `Err` when:
    /// - The JSON-RPC call itself fails
    /// - hebb returns `isError: true` in the result
    /// - The response cannot be parsed
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let response = self
            .send_request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await
            .with_context(|| format!("tools/call '{name}' failed"))?;

        if let Some(err) = response.get("error") {
            bail!("hebb tool '{name}' RPC error: {err}");
        }

        let result = response
            .get("result")
            .ok_or_else(|| anyhow!("hebb response missing 'result' field"))?;

        // MCP CallToolResult: check isError first.
        if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            let msg = extract_first_text_content(result)
                .unwrap_or_else(|| format!("tool '{name}' returned isError=true"));
            bail!("{msg}");
        }

        // Try structuredContent first, fall back to first text content as JSON.
        if let Some(structured) = result.get("structuredContent") {
            return Ok(structured.clone());
        }

        let text = extract_first_text_content(result)
            .ok_or_else(|| anyhow!("hebb tool '{name}' returned no content"))?;

        serde_json::from_str(&text)
            .with_context(|| format!("hebb tool '{name}' text content is not valid JSON: {text}"))
    }

    // ── Typed wrappers ──────────────────────────────────────────────────────

    /// Call hebb's `voice_match` tool.
    ///
    /// Returns matches with `similarity >= threshold`, up to `limit` results,
    /// ordered by descending similarity.
    ///
    /// # Arguments
    ///
    /// - `embedding`: 256-dimensional speaker embedding from the diarizer
    /// - `threshold`: minimum cosine similarity to include (e.g. `0.7`)
    /// - `limit`: maximum number of matches to return
    pub async fn voice_match(
        &mut self,
        embedding: &[f32],
        threshold: f32,
        limit: u32,
    ) -> Result<Vec<VoiceMatch>> {
        let embedding_values: Vec<Value> = embedding.iter().map(|&f| json!(f)).collect();

        let result = self
            .call_tool("voice_match", json!({
                "embedding": embedding_values,
                "threshold": threshold,
                "limit": limit,
            }))
            .await?;

        parse_voice_matches(&result)
    }

    /// Call hebb's `voice_remember` tool to enroll a new voiceprint.
    ///
    /// Returns the `voice_id` assigned by hebb.
    ///
    /// # Arguments
    ///
    /// - `embedding`: 256-dimensional speaker embedding
    /// - `source`: human-readable provenance string (e.g. `"meeting-2026-04-06.mp4"`)
    /// - `name`: optional display name to associate with this voice
    pub async fn voice_remember(
        &mut self,
        embedding: &[f32],
        source: &str,
        name: Option<&str>,
    ) -> Result<String> {
        let embedding_values: Vec<Value> = embedding.iter().map(|&f| json!(f)).collect();

        let mut args = json!({
            "embedding": embedding_values,
            "source": source,
        });

        if let Some(n) = name {
            args["name"] = json!(n);
        }

        let result = self.call_tool("voice_remember", args).await?;

        result
            .get("voice_id")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| anyhow!("voice_remember response missing 'voice_id'"))
    }

    /// Call hebb's `kv_set` tool to store a key-value pair.
    ///
    /// # Arguments
    ///
    /// - `namespace`: logical partition (e.g. `"urls"`, `"sessions"`)
    /// - `key`: unique key within the namespace
    /// - `value`: arbitrary JSON payload
    /// - `content_text`: optional plain-text representation for embedding
    pub async fn kv_set(
        &mut self,
        namespace: &str,
        key: &str,
        value: Value,
        content_text: Option<&str>,
    ) -> Result<()> {
        let mut args = json!({
            "namespace": namespace,
            "key": key,
            "value": value,
        });

        if let Some(text) = content_text {
            args["content_text"] = json!(text);
        }

        self.call_tool("kv_set", args).await?;
        Ok(())
    }

    /// Call hebb's `kv_get` tool.
    ///
    /// Returns `None` when the key does not exist in the given namespace.
    pub async fn kv_get(&mut self, namespace: &str, key: &str) -> Result<Option<Value>> {
        let result = self
            .call_tool("kv_get", json!({ "namespace": namespace, "key": key }))
            .await;

        match result {
            Ok(v) => Ok(Some(v)),
            Err(e) if is_not_found_error(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ─── Binary discovery ─────────────────────────────────────────────────────────

fn locate_hebb_binary() -> Option<PathBuf> {
    if let Ok(path) = which::which("hebb-mcp") {
        return Some(path);
    }
    // Common managed-install location.
    let managed = dirs::data_local_dir()?.join("hebb/bin/hebb-mcp");
    managed.exists().then_some(managed)
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Extract the text from the first `TextContent` entry in a `CallToolResult`.
fn extract_first_text_content(result: &Value) -> Option<String> {
    result
        .get("content")?
        .as_array()?
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .map(String::from)
}

/// Parse a `voice_match` result value into a `Vec<VoiceMatch>`.
fn parse_voice_matches(result: &Value) -> Result<Vec<VoiceMatch>> {
    let matches = result
        .get("matches")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("voice_match result missing 'matches' array"))?;

    matches
        .iter()
        .map(|m| {
            Ok(VoiceMatch {
                voice_id: m
                    .get("voice_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("voice_match entry missing 'voice_id'"))?
                    .to_string(),
                name: m.get("name").and_then(Value::as_str).map(String::from),
                similarity: m
                    .get("similarity")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| anyhow!("voice_match entry missing 'similarity'"))?
                    as f32,
            })
        })
        .collect()
}

/// Return `true` when an error looks like a "key not found" response.
fn is_not_found_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("not found") || msg.contains("no such key") || msg.contains("key_not_found")
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_available fast path ───────────────────────────────────────────────

    /// `is_available()` returns `false` when `hebb-mcp` is not on PATH.
    ///
    /// This test relies on the binary being absent in the CI environment.
    /// It is a best-effort check — it won't fail if hebb-mcp happens to be
    /// installed.
    #[tokio::test]
    async fn is_available_returns_bool_without_panic() {
        // GIVEN the current environment (hebb-mcp may or may not be installed)
        // WHEN we check availability
        let result = HebbClient::is_available().await;
        // THEN we get a bool without panicking
        let _ = result; // value depends on environment
    }

    // ── parse_voice_matches ──────────────────────────────────────────────────

    /// `parse_voice_matches` deserializes a well-formed matches array.
    #[test]
    fn parse_voice_matches_well_formed() {
        // GIVEN a valid voice_match result
        let result = json!({
            "matches": [
                { "voice_id": "v_abc123", "name": "Alice", "similarity": 0.92 },
                { "voice_id": "v_def456", "name": null, "similarity": 0.75 }
            ]
        });
        // WHEN we parse it
        let matches = parse_voice_matches(&result).expect("should parse");
        // THEN we get two entries with correct fields
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].voice_id, "v_abc123");
        assert_eq!(matches[0].name.as_deref(), Some("Alice"));
        assert!((matches[0].similarity - 0.92).abs() < 1e-4);
        assert_eq!(matches[1].voice_id, "v_def456");
        assert!(matches[1].name.is_none());
    }

    /// `parse_voice_matches` returns an error when the `matches` key is absent.
    #[test]
    fn parse_voice_matches_missing_key_returns_err() {
        // GIVEN a result without the 'matches' field
        let result = json!({ "something_else": [] });
        // WHEN we parse it
        let err = parse_voice_matches(&result);
        // THEN we get an error
        assert!(err.is_err(), "expected Err, got Ok");
    }

    /// `parse_voice_matches` returns empty vec for an empty array.
    #[test]
    fn parse_voice_matches_empty_array() {
        // GIVEN an empty matches array
        let result = json!({ "matches": [] });
        // WHEN parsed
        let matches = parse_voice_matches(&result).expect("should parse");
        // THEN result is empty
        assert!(matches.is_empty());
    }

    // ── extract_first_text_content ───────────────────────────────────────────

    /// `extract_first_text_content` finds the first text entry.
    #[test]
    fn extract_first_text_content_picks_text_type() {
        // GIVEN a call result with mixed content types
        let result = json!({
            "content": [
                { "type": "image", "data": "base64..." },
                { "type": "text", "text": "{\"voice_id\": \"v_x\"}" },
            ]
        });
        // WHEN extracted
        let text = extract_first_text_content(&result);
        // THEN we get the text entry
        assert_eq!(text.as_deref(), Some("{\"voice_id\": \"v_x\"}"));
    }

    /// `extract_first_text_content` returns `None` when there is no text block.
    #[test]
    fn extract_first_text_content_none_when_no_text() {
        // GIVEN a result with no text content
        let result = json!({ "content": [{ "type": "image", "data": "..." }] });
        // WHEN extracted
        let text = extract_first_text_content(&result);
        // THEN nothing is returned
        assert!(text.is_none());
    }

    // ── is_not_found_error ───────────────────────────────────────────────────

    /// `is_not_found_error` matches common "not found" phrases.
    #[test]
    fn is_not_found_error_matches_known_phrases() {
        let cases = [
            (anyhow!("key not found in namespace"), true),
            (anyhow!("no such key: urls/abc"), true),
            (anyhow!("key_not_found"), true),
            (anyhow!("connection refused"), false),
            (anyhow!("internal server error"), false),
        ];
        for (err, expected) in cases {
            assert_eq!(
                is_not_found_error(&err),
                expected,
                "mismatch for: {err}"
            );
        }
    }
}
