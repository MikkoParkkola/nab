# nab analyze v2 — multilingual ASR + diarization + vision

**Status**: design
**Date**: 2026-04-07
**Replaces**: `nab/src/analyze/transcribe.rs::ParakeetTranscriber` (dead — references `parakeet.cpp` binary that doesn't exist)

## Goals

1. **Multilingual** — first-class support for en, fi, sv, ru, de, fr, es, zh, ja
2. **SOTA quality** — match the best published WER/DER per language
3. **Fastest possible on Apple Silicon** — Neural Engine, not GPU/CPU
4. **Cross-platform** — also runs on Linux/x86 (CPU + CUDA), Windows
5. **Zero Python in the hot path** — Python only allowed when wrapping a compiled binary
6. **Word-level timestamps + confidence** for every segment
7. **Speaker diarization** with reasonable DER
8. **Vision/multimodal** for "describe what's happening in this video"
9. **MCP-exposed** — gateway picks up `analyze` automatically once registered

## Architecture

```
                   ┌─────────────────────┐
                   │   nab analyze       │  CLI + MCP tool
                   │   (Rust)            │
                   └──────────┬──────────┘
                              │
            ┌─────────────────┼─────────────────┐
            ▼                 ▼                 ▼
    ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
    │ FrameExtract │  │ AsrBackend   │  │ VisionBackend│
    │ (ffmpeg)     │  │ (trait)      │  │ (trait)      │
    └──────────────┘  └──────┬───────┘  └──────┬───────┘
                             │                 │
              ┌──────────────┼──────────┐      │
              ▼              ▼          ▼      ▼
       ┌───────────┐ ┌────────────┐ ┌──────┐ ┌─────────┐
       │FluidAudio │ │SherpaOnnx  │ │Whispe│ │ClaudeAPI│
       │(macOS ARM)│ │(any plat)  │ │rRs   │ │+ MLX-VLM│
       │           │ │            │ │(any) │ │(local)  │
       │ Parakeet  │ │ Parakeet   │ │      │ │         │
       │ TDT v3    │ │ ONNX       │ │ 99   │ │         │
       │ +VBx diar │ │ +pyannote  │ │ langs│ │         │
       │           │ │  ONNX diar │ │      │ │         │
       │ +Qwen3 zh/│ │            │ │      │ │         │
       │  ja/ko/vi │ │            │ │      │ │         │
       └─────┬─────┘ └─────┬──────┘ └──┬───┘ └────┬────┘
             │             │           │          │
             ▼             ▼           ▼          ▼
        Apple ANE     ONNX Runtime  GGML       HTTP
        (in-proc)    (CPU/CUDA/CoreML) (GPU)    API
```

## Backend selection

| Platform | Primary | Diarization | Vision (default) | Vision (opt-in) |
|---|---|---|---|---|
| macOS arm64 | `fluidaudio-rs` (Parakeet TDT v3) | FluidAudio offline VBx (community-1) | Claude API | MLX-VLM (Qwen2.5-VL-3B) |
| macOS arm64 + zh/ja/ko/vi | FluidAudio Qwen3-ASR (CoreML, beta) | FluidAudio VBx | Claude API | MLX-VLM |
| Linux x86_64 | `sherpa-onnx` (Parakeet TDT v3 ONNX) | sherpa-onnx pyannote-seg-3.0 + 3D-Speaker | Claude API | vLLM Qwen2.5-VL-7B (HTTP) |
| Linux x86_64 + CUDA | sherpa-onnx CUDA EP | sherpa-onnx CUDA EP | Claude API | vLLM Qwen2.5-VL-7B local |
| DGX Spark (remote) | SSH → sherpa-onnx CUDA OR pyannote community-1 | same | Claude API | vLLM 72B |
| Universal fallback | `whisper-rs` (whisper-large-v3-turbo) | none | Claude API | none |

**Routing logic** (compile-time + runtime):
1. `cfg(all(target_os = "macos", target_arch = "aarch64"))` → try FluidAudio first
2. else → try sherpa-onnx
3. on either backend failure or language not in Parakeet 25-langs and not Chinese/Japanese → fall back to whisper-rs
4. user override via `nab analyze --backend {fluidaudio,sherpa,whisper,vllm,spark}`

## Data model (Rust)

Refactor `nab/src/analyze/transcribe.rs` to:

```rust
/// A transcribed word with timing and confidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordTiming {
    pub word: String,
    pub start: f64,        // seconds from clip start
    pub end: f64,
    pub confidence: f32,   // [0.0, 1.0]
}

/// One transcribed segment (sentence or chunk), optionally with words.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,    // BCP-47, e.g., "fi", "en-US"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,     // populated after diarization
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<WordTiming>>,
}

/// A speaker turn (output of diarizer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub speaker: String,    // e.g., "SPEAKER_00"
    pub start: f64,
    pub end: f64,
}

/// Full transcription result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub segments: Vec<TranscriptSegment>,
    pub language: String,                   // dominant language
    pub duration_seconds: f64,
    pub model: String,                      // e.g., "parakeet-tdt-0.6b-v3"
    pub backend: String,                    // e.g., "fluidaudio"
    pub rtfx: f64,                          // realtime factor (audio_secs / wall_secs)
    pub processing_time_seconds: f64,
}

/// The single backend trait.
#[async_trait::async_trait]
pub trait AsrBackend: Send + Sync {
    /// Backend identifier ("fluidaudio", "sherpa-onnx", "whisper-rs", "vllm").
    fn name(&self) -> &str;

    /// Languages this backend supports (BCP-47 codes), or "*" for any.
    fn supported_languages(&self) -> &[&str];

    /// Transcribe an audio file.
    async fn transcribe(
        &self,
        audio_path: &Path,
        opts: TranscribeOptions,
    ) -> Result<TranscriptionResult>;
}

#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    pub language: Option<String>,    // BCP-47 hint, None = auto-detect
    pub word_timestamps: bool,
    pub max_duration_seconds: Option<u32>,
}
```

The diarizer stays a separate trait:

```rust
#[async_trait::async_trait]
pub trait DiarizerBackend: Send + Sync {
    fn name(&self) -> &str;
    async fn diarize(&self, audio_path: &Path) -> Result<Vec<SpeakerSegment>>;
}
```

## Cargo features

```toml
[features]
default = ["analyze-fluidaudio", "analyze-whisper"]
analyze = []                                # base trait + extract + fusion
analyze-fluidaudio = ["analyze", "fluidaudio-rs"]    # macOS arm64 only
analyze-sherpa = ["analyze", "sherpa-onnx"]          # cross-platform ONNX
analyze-whisper = ["analyze", "whisper-rs"]          # universal fallback
analyze-vllm = ["analyze"]                            # HTTP only, no extra dep
```

`fluidaudio-rs` is gated behind `cfg(all(target_os = "macos", target_arch = "aarch64"))` in the dependency declaration so Linux builds skip it cleanly.

## MCP exposure

New tool `nab/src/bin/mcp_server/tools/analyze.rs`:

```rust
#[mcp_tool(
    name = "analyze",
    description = "Transcribe and analyze audio/video. Multilingual SOTA ASR with \
                   word-level timestamps, optional speaker diarization, optional \
                   visual frame analysis. Local and on-device by default.",
)]
pub struct AnalyzeTool {
    /// Audio or video file path or URL
    pub input: String,
    /// Language hint (BCP-47, e.g., "fi", "en-US"). None = auto-detect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Backend override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Enable speaker diarization
    #[serde(default)]
    pub diarize: bool,
    /// Enable visual frame analysis (video only)
    #[serde(default)]
    pub vision: bool,
    /// Audio-only mode (skip vision even for videos)
    #[serde(default)]
    pub audio_only: bool,
}
```

Annotations: `read_only_hint=true`, `destructive_hint=false`, `idempotent_hint=true`, `open_world_hint=true`.

Output schema: `TranscriptionResult` JSON with optional `speakers[]` and `frames[]`.

Task-augmented execution: **YES** — long videos require it (see fetch_batch precedent at `main.rs:749-807`). Set `tool.execution = Some(ToolExecution { task_support: Some(ToolExecutionTaskSupport::Required) })` for `analyze`.

Progress notifications: emit `notifications/progress` from inside the `tokio::spawn` block at `main.rs:788`, with `progress` 0..100 covering audio extract → transcribe → diarize → vision → fuse.

## Model storage

Single canonical location:
```
~/.cache/nab/models/
├── fluidaudio/                      # symlinked to ~/Library/Application Support/FluidAudio/Models/
│   ├── parakeet-tdt-0.6b-v3/
│   └── speaker-diarization/
├── sherpa-onnx/
│   ├── sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/
│   └── pyannote-segmentation-3.0/
├── whisper-cpp/
│   └── ggml-large-v3-turbo-q5_0.bin
└── vllm/                            # vLLM downloads to its own HF cache, symlinked
```

`nab models` subcommand:
```
nab models list                      # show installed models per backend
nab models fetch fluidaudio          # download FluidAudio Parakeet + Sortformer
nab models fetch sherpa-onnx         # download Parakeet ONNX + pyannote ONNX
nab models fetch whisper-large-v3-turbo
nab models fetch all                 # fetch everything for current platform
nab models verify                    # checksum + load test
```

## FluidAudio install

FluidAudio is an external Swift project. nab depends on `fluidaudio-rs` (Rust crate) which embeds the Swift bridge as a build-time C library. cargo build handles it.

For the standalone CLI (used as fallback when in-process FFI fails):

```
nab models fetch fluidaudiocli
# downloads from https://github.com/FluidInference/FluidAudio at the pinned version,
# runs `swift build -c release`,
# installs to ~/.local/share/nab/bin/fluidaudiocli
```

Pinned version stored in `~/.cache/nab/models/fluidaudio/VERSION`. Updates are explicit (`nab models update fluidaudio`).

## Migration plan

1. **Phase 1 — preserve existing behavior** (this PR)
   - Add `AsrBackend` trait
   - Implement `FluidAudioBackend` (in-process via `fluidaudio-rs`)
   - Implement `SherpaOnnxBackend` stub (returns "not yet implemented" — fills in next PR)
   - Wire up `nab analyze --backend fluidaudio` to use the new path
   - Keep `ParakeetTranscriber` and `VllmTranscriber` for one release with `#[deprecated]`
   - Add `nab models fetch fluidaudio` subcommand
   - Add `nab/src/bin/mcp_server/tools/analyze.rs` with task-augmented execution
   - Update `main.rs` to register `AnalyzeTool` in the `tool_box!` macro

2. **Phase 2 — diarization + multilingual** (next PR)
   - Wire up FluidAudio diarizer via `fluidaudio-rs::FluidAudio::diarize_file`
   - Add fusion logic to map speakers to transcript segments
   - Add Qwen3-ASR opt-in for zh/ja/ko/vi

3. **Phase 3 — Linux/cross-platform** (later)
   - Implement `SherpaOnnxBackend` properly using `sherpa-onnx` crate
   - Implement `WhisperRsBackend` using `whisper-rs` crate
   - CI build matrix for {macOS arm64, macOS x86, Linux x86, Linux arm}
   - Bundle prebuilt `whisper-large-v3-turbo-q5_0.bin` download URL

4. **Phase 4 — vision + fusion** (later)
   - Replace existing `vision.rs` Claude API path with multimodal upgrades
   - Add MLX-VLM subprocess backend for fully-local mode
   - Add frame sampling improvements (scene detect + uniform 1fps + 64-frame cap)

5. **Phase 5 — drop dead code**
   - Remove `ParakeetTranscriber` and `VllmTranscriber` (replaced by trait impls)
   - Remove `nab/src/analyze/transcribe.rs::PARAKEET_BINARY_SEARCH_PATHS` constants
   - Update README

## What we're NOT doing

- **No Python in the hot path.** pyannote 3.1 / community-1 is the SOTA but lives in PyTorch. We use FluidAudio's CoreML port (community-1 model, ~13.89% DER on VoxConverse, 122× RTFx) on macOS, and sherpa-onnx pyannote-seg-3.0 on Linux. If neither suffices we offer remote SSH offload to DGX Spark — but the local hot path stays Python-free.
- **No bundled models in the binary.** Models are 100s of MB and version separately from nab.
- **No vendored FluidAudio source.** It's an external project; we depend on `fluidaudio-rs` from crates.io.
- **No `parakeet.cpp` binary path.** It doesn't exist as a real project. Delete the dead code.
- **No backwards-compat for the old `Transcriber::new` signature.** Rev the public API. nab is pre-1.0.

## References

- FluidAudio: https://github.com/FluidInference/FluidAudio (v0.13.6, 2026-04-04, MIT)
- fluidaudio-rs: https://crates.io/crates/fluidaudio-rs (0.1.0, MIT, macOS ARM only)
- sherpa-onnx: https://crates.io/crates/sherpa-onnx (1.12.35, official Rust API)
- whisper-rs: https://crates.io/crates/whisper-rs (0.16.0)
- Parakeet TDT v3 CoreML: https://huggingface.co/FluidInference/parakeet-tdt-0.6b-v3-coreml
- MCP 2025-11-25 spec: https://modelcontextprotocol.io/specification/2025-11-25/
- Live test (this session): 30s Finnish audio → 0.21s wall, 143× realtime, 94% confidence, perfect ä/ö
