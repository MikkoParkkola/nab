# The sovereign multimodal stack

How [nab](https://github.com/MikkoParkkola/nab) and [hebb](https://github.com/MikkoParkkola/hebb) compose into a local-first AI stack that owns its data, runs on its own hardware, and depends on no commercial APIs in the default path.

## The thesis

The current shape of commercial AI is a dependency. You feed it your prompts, your documents, your conversations, your voice. It returns an answer. The pipeline crosses a network boundary, a billing boundary, a jurisdictional boundary, and a vendor boundary every single time.

The remedy is not to abandon AI. It is to recompose it locally, with open weights, open source, and an open protocol. That gives you four properties at once:

1. **Data sovereignty** — your files never leave your machine.
2. **Vendor independence** — swap any component without rewriting the rest.
3. **Latency** — local inference is consistently faster than a network round-trip.
4. **Cost** — zero per-query cost.

The Model Context Protocol (MCP) is the binding glue. As of the 2025-11-25 spec, MCP is rich enough to express almost everything an AI app needs: tools, resources, prompts, sampling, roots, elicitation, structured logging, completions, subscribable resources. Once two components both speak MCP, they compose without bespoke integration code.

This document is the design of one such composition.

## Three layers

### 1. Verb tool — nab

nab is the *doer*. It takes a verb and an object and returns a result.

| Verb | Object | Result |
|------|--------|--------|
| fetch | URL | clean markdown |
| analyze | audio / video | transcript with timestamps + speaker turns |
| watch | URL | subscribable resource that fires on change |
| OCR | image | extracted text |

nab is stateless from one invocation to the next. It does not remember. It does not learn. It is a thin, fast, well-instrumented set of HTTP and audio primitives.

### 2. Memory layer — hebb

hebb is the *rememberer*. Where nab is stateless, hebb is the long-term store. Its mission is personal memory across modalities — text, decisions, code patterns, voiceprints, key-value records, web snapshots.

The hebb stack:

- **BGE-M3 1024-dim embeddings**, INT8 quantized, ONNX Runtime, fully local. 100+ languages. ~50 ms per query.
- **SurrealDB embedded** with HNSW vector index + BM25 full-text search + RRF fusion.
- **Neuroscience-inspired write gates** — surprisal, MDL, contradiction detection.
- **MCP server** with 21+ tools across memory, voice, KV, decisions, replay, prospective memory.
- **Top-of-leaderboard memory recall** on LoCoMo: 70.67% F1, 83.2% Judge — #1 globally by 13 percentage points.

hebb does not know how to fetch a URL, transcribe a podcast, or run OCR. It is a memory primitive. It only knows how to remember and recall.

### 3. Inference primitives — fluidaudiocli, future BLAS layers

Some inference workloads are heavy enough to deserve dedicated processes. fluidaudiocli is the example: it is an external Swift binary that loads Parakeet TDT v3 weights and runs them on the Apple Neural Engine via CoreML, achieving 131× realtime on a 2-hour English audio file.

nab manages the lifecycle (`nab models fetch fluidaudio`), invokes fluidaudiocli as a subprocess, and parses its output. The choice to keep this as a separate binary instead of an in-process Rust binding is deliberate: ANE/CoreML access requires the Swift+ObjC runtime, and isolating it as a subprocess keeps nab's binary size small and its build hermetic.

Phase 3 will add `sherpa-onnx` (cross-platform ONNX backend, GPU + CPU + CoreML execution providers) and `whisper-rs` (universal fallback) using the same `nab models` lifecycle pattern. Phase 4 will add an optional VLM (vision-language model) layer via mistralrs running Qwen3-VL.

## Composition patterns

### Pattern 1: speaker identification

The hardest part of multi-speaker transcription is naming the speakers. ASR gives you `SPEAKER_00`, `SPEAKER_01`, but those labels are not stable across recordings — `SPEAKER_00` in your Karen Hao interview is `SPEAKER_03` in your Marc Andreessen interview. To name speakers reliably, you need a *voice identity* layer that survives across files.

Composition:

```text
nab analyze interview.mp4 --diarize --include-embeddings
        |
        v
[256-dim WeSpeaker vectors per speaker turn]
        |
        v
hebb voice_match (queries hebb's voiceprint table via cosine similarity)
        |
        v
{ "SPEAKER_00": "Karen Hao", "SPEAKER_01": "Mikko Parkkola" }
```

The first time you encounter a new speaker, hebb does not match. You enroll them once via `hebb voice_enroll <id> <name>`. From then on, every recording with that voice gets the name automatically — across all your interviews, podcasts, meeting recordings, and voice memos.

The MCP prompt `match-speakers-with-hebb` (shipped with nab) gives the host LLM exactly this workflow: call `analyze`, parse the embeddings, call `hebb voice_match` for each, and rewrite the transcript with named speakers.

### Pattern 2: personal sovereign web memory

Standard "save this URL" workflows go through Pocket, Instapaper, Readwise, Mem, or just a browser bookmark folder. Every one of these is a vendor dependency. The sovereign alternative:

```text
nab fetch URL  --->  hebb kv_set "urls:saved" URL <markdown body>
                 |
                 +->  hebb kv_set "urls:meta" URL { "title", "fetched_at", "source" }
```

Now your saved URLs live in your hebb namespace `urls:saved`. They are searchable via BGE-M3 semantic search (because hebb embeds them automatically when the namespace policy includes `embed=true`). They are never lost when a vendor pivots.

Querying back:

```text
hebb kv_search "urls:saved" "what did Anthropic say about constitutional AI"
        |
        v
[ranked URLs by semantic relevance]
```

This is the same primitive Pocket sells, but locally, with embeddings, with no vendor.

### Pattern 3: Apple Vision OCR in nab fetch

When `nab fetch` encounters a page with images, the OCR engine extracts text from each image and inlines it as alt-text-like annotations in the markdown:

```markdown
![architecture diagram](./img/diag.png)
> *(image text)* "API Gateway → Auth Service → Database. Three pods per AZ."
```

The OCR engine is `nab::content::ocr` — Apple Vision framework via objc2-vision, 15 languages, ANE accelerated, ~10-50 ms per image. macOS only at present; Phase 3 will add Tesseract for Linux and Windows.

This makes screenshot-heavy pages — engineering blogs, documentation with diagrams, status pages with charts — actually queryable instead of being opaque.

### Pattern 4: active reading via MCP sampling

Standard ASR transcribes passively. When the speaker mentions Dijkstra's 1968 GOTO paper, you get the words "Dijkstra showed in his famous 1968 paper" — and that's it. The transcript is a dead artifact.

A good *human* listener does not transcribe passively. They notice references, pause to look them up, take notes with citations, mark uncertain claims for fact-checking later. That's "active reading" applied to listening.

MCP sampling makes this possible *for the first time*. The MCP 2025-11-25 spec adds `sampling/createMessage`, which lets a tool server reach back to its caller's LLM and ask for a generation. nab uses this:

```text
1. nab analyze starts transcribing
2. After each segment, nab sends the chunk to the host LLM via sampling/createMessage:
     "Identify any references in this segment that warrant lookup."
3. LLM returns: [
     { type: "paper",  query: "Dijkstra 1968 GOTO considered harmful" },
     { type: "person", query: "Geoffrey Hinton" },
     { type: "claim",  query: "data center water usage Memphis" }
   ]
4. For each reference, nab calls itself: nab fetch <appropriate URL>
5. nab inlines the lookup as a footnote in the transcript
6. At end of transcription, nab passes the full transcript + lookups to the LLM
   for a final summary
```

This is novel. No existing ASR pipeline does it. Whisper.cpp has no LLM. OpenAI Whisper has no callback. AWS Transcribe is a one-shot cloud service. Otter.ai has post-hoc summarization but no live lookup. The combination of (a) MCP sampling and (b) nab being both an ASR tool *and* a fetch tool in the same MCP server is what unlocks active reading.

The compute model is fully sovereign as long as the host LLM is local — Claude Code with a local backend, Continue with a local model, etc. The lookups go through nab, which runs locally. The LLM round-trips never leave your machine.

## What is sovereign about this stack

Every default-path operation is local:

| Operation | Where it runs |
|-----------|---------------|
| HTML fetch | nab process, your machine |
| HTML → markdown | nab process, your machine |
| PDF extraction | pdfium, your machine |
| OCR | Apple Vision framework, your Apple Neural Engine |
| ASR | fluidaudiocli on your Apple Neural Engine |
| Speaker diarization | FluidAudio offline VBx, your machine |
| Embeddings | ONNX Runtime + BGE-M3, your machine |
| Vector search | SurrealDB + HNSW, your machine |
| Voice matching | hebb cosine similarity, your machine |
| Live URL watching | nab poller, your machine |
| MCP transport | stdio (in-process) or HTTP (localhost) |

There are no API keys required for any of the above. There are no SaaS dependencies. There are no usage quotas. There is no telemetry. The data path never crosses your machine boundary unless you explicitly fetch a remote URL.

## Benchmarks from this session

Real measurements from the 2026-04-07 development session, not vendor marketing:

| Workload | Hardware | Throughput |
|----------|----------|-----------|
| nab analyze on 2 h 09 m English audio (Karen Hao interview) | Apple Silicon, ANE | 59.6 s wall = 131× realtime |
| FluidAudio transcription confidence | (same) | 97.18 % mean |
| Audio extraction via ffmpeg | (same) | ~650× realtime |
| hebb LoCoMo benchmark | (same) | 70.67% F1, 83.2% Judge — #1 globally by 13pp |
| hebb query latency | (same) | ~50 ms BGE-M3 + HNSW |
| nab HTML → markdown (10 KB payload) | (same) | 14.5 MB/s |

These are not theoretical maxima. They are observed numbers from real files on a single MacBook.

## What is not sovereign yet

A few honest exceptions to flag:

- **Vision (frame description) backend** — nab analyze with `--vision` currently uses an optional Claude API call. Phase 4 replaces this with a local VLM (Qwen3-VL via mistralrs). Until then, vision frame analysis is the only non-local default.
- **OCR on Linux/Windows** — Apple Vision is macOS-only. Tesseract for Linux and Windows is in Phase 3.
- **fluidaudiocli on non-Apple platforms** — FluidAudio is Apple Silicon only by design (CoreML on ANE). Phase 3 adds sherpa-onnx (Parakeet ONNX, cross-platform) and whisper-rs (universal fallback) so non-Apple machines have a sovereign path.
- **Some site providers (Twitter, LinkedIn, Instagram)** depend on third-party APIs (FxTwitter, oEmbed). If the upstream API disappears, nab falls back to scraped HTML, but the structured providers are not technically local-only.

Everything else is local.

## Roadmap

### Phase 3 — cross-platform parity

- `sherpa-onnx` ASR backend (Parakeet TDT v3 ONNX, ONNX Runtime CPU/CUDA/CoreML execution providers)
- `whisper-rs` ASR backend (whisper-large-v3-turbo, universal fallback)
- `nab models fetch sherpa-onnx` and `nab models fetch whisper`
- Tesseract OCR engine for Linux and Windows
- Linux + Windows builds for nab and hebb

### Phase 4 — vision and VLM

- Optional `mistralrs` integration with Qwen3-VL (3B for laptops, 7B for workstations, 72B on NVIDIA DGX Spark)
- `nab analyze --vision` defaults to local VLM
- Frame-level vision descriptions inlined into transcripts
- Sovereign image-to-text on top of OCR

### Phase 5 — agentic memory

- hebb prospective memory triggers nab watches automatically
- nab analyze writes named-speaker transcripts to hebb without human prompting
- Cross-tool composition without LLM-in-the-loop coordination, where the data flow itself is the program

## Why this matters

Two reasons.

The first is practical. Local-first stacks are faster, cheaper, and more private than cloud stacks. They survive vendor changes. They survive billing changes. They survive jurisdictional changes. They survive being offline.

The second is structural. The current shape of AI is concentrating value in three or four hyperscalers. A sovereign stack shows that the same workloads — fetch, transcribe, remember, search, watch — can be assembled from open weights and open source on commodity hardware, without giving up quality. nab analyze hits 131× realtime on a laptop. hebb hits #1 on LoCoMo on the same laptop. The cloud is not necessary for these workloads. The dependency is a choice, not a requirement.

The MCP protocol is the lever. Once two components both speak it, they compose without bespoke integration. Once nab and hebb both speak it, you have a verb tool plus a memory layer plus a sovereign substrate. Add the inference primitives below and the language model on top, and you have an AI stack that owes nothing to anyone.

That is the bet of this project.
