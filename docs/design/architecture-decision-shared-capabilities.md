# ADR: Shared capabilities across nab, axterminator, and hebb

**Status**: accepted
**Date**: 2026-04-08
**Decision**: Don't share Rust code across tools. Share binary artifacts + MCP protocol. Each tool independently wraps the same inference binaries.

## Context

This session built multimodal ASR capabilities (FluidAudio Parakeet TDT v3, sherpa-onnx, whisper-rs) inside nab. The question arose: should axterminator (which needs the same ASR for mic streaming and device-captured files) share this Rust code?

## Decision

**No shared Rust crates.** Share at the binary + protocol layer instead.

### What's shared (filesystem + protocol)

```
~/Library/Application Support/nab/bin/fluidaudiocli   ← both tools subprocess to this
~/Library/Application Support/nab/bin/whisper-cli      ← future, same pattern
~/Library/Application Support/nab/bin/mistralrs-server ← future VLM, same pattern

MCP JSON-RPC over stdio                                ← cross-tool composition protocol
hebb-mcp                                               ← the canonical memory endpoint
```

### What's NOT shared

Each tool owns its own thin Rust wrapper (~500 lines) around the shared binaries. The wrappers will diverge:
- nab's wrapper: batch mode, sentence segmentation, markdown output
- axterminator's wrapper: streaming mode, real-time events, speaker-change callbacks

### Why this is right

1. **Asymmetric growth**: axterminator will add 5+ ML backends (mic ASR, screen OCR, camera face detection, VLM, translation). nab adds 0-1. A shared library would be 80% axterminator code.

2. **Divergent abstractions**: nab wants "file in → transcript out". axterminator wants "stream in → events out". These are different enough that a shared trait would need to be retrofitted.

3. **500 lines of duplication is acceptable**: the wrapper code is thin, stable, and mechanical. Drift between copies is manageable. The cost of extraction (release coordination, versioning coupling, API surface negotiation) exceeds the cost of duplication.

4. **MCP is the composition layer**: both tools expose MCP servers. Both can call each other. The protocol IS the shared abstraction, at the process boundary where it belongs.

5. **Reversible**: if duplication grows painful (>5,000 lines), extract a shared crate then. Nothing prevents it.

## Architecture

```
nab: URL → content (fetch, watch, OCR, media-URL transcription)
  └ subprocesses to: fluidaudiocli, yt-dlp, ffmpeg

axterminator: device → events (mic, screen, camera, keyboard, ambient)
  └ subprocesses to: fluidaudiocli (same binary, own wrapper)

hebb: memory layer (text, voiceprints, KV, live queries)
  └ both tools write to hebb via MCP

Shared binary install: `nab models fetch fluidaudio` installs once,
both tools find it at the same filesystem path.
```

## Consequences

- nab keeps its analyze module as-is
- axterminator builds its own wrapper when it needs ASR (copy the pattern, not the code)
- `nab fetch <youtube-url>` routes through nab's own analyze internally
- `axterminator analyze <file>` routes through axterminator's own wrapper
- Both call hebb for memory (voiceprints, KV) via MCP
- Model binaries installed once, shared via filesystem convention

## References

- Session transcript 2026-04-07/08, architecture discussion
- Four-perspective analysis: CTO, staff engineer, Unix philosopher, moonshot architect
- All four perspectives converged on the same answer
