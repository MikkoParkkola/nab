//! MCP-specific glue that connects [`ActiveReader`] to the live MCP runtime.
//!
//! [`McpLlmSampler`] implements [`LlmSampler`] by sending `sampling/createMessage`
//! requests through the connected MCP client via the `sampling` helper.
//!
//! [`NabUrlFetcher`] implements [`UrlFetcher`] by delegating to
//! [`nab::AcceleratedClient::fetch_text`].
//!
//! Both types live here (in the binary crate) so they can reference the
//! `sampling` module without causing circular-dependency issues in the library.

use std::sync::Arc;

use async_trait::async_trait;
use nab::analyze::active_reading::{
    ActiveReadingError, LlmSampler, Reference, ReferenceKind, Result, UrlFetcher,
};
use nab::AcceleratedClient;
use rust_mcp_sdk::McpServer;
use serde::Deserialize;
use tracing::debug;

// ─── McpLlmSampler ───────────────────────────────────────────────────────────

/// [`LlmSampler`] implementation that calls the MCP client via sampling.
pub struct McpLlmSampler {
    runtime: Arc<dyn McpServer>,
}

impl McpLlmSampler {
    /// Create a new sampler backed by `runtime`.
    pub fn new(runtime: Arc<dyn McpServer>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl LlmSampler for McpLlmSampler {
    async fn identify_references(
        &self,
        chunk: &str,
        segment_offset: usize,
    ) -> Result<Vec<Reference>> {
        let prompt = build_identify_prompt(chunk);
        debug!(
            segment_offset,
            chunk_len = chunk.len(),
            "active reading: sampling identify_references"
        );

        let response_text = crate::sampling::create_message(&self.runtime, &prompt, 500, None)
            .await
            .map_err(|e| ActiveReadingError::SamplingFailed(e.to_string()))?;

        parse_references_response(&response_text, segment_offset)
    }

    async fn summarize(&self, content: &str, query: &str, max_tokens: u32) -> Result<String> {
        let trimmed = trim_content(content);
        let prompt = format!(
            "Summarize this content in ~150 words, focusing on what answers the query.\n\
             Query: {query}\n\nContent:\n{trimmed}"
        );

        crate::sampling::create_message(&self.runtime, &prompt, max_tokens, None)
            .await
            .map_err(|e| ActiveReadingError::SamplingFailed(e.to_string()))
    }
}

// ─── NabUrlFetcher ────────────────────────────────────────────────────────────

/// [`UrlFetcher`] implementation backed by [`AcceleratedClient`].
pub struct NabUrlFetcher {
    client: Arc<AcceleratedClient>,
}

impl NabUrlFetcher {
    /// Create a fetcher that uses `client` for all requests.
    pub fn new(client: Arc<AcceleratedClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl UrlFetcher for NabUrlFetcher {
    async fn fetch_text(&self, url: &str) -> Result<String> {
        self.client
            .fetch_text(url)
            .await
            .map_err(|e| ActiveReadingError::FetchFailed(e.to_string()))
    }
}

// ─── Prompt builders ──────────────────────────────────────────────────────────

fn build_identify_prompt(chunk: &str) -> String {
    format!(
        "You are analyzing a video transcript chunk. Identify references that warrant lookup.\n\
         Return ONLY valid JSON in this exact format:\n\
         {{\"refs\": [{{\"kind\": \"paper|person|tool|claim|number|other\", \
         \"query\": \"...\", \"confidence\": 0.0-1.0}}]}}\n\
         Be conservative — only flag concrete, lookupable items. No more than 5 per chunk.\n\
         Transcript chunk:\n{chunk}"
    )
}

/// Trim content to at most 4000 characters to keep prompt tokens bounded.
fn trim_content(content: &str) -> &str {
    const MAX_CHARS: usize = 4_000;
    if content.len() <= MAX_CHARS {
        content
    } else {
        let mut end = MAX_CHARS;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        &content[..end]
    }
}

// ─── Response parsing ─────────────────────────────────────────────────────────

/// Parse the LLM response into a list of [`Reference`] values.
///
/// Handles markdown code fences that some models wrap their JSON in.
pub(crate) fn parse_references_response(
    text: &str,
    segment_offset: usize,
) -> Result<Vec<Reference>> {
    let cleaned = strip_code_fences(text);

    #[derive(Deserialize)]
    struct Response {
        refs: Vec<RawRef>,
    }

    #[derive(Deserialize)]
    struct RawRef {
        kind: String,
        query: String,
        confidence: f32,
    }

    let parsed: Response = serde_json::from_str(cleaned)
        .map_err(|e| ActiveReadingError::InvalidResponse(format!("JSON parse: {e}")))?;

    Ok(parsed
        .refs
        .into_iter()
        .map(|r| Reference {
            kind: kind_from_str(&r.kind),
            query: r.query,
            confidence: r.confidence,
            segment_idx: segment_offset,
        })
        .collect())
}

/// Strip markdown code fences from the start and end of `text`.
fn strip_code_fences(text: &str) -> &str {
    let s = text.trim();
    let s = s.strip_prefix("```json").unwrap_or(s);
    let s = s.strip_prefix("```").unwrap_or(s);
    let s = s.strip_suffix("```").unwrap_or(s);
    s.trim()
}

/// Convert a kind string returned by the LLM into a [`ReferenceKind`].
fn kind_from_str(s: &str) -> ReferenceKind {
    match s.to_ascii_lowercase().as_str() {
        "paper" => ReferenceKind::Paper,
        "person" => ReferenceKind::Person,
        "tool" => ReferenceKind::Tool,
        "claim" => ReferenceKind::Claim,
        "number" => ReferenceKind::Number,
        _ => ReferenceKind::Other,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Bare JSON without fences parses correctly.
    #[test]
    fn parse_references_response_handles_bare_json() {
        // GIVEN a bare JSON response
        let text =
            r#"{"refs": [{"kind": "paper", "query": "Dijkstra 1968", "confidence": 0.95}]}"#;

        // WHEN parsed
        let refs = parse_references_response(text, 3).unwrap();

        // THEN one reference is returned with the correct segment offset
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, ReferenceKind::Paper);
        assert_eq!(refs[0].query, "Dijkstra 1968");
        assert!((refs[0].confidence - 0.95).abs() < 0.01);
        assert_eq!(refs[0].segment_idx, 3);
    }

    /// JSON wrapped in ```json ... ``` fences parses correctly.
    #[test]
    fn parse_references_response_handles_markdown_fences() {
        // GIVEN a fenced JSON response (as many models return)
        let text = "```json\n{\"refs\": [{\"kind\": \"person\", \"query\": \"Geoffrey Hinton\", \"confidence\": 0.9}]}\n```";

        // WHEN parsed
        let refs = parse_references_response(text, 0).unwrap();

        // THEN the reference is correctly extracted
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, ReferenceKind::Person);
        assert_eq!(refs[0].query, "Geoffrey Hinton");
    }

    /// JSON wrapped in plain ``` fences parses correctly.
    #[test]
    fn parse_references_response_handles_plain_fences() {
        // GIVEN a response with plain code fences
        let text =
            "```\n{\"refs\": [{\"kind\": \"tool\", \"query\": \"ripgrep\", \"confidence\": 0.85}]}\n```";

        // WHEN parsed
        let refs = parse_references_response(text, 1).unwrap();

        // THEN the reference is extracted
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, ReferenceKind::Tool);
    }

    /// Malformed JSON returns `InvalidResponse` error.
    #[test]
    fn parse_references_response_errors_on_malformed_json() {
        // GIVEN broken JSON
        let text = "this is not JSON at all";

        // WHEN parsed
        let result = parse_references_response(text, 0);

        // THEN it's an InvalidResponse error
        assert!(matches!(result, Err(ActiveReadingError::InvalidResponse(_))));
    }

    /// An empty refs array is valid and returns an empty vec.
    #[test]
    fn parse_references_response_handles_empty_refs() {
        // GIVEN a valid response with no references
        let text = r#"{"refs": []}"#;

        // WHEN parsed
        let refs = parse_references_response(text, 0).unwrap();

        // THEN an empty vec is returned
        assert!(refs.is_empty());
    }

    /// Unknown kind strings fall back to `Other`.
    #[test]
    fn kind_from_str_unknown_kind_becomes_other() {
        // GIVEN an unknown kind string
        // WHEN converted
        let kind = kind_from_str("widget");

        // THEN it becomes Other
        assert_eq!(kind, ReferenceKind::Other);
    }

    /// `trim_content` passes short content through unchanged.
    #[test]
    fn trim_content_short_is_unchanged() {
        // GIVEN content shorter than 4000 chars
        let content = "Hello, world!";

        // WHEN trimmed
        let result = trim_content(content);

        // THEN it's identical
        assert_eq!(result, content);
    }

    /// `trim_content` truncates long content at a valid UTF-8 boundary.
    #[test]
    fn trim_content_long_is_truncated() {
        // GIVEN content much longer than 4000 chars
        let content = "a".repeat(8_000);

        // WHEN trimmed
        let result = trim_content(&content);

        // THEN it's at most 4000 chars and is valid UTF-8
        assert!(result.len() <= 4_000);
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }
}
