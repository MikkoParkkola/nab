# nab MCP — active reading via cross-tool sampling

**Status**: design
**Date**: 2026-04-07
**Phase**: 1.5 (after analyze v2)
**Novelty**: novel for ASR pipelines; produces *fundamentally better* transcripts than passive transcription

## The problem

Standard ASR (Whisper, Otter, FluidAudio, etc.) transcribes passively — you get a wall of text, but every time the speaker mentions something — a paper, a person, a number, a quote, a tool — the listener has to manually look it up. The transcript is a dead artifact.

A good *human* listener doesn't transcribe passively. They:
- Notice when something is referenced
- Pause, look it up
- Cross-check the claim
- Take notes with citations
- Mark uncertain claims for fact-checking later

That's "active reading" applied to listening. No ASR pipeline does this. It's possible *only because of MCP sampling*.

## How MCP sampling enables it

MCP 2025-11-25 has `sampling/createMessage` — the server can ask the *host LLM* to generate text on demand. Combined with the host's other tools (web fetch, search, semantic search), this lets nab *recursively analyze its own transcripts as they are produced*.

The flow:

```
1. nab analyze starts transcribing video
2. After every N segments OR every M seconds, nab sends the transcript chunk to the host LLM via sampling/createMessage:
   "Identify any references in this segment that warrant lookup. 
    Return JSON: {refs: [{type, query, confidence}]}"
3. Host LLM returns: 
   {refs: [
     {type: "paper", query: "Dijkstra 1968 GOTO considered harmful", confidence: 0.95},
     {type: "person", query: "Geoffrey Hinton", confidence: 0.9},
     {type: "claim", query: "data center water usage Memphis", confidence: 0.7}
   ]}
4. For each ref above the confidence threshold, nab calls itself:
   - paper → nab fetch "https://scholar.google.com/scholar?q=..."
   - person → nab fetch "https://en.wikipedia.org/wiki/..."
   - claim → nab fetch "https://www.google.com/search?q=..."
5. nab inlines the lookup result as a footnote in the transcript:
   "...as Dijkstra showed in his famous 1968 paper¹..."
   "[1] Dijkstra, E.W. 'Go To Statement Considered Harmful' (1968), Comm. ACM 11(3): 147-148."
6. At end of transcription, nab also passes the entire transcript + lookups back to the host LLM via sampling for a final coherent summary.
```

## Why this is genuinely novel

| System | Active reading? | Why not |
|---|---|---|
| Whisper.cpp | ❌ | No agency, just CTC decoder |
| OpenAI Whisper | ❌ | Same |
| AWS Transcribe | ❌ | Cloud service, no callback to LLM |
| Google STT | ❌ | Same |
| Otter.ai | Partial | Has summarization, no live lookup |
| Krisp / Read.ai | ❌ | Post-hoc only |
| Notion Voice | ❌ | Post-hoc only |
| Apple Voice Memos | ❌ | OS-level, no LLM |
| **nab analyze + sampling** | ✅ | **First to combine ASR with MCP sampling** |

The key insight: **MCP sampling means a tool server can reach back to its caller's LLM**. Until 2025-11-25, this was a one-way street (LLM → tool). Now the tool can act as an active research participant.

## Architecture

### Modes

```bash
nab analyze video.mp4                          # passive (Phase 1)
nab analyze video.mp4 --active                 # active reading (Phase 1.5)
nab analyze video.mp4 --active --budget 5000   # cap LLM tokens spent on lookups
nab analyze video.mp4 --active --depth 2       # recursive lookup (lookups can spawn more lookups)
```

### Token budgets

Active reading is expensive — every lookup is one sampling round-trip plus N fetch calls plus another round-trip for inline integration. Budget:

| Setting | Default | Rationale |
|---|---|---|
| `--budget` | 10000 tokens | Hard cap on LLM tokens consumed by reference identification + integration |
| `--depth` | 1 | Don't recursively look up references in lookups |
| `--threshold` | 0.7 | Only follow refs with confidence ≥ 0.7 |
| `--max-refs-per-segment` | 3 | Avoid runaway lookups |
| `--lookup-timeout` | 10s | Each fetch call has 10s budget |
| `--cache-ttl` | 7d | Don't re-look-up the same thing within a week |

### Cache layer

Reference lookups cache by (type, normalized query) to avoid hammering external sources:

```
~/.local/share/nab/active-reading-cache/
  papers/
    sha256(query).json
  people/
    sha256(query).json
  claims/
    sha256(query).json
```

Cache hit rate is high in practice — the same paper gets referenced across many videos.

### Failure modes (devil's advocate)

| Failure | Mitigation |
|---|---|
| Sampling not supported by host | Detect via `initialize` capabilities; degrade to passive transcription with a warning |
| Token budget exhausted mid-video | Stop active reading, finish passive |
| Wrong reference identified | Threshold + user can disable per-type via `--types papers,people` |
| Ambiguous "person" name (e.g., "Karen") | Use surrounding context window for disambiguation |
| Adversarial transcript (LLM injection in audio) | Sanitize before passing to sampling — strip URLs, code blocks, special tokens |
| Infinite recursion | `--depth 1` default, hard cap at 3 |
| Cost explosion | Token budget hard cap, log spend after each segment |

### Privacy

Active reading sends transcript chunks to the host LLM. The user has already consented to this by running the analysis through an MCP host (Claude Code, Continue, etc.). Add a `--no-active` flag to opt out per-call. Add a `nab config set active_reading.default false` to opt out globally.

## Implementation

### MCP capability advertisement

In `nab/src/bin/mcp_server/main.rs`, the server doesn't advertise sampling — that's a *client* capability. nab needs to *check* whether the connected client supports sampling before attempting it. The `rust-mcp-sdk` runtime exposes the negotiated client capabilities; nab queries them via `runtime.peer_capabilities()`.

If `client.sampling` is None → fall back to passive transcription with warning logged via `notifications/message`.

If `client.sampling` is Some → enable active reading.

### Sampling call flow

```rust
pub async fn identify_references(
    runtime: Arc<dyn McpServer>,
    transcript_chunk: &str,
) -> Result<Vec<Reference>> {
    let messages = vec![SamplingMessage {
        role: Role::User,
        content: SamplingContent::Text(format!(
            "You are analyzing a video transcript. Identify references that warrant lookup.\n\
             Return JSON: {{\"refs\": [{{\"type\": \"paper|person|claim|tool|number\", \
             \"query\": \"...\", \"confidence\": 0.0-1.0}}]}}\n\
             Transcript chunk:\n{}",
            transcript_chunk
        )),
    }];
    
    let request = CreateMessageRequestParams {
        messages,
        max_tokens: 500,
        model_preferences: Some(ModelPreferences {
            cost_priority: Some(0.8),  // prefer cheap
            speed_priority: Some(0.7),
            intelligence_priority: Some(0.5),
            ..Default::default()
        }),
        system_prompt: Some("You identify references in transcripts. Be conservative — only flag concrete, lookupable items.".into()),
        include_context: Some(IncludeContext::None),
        ..Default::default()
    };
    
    let response = runtime.create_message(request).await?;
    parse_references(response.content)
}
```

### Lookup → fetch → inline

```rust
pub async fn lookup_reference(
    client: &AcceleratedClient,
    reference: &Reference,
) -> Result<LookupResult> {
    let url = match reference.kind {
        RefKind::Paper => format!("https://scholar.google.com/scholar?q={}", urlencoding::encode(&reference.query)),
        RefKind::Person => format!("https://en.wikipedia.org/wiki/Special:Search?search={}", urlencoding::encode(&reference.query)),
        RefKind::Tool => format!("https://github.com/search?q={}", urlencoding::encode(&reference.query)),
        RefKind::Claim => format!("https://www.google.com/search?q={}", urlencoding::encode(&reference.query)),
        RefKind::Number => return Ok(LookupResult::Skipped),  // numbers don't lookup well
    };
    
    let body = client.fetch_text(&url).await?;
    let summary = summarize(&body, &reference.query, 200)?;  // ~200 tokens summary
    Ok(LookupResult::Found { url, summary })
}
```

### Citation insertion

```rust
pub fn insert_citations(
    transcript: &mut TranscriptionResult,
    references: &[(usize, Reference, LookupResult)],
) {
    let mut footnotes = Vec::new();
    for (segment_idx, reference, result) in references {
        if let LookupResult::Found { url, summary } = result {
            let footnote_num = footnotes.len() + 1;
            transcript.segments[*segment_idx].text.push_str(&format!("[{}]", footnote_num));
            footnotes.push(format!("[{}] {} — {}", footnote_num, summary, url));
        }
    }
    transcript.footnotes = Some(footnotes);
}
```

## Output format change

`TranscriptionResult` gets two optional fields:

```rust
pub struct TranscriptionResult {
    // ... existing fields ...
    
    /// Inline reference footnotes (when --active enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footnotes: Option<Vec<String>>,
    
    /// Active reading metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_reading: Option<ActiveReadingMetadata>,
}

pub struct ActiveReadingMetadata {
    pub references_identified: usize,
    pub references_followed: usize,
    pub tokens_spent: usize,
    pub cache_hits: usize,
    pub elapsed_ms: u64,
}
```

## Cargo deps

Already present:
- `reqwest`
- `serde_json`
- `tokio`
- `urlencoding`

New:
- nothing — pure plumbing

## Tests

- Unit: reference parser handles malformed JSON gracefully
- Unit: token budget tracking
- Unit: cache hit / miss
- Integration: mock sampling server, verify call shape
- E2E (manual): real video with `--active`, verify references appear

## Ship plan

Single PR after URL watch lands (Phase 1.5b):
- ~500 lines Rust core
- ~100 lines tests
- Docs (CHANGELOG, README, design doc this file)
- Add `--active` flag to CLI
- Cargo.toml — no new deps

## Patent assessment

The pattern "ASR pipeline that uses MCP sampling to actively look up references via additional MCP tools and inlines citations into the output transcript" appears novel as of April 2026.

Prior art search:
- ❌ Whisper, Otter, AWS Transcribe — passive
- ❌ Notion AI Voice — post-hoc summarization, no active lookup
- ❌ Brilliant Labs — no LLM callback
- ✅ Closest analog: **Anthropic's MCP examples themselves** — but no ASR pipeline references this pattern

If this pattern matters strategically, file a provisional. Otherwise ship as open source and let it become standard.
