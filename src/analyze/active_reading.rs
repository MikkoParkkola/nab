//! Active reading pipeline — enrich transcripts with live reference lookups.
//!
//! While a video is being transcribed, this module periodically sends transcript
//! chunks to the host LLM (via MCP sampling) to identify references worth looking
//! up — papers, people, tools, claims, numbers. Each surviving reference is then
//! fetched via [`UrlFetcher`] and summarised back through the LLM. The resulting
//! summaries are inlined as numbered footnotes in the [`TranscriptionResult`].
//!
//! # Design
//!
//! The module is pure logic — it depends on two injected traits:
//!
//! - [`LlmSampler`] — asks the host LLM to identify references or summarise text.
//! - [`UrlFetcher`] — retrieves the target URL as plain text.
//!
//! The MCP-specific implementations live in [`super::active_reading_mcp`].
//!
//! # Example
//!
//! ```rust,ignore
//! let sampler = McpLlmSampler::new(runtime.clone());
//! let fetcher  = NabUrlFetcher::new(client.clone());
//! let mut reader = ActiveReader::new(&sampler, &fetcher, ActiveReadingConfig::default());
//! let output = reader.process(&mut transcript).await?;
//! println!("{} footnotes generated", output.footnotes.len());
//! ```

use std::collections::HashMap;
use std::fmt::Write as _;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

use super::asr_backend::{TranscriptSegment, TranscriptionResult};

// ─── Domain types ─────────────────────────────────────────────────────────────

/// The kind of reference detected in a transcript chunk.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    /// Academic paper or publication.
    Paper,
    /// Named individual (researcher, public figure, etc.).
    Person,
    /// Software tool, library, or service.
    Tool,
    /// Factual claim that can be fact-checked.
    Claim,
    /// Numeric statistic or measurement that can be verified.
    Number,
    /// Anything else that might be useful to look up.
    Other,
}

/// A reference identified by the LLM inside a transcript chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// What kind of thing is being referenced.
    pub kind: ReferenceKind,
    /// The lookup query string (e.g., `"Dijkstra 1968 GOTO considered harmful"`).
    pub query: String,
    /// LLM-reported confidence that this is worth following up, in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Index into [`TranscriptionResult::segments`] where this reference appears.
    pub segment_idx: usize,
}

/// The result of fetching and summarising one reference URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LookupResult {
    /// The URL that was fetched.
    pub url: String,
    /// LLM-generated summary of the fetched content, focused on the query.
    pub summary: String,
    /// UTC timestamp when the lookup was performed.
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

// ─── Configuration ────────────────────────────────────────────────────────────

/// Tunables for the active-reading pipeline.
///
/// All fields have safe defaults — see [`ActiveReadingConfig::default`].
#[derive(Debug, Clone)]
pub struct ActiveReadingConfig {
    /// Hard cap on LLM tokens consumed by all sampling calls combined.
    pub token_budget: u32,
    /// Maximum recursion depth for reference lookups (≥1 means no recursion).
    pub max_depth: u32,
    /// Only follow references whose `confidence` meets this threshold.
    pub confidence_threshold: f32,
    /// Maximum references to follow per transcript segment.
    pub max_refs_per_segment: usize,
    /// Timeout in seconds for each individual URL fetch.
    pub lookup_timeout_secs: u64,
    /// How many days a cached lookup result remains valid.
    pub cache_ttl_days: u64,
    /// Whitelist of reference kinds to follow; others are silently skipped.
    pub allowed_kinds: Vec<ReferenceKind>,
}

impl Default for ActiveReadingConfig {
    fn default() -> Self {
        Self {
            token_budget: 10_000,
            max_depth: 1,
            confidence_threshold: 0.7,
            max_refs_per_segment: 3,
            lookup_timeout_secs: 10,
            cache_ttl_days: 7,
            allowed_kinds: vec![
                ReferenceKind::Paper,
                ReferenceKind::Person,
                ReferenceKind::Tool,
                ReferenceKind::Claim,
            ],
        }
    }
}

// ─── Output ───────────────────────────────────────────────────────────────────

/// Metrics collected during an active-reading pass.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActiveReadingMetadata {
    /// Total references the LLM identified across all chunks.
    pub references_identified: usize,
    /// References that survived filtering and were actually fetched.
    pub references_followed: usize,
    /// Approximate LLM tokens consumed (sampling calls only).
    pub tokens_spent: u32,
    /// Cache hits avoided during the run.
    pub cache_hits: usize,
    /// Wall-clock time for the entire active-reading pass, in milliseconds.
    pub elapsed_ms: u64,
}

/// Everything produced by a successful active-reading pass.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActiveReadingOutput {
    /// Formatted footnote strings, e.g. `"[1] Dijkstra 1968 — https://..."`.
    pub footnotes: Vec<String>,
    /// Pipeline metrics.
    pub metadata: ActiveReadingMetadata,
}

// ─── Error ────────────────────────────────────────────────────────────────────

/// Errors that can occur during active reading.
#[derive(Error, Debug)]
pub enum ActiveReadingError {
    /// The connected MCP client does not support `sampling/createMessage`.
    #[error("LLM sampling not supported by client")]
    SamplingNotSupported,
    /// All allocated LLM tokens have been consumed.
    #[error("token budget exhausted")]
    BudgetExhausted,
    /// A URL fetch did not complete within the configured timeout.
    #[error("lookup timeout: {0}")]
    Timeout(String),
    /// The sampling round-trip failed.
    #[error("sampling error: {0}")]
    SamplingFailed(String),
    /// A URL fetch failed.
    #[error("fetch error: {0}")]
    FetchFailed(String),
    /// The LLM returned something we could not parse.
    #[error("invalid response from LLM: {0}")]
    InvalidResponse(String),
}

/// Convenience alias used throughout this module.
pub type Result<T> = std::result::Result<T, ActiveReadingError>;

// ─── Traits ───────────────────────────────────────────────────────────────────

/// Ask the host LLM to identify or summarise content.
///
/// The MCP-specific implementation lives in [`super::active_reading_mcp::McpLlmSampler`].
/// Tests inject a simple mock.
#[async_trait]
pub trait LlmSampler: Send + Sync {
    /// Identify references in `chunk` that warrant lookup.
    ///
    /// `segment_offset` is the index of the first segment in the chunk —
    /// implementations should store it in each returned [`Reference::segment_idx`].
    async fn identify_references(
        &self,
        chunk: &str,
        segment_offset: usize,
    ) -> Result<Vec<Reference>>;

    /// Summarise `content` in at most `max_tokens` tokens, focused on `query`.
    async fn summarize(&self, content: &str, query: &str, max_tokens: u32) -> Result<String>;
}

/// Retrieve the text content of a URL.
///
/// The production implementation wraps [`nab::AcceleratedClient::fetch_text`].
#[async_trait]
pub trait UrlFetcher: Send + Sync {
    /// Fetch `url` and return its plain-text (or markdown) body.
    async fn fetch_text(&self, url: &str) -> Result<String>;
}

// ─── Chunking helpers ─────────────────────────────────────────────────────────

/// Target chunk size in characters before overlap is added.
const CHUNK_SIZE_CHARS: usize = 4_000;
/// Overlap between consecutive chunks so boundary references aren't missed.
const CHUNK_OVERLAP_CHARS: usize = 200;

// ─── ActiveReader ─────────────────────────────────────────────────────────────

/// Drives the active-reading pass over a completed [`TranscriptionResult`].
pub struct ActiveReader<'a> {
    sampler: &'a dyn LlmSampler,
    fetcher: &'a dyn UrlFetcher,
    config: ActiveReadingConfig,
    /// In-memory cache keyed by `(kind, normalised_query)`.
    cache: HashMap<(ReferenceKind, String), LookupResult>,
}

impl<'a> ActiveReader<'a> {
    /// Create a fresh reader with an empty cache.
    pub fn new(
        sampler: &'a dyn LlmSampler,
        fetcher: &'a dyn UrlFetcher,
        config: ActiveReadingConfig,
    ) -> Self {
        Self {
            sampler,
            fetcher,
            config,
            cache: HashMap::new(),
        }
    }

    /// Pre-seed the reader with a previously built cache.
    #[must_use]
    pub fn with_cache(mut self, cache: HashMap<(ReferenceKind, String), LookupResult>) -> Self {
        self.cache = cache;
        self
    }

    /// Process a full transcription, identifying and inlining references.
    ///
    /// Segments in `transcript` may have `[N]` markers appended to their `text`
    /// where references were found. The returned [`ActiveReadingOutput`] holds the
    /// corresponding footnote strings and pipeline metadata.
    pub async fn process(
        &mut self,
        transcript: &mut TranscriptionResult,
    ) -> Result<ActiveReadingOutput> {
        let started = std::time::Instant::now();
        let mut metadata = ActiveReadingMetadata::default();
        let mut footnotes: Vec<String> = Vec::new();
        // Track how many refs have already been attached to each segment index.
        let mut refs_per_segment: HashMap<usize, usize> = HashMap::new();

        if transcript.segments.is_empty() {
            return Ok(ActiveReadingOutput {
                footnotes,
                metadata,
            });
        }

        let chunks = self.chunk_segments(&transcript.segments);
        debug!(chunks = chunks.len(), "active reading: chunked transcript");

        for (offset, chunk_text) in &chunks {
            if metadata.tokens_spent >= self.config.token_budget {
                info!("active reading: token budget exhausted, stopping");
                break;
            }

            let refs = match self.sampler.identify_references(chunk_text, *offset).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("active reading: identify_references failed: {e}");
                    continue;
                }
            };
            // Rough token estimate: ~2 tokens per word in prompt + response.
            metadata.tokens_spent = metadata
                .tokens_spent
                .saturating_add(estimate_tokens(chunk_text));

            metadata.references_identified += refs.len();
            debug!(
                count = refs.len(),
                offset, "active reading: references identified in chunk"
            );

            for reference in refs {
                if !self.should_follow(&reference, &refs_per_segment) {
                    continue;
                }

                let lookup = match self.lookup_reference(&reference).await {
                    Ok(l) => l,
                    Err(e) => {
                        warn!(query = %reference.query, "active reading: lookup failed: {e}");
                        continue;
                    }
                };

                metadata.tokens_spent = metadata
                    .tokens_spent
                    .saturating_add(estimate_tokens(&lookup.summary));
                metadata.references_followed += 1;

                let fn_num = footnotes.len() + 1;
                // Safety: segment_idx was produced by chunk_segments, which only
                // yields indices in [0, segments.len()), so the index is valid.
                if let Some(seg) = transcript.segments.get_mut(reference.segment_idx) {
                    let _ = write!(seg.text, "[{fn_num}]");
                }

                footnotes.push(format!("[{fn_num}] {} — {}", lookup.summary, lookup.url));

                *refs_per_segment.entry(reference.segment_idx).or_insert(0) += 1;
            }
        }

        metadata.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        info!(
            identified = metadata.references_identified,
            followed = metadata.references_followed,
            tokens = metadata.tokens_spent,
            elapsed_ms = metadata.elapsed_ms,
            "active reading complete"
        );

        Ok(ActiveReadingOutput {
            footnotes,
            metadata,
        })
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Split segments into overlapping text chunks of ~[`CHUNK_SIZE_CHARS`] chars.
    ///
    /// Returns `Vec<(starting_segment_idx, chunk_text)>`.
    // `&self` is kept so callers can access config-driven chunk sizes in the future.
    #[allow(clippy::unused_self)]
    pub(crate) fn chunk_segments(&self, segments: &[TranscriptSegment]) -> Vec<(usize, String)> {
        let mut chunks: Vec<(usize, String)> = Vec::new();
        let mut current = String::new();
        let mut chunk_start_idx: usize = 0;

        for (idx, seg) in segments.iter().enumerate() {
            if current.len() + seg.text.len() > CHUNK_SIZE_CHARS && !current.is_empty() {
                // Save overlap tail before flushing so references at chunk
                // boundaries aren't lost.
                let tail: String = if current.len() > CHUNK_OVERLAP_CHARS {
                    current[current.len() - CHUNK_OVERLAP_CHARS..].to_string()
                } else {
                    current.clone()
                };
                chunks.push((chunk_start_idx, current.clone()));
                current = tail;
                chunk_start_idx = idx;
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&seg.text);
        }

        if !current.is_empty() {
            chunks.push((chunk_start_idx, current));
        }

        chunks
    }

    /// Build a lookup URL for a reference based on its kind.
    pub(crate) fn url_for_reference(reference: &Reference) -> Result<String> {
        let q = urlencoding::encode(&reference.query);
        let url = match reference.kind {
            ReferenceKind::Paper => {
                format!("https://scholar.google.com/scholar?q={q}")
            }
            ReferenceKind::Person => {
                format!("https://en.wikipedia.org/wiki/Special:Search?search={q}")
            }
            ReferenceKind::Tool => {
                format!("https://github.com/search?q={q}&type=repositories")
            }
            ReferenceKind::Claim | ReferenceKind::Other => {
                format!("https://www.google.com/search?q={q}")
            }
            ReferenceKind::Number => {
                return Err(ActiveReadingError::FetchFailed(
                    "numbers do not look up well".to_string(),
                ));
            }
        };
        Ok(url)
    }

    /// Fetch a reference, using the in-memory cache to avoid redundant requests.
    async fn lookup_reference(&mut self, reference: &Reference) -> Result<LookupResult> {
        let cache_key = (reference.kind, reference.query.to_lowercase());

        // Cache hit — check TTL.
        if let Some(cached) = self.cache.get(&cache_key) {
            let age_days = chrono::Utc::now()
                .signed_duration_since(cached.fetched_at)
                .num_days();
            if age_days < i64::try_from(self.config.cache_ttl_days).unwrap_or(i64::MAX) {
                debug!(query = %reference.query, "active reading: cache hit");
                return Ok(cached.clone());
            }
        }

        let url = Self::url_for_reference(reference)?;
        debug!(url = %url, "active reading: fetching reference");

        let fetch_result = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.lookup_timeout_secs),
            self.fetcher.fetch_text(&url),
        )
        .await
        .map_err(|_| ActiveReadingError::Timeout(url.clone()))?;

        let content = fetch_result?;

        let summary = self
            .sampler
            .summarize(&content, &reference.query, 200)
            .await?;

        let result = LookupResult {
            url,
            summary,
            fetched_at: chrono::Utc::now(),
        };

        self.cache.insert(cache_key, result.clone());
        Ok(result)
    }

    /// Return `true` if this reference passes all filters and should be followed.
    fn should_follow(
        &self,
        reference: &Reference,
        refs_per_segment: &HashMap<usize, usize>,
    ) -> bool {
        if reference.confidence < self.config.confidence_threshold {
            return false;
        }
        if !self.config.allowed_kinds.contains(&reference.kind) {
            return false;
        }
        let count = refs_per_segment
            .get(&reference.segment_idx)
            .copied()
            .unwrap_or(0);
        if count >= self.config.max_refs_per_segment {
            return false;
        }
        true
    }
}

/// Rough token estimate — ~4 characters per token is close enough for budgeting.
fn estimate_tokens(text: &str) -> u32 {
    u32::try_from((text.len() / 4).min(u32::MAX as usize)).unwrap_or(u32::MAX)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    // ── Mock implementations ──────────────────────────────────────────────────

    /// Deterministic mock sampler for testing.
    struct MockSampler {
        /// Fixed list of references returned on every `identify_references` call.
        refs: Vec<Reference>,
        /// Fixed summary string returned on every `summarize` call.
        summary: String,
        /// Counts how many times each method was called.
        identify_calls: Mutex<usize>,
        summarize_calls: Mutex<usize>,
    }

    impl MockSampler {
        fn new(refs: Vec<Reference>, summary: impl Into<String>) -> Self {
            Self {
                refs,
                summary: summary.into(),
                identify_calls: Mutex::new(0),
                summarize_calls: Mutex::new(0),
            }
        }

        fn identify_call_count(&self) -> usize {
            *self.identify_calls.lock().unwrap()
        }

    }

    #[async_trait]
    impl LlmSampler for MockSampler {
        async fn identify_references(
            &self,
            _chunk: &str,
            segment_offset: usize,
        ) -> Result<Vec<Reference>> {
            *self.identify_calls.lock().unwrap() += 1;
            Ok(self
                .refs
                .iter()
                .cloned()
                .map(|mut r| {
                    r.segment_idx = segment_offset;
                    r
                })
                .collect())
        }

        async fn summarize(
            &self,
            _content: &str,
            _query: &str,
            _max_tokens: u32,
        ) -> Result<String> {
            *self.summarize_calls.lock().unwrap() += 1;
            Ok(self.summary.clone())
        }
    }

    /// Mock fetcher that returns a fixed body.
    struct MockFetcher {
        body: String,
        call_count: Mutex<usize>,
        should_fail: bool,
    }

    impl MockFetcher {
        fn new(body: impl Into<String>) -> Self {
            Self {
                body: body.into(),
                call_count: Mutex::new(0),
                should_fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                body: String::new(),
                call_count: Mutex::new(0),
                should_fail: true,
            }
        }

        fn call_count(&self) -> usize {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl UrlFetcher for MockFetcher {
        async fn fetch_text(&self, _url: &str) -> Result<String> {
            *self.call_count.lock().unwrap() += 1;
            if self.should_fail {
                return Err(ActiveReadingError::FetchFailed("mock fail".into()));
            }
            Ok(self.body.clone())
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_segment(text: &str) -> TranscriptSegment {
        TranscriptSegment {
            text: text.to_string(),
            start: 0.0,
            end: 1.0,
            confidence: 0.95,
            language: None,
            speaker: None,
            words: None,
        }
    }

    fn make_transcript(texts: &[&str]) -> TranscriptionResult {
        TranscriptionResult {
            segments: texts.iter().map(|t| make_segment(t)).collect(),
            language: "en".to_string(),
            duration_seconds: 10.0,
            model: "test".to_string(),
            backend: "test".to_string(),
            rtfx: 1.0,
            processing_time_seconds: 1.0,
            speakers: None,
            footnotes: None,
            active_reading: None,
        }
    }

    fn high_confidence_paper_ref() -> Reference {
        Reference {
            kind: ReferenceKind::Paper,
            query: "Dijkstra 1968 GOTO".to_string(),
            confidence: 0.95,
            segment_idx: 0,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// `chunk_segments` returns one chunk when text fits within the limit.
    #[test]
    fn chunk_segments_small_transcript_produces_single_chunk() {
        // GIVEN a tiny transcript
        let segs: Vec<TranscriptSegment> = vec![make_segment("Hello world.")];
        let config = ActiveReadingConfig::default();
        let sampler = MockSampler::new(vec![], "");
        let fetcher = MockFetcher::new("");
        let reader = ActiveReader::new(&sampler, &fetcher, config);

        // WHEN chunked
        let chunks = reader.chunk_segments(&segs);

        // THEN exactly one chunk is produced starting at index 0
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, 0);
        assert!(chunks[0].1.contains("Hello world."));
    }

    /// `chunk_segments` splits large transcripts and respects character limit.
    #[test]
    fn chunk_segments_respects_word_count() {
        // GIVEN segments that together exceed the chunk size
        let long_word = "word ".repeat(200); // 1000 chars each
        let segs: Vec<TranscriptSegment> =
            (0..10).map(|_| make_segment(long_word.trim())).collect();
        let config = ActiveReadingConfig::default();
        let sampler = MockSampler::new(vec![], "");
        let fetcher = MockFetcher::new("");
        let reader = ActiveReader::new(&sampler, &fetcher, config);

        // WHEN chunked
        let chunks = reader.chunk_segments(&segs);

        // THEN multiple chunks are produced
        assert!(
            chunks.len() >= 2,
            "expected ≥2 chunks, got {}",
            chunks.len()
        );
        // Each chunk must be non-empty
        for (_, text) in &chunks {
            assert!(!text.is_empty());
        }
    }

    /// `url_for_reference` routes Paper → Google Scholar.
    #[test]
    fn url_for_reference_paper_uses_scholar() {
        // GIVEN a paper reference
        let r = high_confidence_paper_ref();

        // WHEN the URL is built
        let url = ActiveReader::url_for_reference(&r).unwrap();

        // THEN it points to Google Scholar
        assert!(
            url.starts_with("https://scholar.google.com/scholar?q="),
            "got {url}"
        );
        assert!(url.contains("Dijkstra"));
    }

    /// `url_for_reference` routes Person → Wikipedia search.
    #[test]
    fn url_for_reference_person_uses_wikipedia() {
        // GIVEN a person reference
        let r = Reference {
            kind: ReferenceKind::Person,
            query: "Geoffrey Hinton".to_string(),
            confidence: 0.9,
            segment_idx: 0,
        };

        // WHEN the URL is built
        let url = ActiveReader::url_for_reference(&r).unwrap();

        // THEN it points to Wikipedia special search
        assert!(
            url.starts_with("https://en.wikipedia.org/wiki/Special:Search?search="),
            "got {url}"
        );
    }

    /// `url_for_reference` returns an error for Number (numbers don't look up well).
    #[test]
    fn url_for_reference_number_returns_error() {
        // GIVEN a number reference
        let r = Reference {
            kind: ReferenceKind::Number,
            query: "42".to_string(),
            confidence: 0.8,
            segment_idx: 0,
        };

        // WHEN the URL is built
        let result = ActiveReader::url_for_reference(&r);

        // THEN it's an error
        assert!(result.is_err());
    }

    /// References below the confidence threshold are not followed.
    #[tokio::test]
    async fn process_skips_below_threshold() {
        // GIVEN a reference with confidence below the default 0.7 threshold
        let low_conf_ref = Reference {
            kind: ReferenceKind::Paper,
            query: "obscure thing".to_string(),
            confidence: 0.3,
            segment_idx: 0,
        };
        let sampler = MockSampler::new(vec![low_conf_ref], "summary");
        let fetcher = MockFetcher::new("content");
        let mut reader = ActiveReader::new(&sampler, &fetcher, ActiveReadingConfig::default());
        let mut transcript = make_transcript(&["Some text."]);

        // WHEN processed
        let output = reader.process(&mut transcript).await.unwrap();

        // THEN no footnotes are added and the fetcher is never called
        assert!(output.footnotes.is_empty());
        assert_eq!(fetcher.call_count(), 0);
    }

    /// At most `max_refs_per_segment` references are followed per segment.
    #[tokio::test]
    async fn process_caps_refs_per_segment() {
        // GIVEN 5 high-confidence paper references all pointing at segment 0
        let refs: Vec<Reference> = (0..5)
            .map(|i| Reference {
                kind: ReferenceKind::Paper,
                query: format!("paper {i}"),
                confidence: 0.95,
                segment_idx: 0,
            })
            .collect();
        let sampler = MockSampler::new(refs, "summary");
        let fetcher = MockFetcher::new("content");
        let mut config = ActiveReadingConfig::default();
        config.max_refs_per_segment = 2;
        let mut reader = ActiveReader::new(&sampler, &fetcher, config);
        let mut transcript = make_transcript(&["Segment zero."]);

        // WHEN processed
        let output = reader.process(&mut transcript).await.unwrap();

        // THEN at most max_refs_per_segment footnotes are produced for that segment
        assert!(
            output.footnotes.len() <= 2,
            "expected ≤2 footnotes, got {}",
            output.footnotes.len()
        );
    }

    /// Second lookup of the same query is served from the cache.
    #[tokio::test]
    async fn process_uses_cache_on_repeat() {
        // GIVEN two identical references in two chunks (simulated via two segments)
        let refs = vec![Reference {
            kind: ReferenceKind::Paper,
            query: "same paper".to_string(),
            confidence: 0.9,
            segment_idx: 0,
        }];
        let sampler = MockSampler::new(refs, "cached summary");
        let fetcher = MockFetcher::new("content");
        let mut reader = ActiveReader::new(&sampler, &fetcher, ActiveReadingConfig::default());
        // Two segments — the mock sampler attaches the ref to the first segment of each chunk.
        // With tiny segments a second chunk is unlikely, so we directly call lookup twice.
        let ref1 = Reference {
            kind: ReferenceKind::Paper,
            query: "same paper".to_string(),
            confidence: 0.9,
            segment_idx: 0,
        };

        // WHEN the same reference is looked up twice
        let _first = reader.lookup_reference(&ref1).await.unwrap();
        let second = reader.lookup_reference(&ref1).await.unwrap();

        // THEN the fetcher is called only once (second is a cache hit)
        assert_eq!(
            fetcher.call_count(),
            1,
            "fetcher should be called once; cache should serve second"
        );
        assert_eq!(second.summary, "cached summary");
    }

    /// A failed lookup for one reference does not abort the whole pipeline.
    #[tokio::test]
    async fn process_continues_on_lookup_failure() {
        // GIVEN two references: one good, one bad URL (fetcher fails)
        let refs = vec![
            Reference {
                kind: ReferenceKind::Paper,
                query: "good paper".to_string(),
                confidence: 0.9,
                segment_idx: 0,
            },
            Reference {
                kind: ReferenceKind::Tool,
                query: "bad tool".to_string(),
                confidence: 0.9,
                segment_idx: 0,
            },
        ];
        let sampler = MockSampler::new(refs, "summary");
        let fetcher = MockFetcher::failing(); // always fails
        let mut reader = ActiveReader::new(&sampler, &fetcher, ActiveReadingConfig::default());
        let mut transcript = make_transcript(&["Some text mentioning a paper and a tool."]);

        // WHEN processed
        let output = reader.process(&mut transcript).await;

        // THEN the pipeline does not error — it just produces fewer (or zero) footnotes
        assert!(output.is_ok(), "process should not propagate lookup errors");
        // No footnotes because fetcher always fails
        assert!(output.unwrap().footnotes.is_empty());
    }

    /// Token budget stops the pipeline before processing all chunks.
    #[tokio::test]
    async fn process_respects_token_budget() {
        // GIVEN a very tight budget of 1 token
        let refs = vec![high_confidence_paper_ref()];
        let sampler = MockSampler::new(refs, "summary");
        let fetcher = MockFetcher::new("content");
        let mut config = ActiveReadingConfig::default();
        config.token_budget = 1; // exhausted immediately after the first chunk
        let mut reader = ActiveReader::new(&sampler, &fetcher, config);
        // 20 segments, each ~100 chars — multiple chunks
        let texts: Vec<&str> = (0..20)
            .map(|_| "This is a sentence that mentions Dijkstra.")
            .collect();
        let mut transcript = make_transcript(&texts);

        // WHEN processed
        let output = reader.process(&mut transcript).await.unwrap();

        // THEN the sampler is called only once (budget stops after first chunk)
        assert_eq!(
            sampler.identify_call_count(),
            1,
            "expected 1 sampling call before budget was exhausted"
        );
        let _ = output; // footnotes may or may not exist depending on timing
    }

    /// References of disallowed kinds are silently skipped.
    #[tokio::test]
    async fn process_skips_disallowed_kinds() {
        // GIVEN a number reference and a config that doesn't allow numbers
        let refs = vec![Reference {
            kind: ReferenceKind::Number,
            query: "3.14".to_string(),
            confidence: 0.9,
            segment_idx: 0,
        }];
        let sampler = MockSampler::new(refs, "summary");
        let fetcher = MockFetcher::new("content");
        let config = ActiveReadingConfig::default();
        // Default config does not include Number
        let mut reader = ActiveReader::new(&sampler, &fetcher, config);
        let mut transcript = make_transcript(&["The value is 3.14."]);

        // WHEN processed
        let output = reader.process(&mut transcript).await.unwrap();

        // THEN no footnote and no fetch
        assert!(output.footnotes.is_empty());
        assert_eq!(fetcher.call_count(), 0);
    }

    /// An empty transcript produces an empty output without errors.
    #[tokio::test]
    async fn process_empty_transcript_returns_empty_output() {
        // GIVEN an empty transcript
        let sampler = MockSampler::new(vec![], "");
        let fetcher = MockFetcher::new("");
        let mut reader = ActiveReader::new(&sampler, &fetcher, ActiveReadingConfig::default());
        let mut transcript = make_transcript(&[]);

        // WHEN processed
        let output = reader.process(&mut transcript).await.unwrap();

        // THEN nothing is produced
        assert!(output.footnotes.is_empty());
        assert_eq!(output.metadata.references_identified, 0);
    }

    /// Footnote markers are appended to the correct segment text.
    #[tokio::test]
    async fn process_appends_footnote_marker_to_segment_text() {
        // GIVEN one paper reference for segment 0
        let refs = vec![Reference {
            kind: ReferenceKind::Paper,
            query: "Dijkstra GOTO".to_string(),
            confidence: 0.95,
            segment_idx: 0,
        }];
        let sampler = MockSampler::new(refs, "Dijkstra, 1968 paper summary");
        let fetcher = MockFetcher::new("full content");
        let mut reader = ActiveReader::new(&sampler, &fetcher, ActiveReadingConfig::default());
        let mut transcript = make_transcript(&["Dijkstra's famous paper."]);

        // WHEN processed
        let output = reader.process(&mut transcript).await.unwrap();

        // THEN segment 0 text ends with [1] and footnote 1 is populated
        assert_eq!(output.footnotes.len(), 1);
        assert!(
            transcript.segments[0].text.contains("[1]"),
            "segment text should contain footnote marker, got: {}",
            transcript.segments[0].text
        );
        assert!(output.footnotes[0].starts_with("[1]"));
    }
}
