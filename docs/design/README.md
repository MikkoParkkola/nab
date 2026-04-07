# nab design docs

This directory contains design proposals for nab features. Each document is a snapshot of the thinking behind a feature at the time it was written. Implementation may have diverged.

## Phase 1.5 — analyze v2 + URL watch + active reading

Three features that landed in 0.7.0 and unlock the multimodal local-first story.

| Document | Status | Summary |
|----------|--------|---------|
| [analyze-v2.md](analyze-v2.md) | shipped (0.7.0) | Multilingual ASR + speaker diarization + vision pipeline. FluidAudio on Apple Neural Engine, sherpa-onnx and whisper-rs in Phase 3. `AsrBackend` trait. New `analyze` MCP tool. |
| [url-watch-resources.md](url-watch-resources.md) | shipped (0.7.0) | URL monitoring exposed as MCP subscribable resources. RSS for the entire web. Conditional GETs, semantic diff, adaptive backoff. New `watch_create`/`watch_list`/`watch_remove` MCP tools. |
| [active-reading.md](active-reading.md) | shipped (0.7.0) | Active reading via MCP `sampling/createMessage`. nab calls back to the host LLM to identify references in transcripts and inlines them as footnotes. Novel for ASR pipelines. |

## Phase 2 — MCP spec closure

Bringing nab from ~80% to 100% MCP 2025-11-25 spec compliance.

| Document | Status | Summary |
|----------|--------|---------|
| [mcp-spec-closure.md](mcp-spec-closure.md) | shipped (0.7.0) | Streamable HTTP transport, structured logging, sampling helper, roots helper, argument completion. Last gaps closed. |

## Earlier design notes

The repo also carries older design notes that predate the design directory:

- [../ARCHITECTURE.md](../ARCHITECTURE.md) — current architecture reference
- [../design-mcp-innovations.md](../design-mcp-innovations.md) — earlier MCP feature ideas
- [../design-wasm-marketplace.md](../design-wasm-marketplace.md) — WASM provider marketplace (deferred from 0.7.0)
- [../browser-automation.md](../browser-automation.md) — SPA + browser automation pipeline
- [../ffmpeg_backend.md](../ffmpeg_backend.md) — ffmpeg integration notes

## How to read these

Each design doc opens with a status, date, phase, and goal block. Read these first — they tell you whether the design has shipped, been deferred, or been superseded. Then skim the architecture diagram, then the data model, then the cargo features section. Implementation details below that are useful when you actually need to touch the code.

## How to add a new design doc

Use the existing docs as templates. The convention:

1. Filename in kebab-case: `feature-name.md`
2. Front-matter block with `Status`, `Date`, `Phase`, `Novelty` (optional)
3. The unlock or thesis in one paragraph
4. User-facing behavior with concrete CLI examples
5. Architecture diagram (ASCII art is fine)
6. Data model in Rust types
7. Cargo features
8. MCP exposure
9. Test plan
10. Open questions
11. References

After landing the feature, add an entry to this index with `shipped` status.
