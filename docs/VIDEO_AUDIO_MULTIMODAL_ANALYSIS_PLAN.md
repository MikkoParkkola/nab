# Video + Audio Multimodal Behavioral Analysis

## Technical Architecture Plan

**Date**: 2025-01-25
**Scope**: Synchronized video/audio analysis with speaker diarization, action recognition, and behavioral analysis

---

## 1. Executive Summary

Build a cost-efficient multimodal analysis pipeline combining:
- **Audio**: Whisper transcription + pyannote diarization (local)
- **Video**: Intelligent keyframe extraction + VLM analysis (hybrid local/API)
- **Behavioral**: Emotion/microexpression via DeepFace + Hume.ai

**Estimated Costs**:
- Local-only: ~$0/hour (GPU compute only)
- Hybrid (Claude Vision for key moments): ~$5-15/hour of video
- Full API: ~$50-100/hour of video

---

## 2. Existing Infrastructure

### Available Now
| Component | Status | Location |
|-----------|--------|----------|
| mlx_whisper | Installed | `~/.claude/.venv` |
| transformers (LLaVA, Video-LLaVA, Qwen2-VL) | Installed | `~/.claude/.venv` |
| ffmpeg 8.0.1 | Installed | `/opt/homebrew/bin` |
| DGX Spark (GB10 Blackwell) | Available | `ssh spark` |
| librosa | Installed | Audio processing |

### To Install
| Component | Purpose | Install Command |
|-----------|---------|----------------|
| pyannote.audio | Speaker diarization | `pip install pyannote.audio` |
| scenedetect | Keyframe extraction | `pip install scenedetect[opencv]` |
| deepface | Emotion detection | `pip install deepface` |
| mediapipe | Face mesh/tracking | `pip install mediapipe` |

---

## 3. Pipeline Architecture

```
                    ┌─────────────────────────────────────────────────────┐
                    │                   INPUT VIDEO                        │
                    └─────────────────────┬───────────────────────────────┘
                                          │
                    ┌─────────────────────┴───────────────────────────────┐
                    │                                                      │
          ┌─────────▼─────────┐                              ┌─────────────▼──────────┐
          │   AUDIO TRACK     │                              │    VIDEO TRACK         │
          └─────────┬─────────┘                              └─────────────┬──────────┘
                    │                                                      │
          ┌─────────▼─────────┐                              ┌─────────────▼──────────┐
          │ 1. Transcription  │                              │ 1. Scene Detection     │
          │    (Whisper)      │                              │    (PySceneDetect)     │
          └─────────┬─────────┘                              └─────────────┬──────────┘
                    │                                                      │
          ┌─────────▼─────────┐                              ┌─────────────▼──────────┐
          │ 2. Diarization    │                              │ 2. Keyframe Extract    │
          │    (pyannote)     │                              │    (ffmpeg + adaptive) │
          └─────────┬─────────┘                              └─────────────┬──────────┘
                    │                                                      │
          ┌─────────▼─────────┐                              ┌─────────────▼──────────┐
          │ 3. Word-Level     │                              │ 3. Face Detection      │
          │    Timestamps     │                              │    (DeepFace/MediaPipe)│
          └─────────┬─────────┘                              └─────────────┬──────────┘
                    │                                                      │
                    │                                        ┌─────────────▼──────────┐
                    │                                        │ 4. Emotion Analysis    │
                    │                                        │    (DeepFace/Hume)     │
                    │                                        └─────────────┬──────────┘
                    │                                                      │
                    │                                        ┌─────────────▼──────────┐
                    │                                        │ 5. Action Recognition  │
                    │                                        │    (TimeSformer/VLM)   │
                    │                                        └─────────────┬──────────┘
                    │                                                      │
                    └────────────────────┬─────────────────────────────────┘
                                         │
                              ┌──────────▼───────────┐
                              │   MULTIMODAL FUSION  │
                              │   (Timestamp-based)  │
                              └──────────┬───────────┘
                                         │
                              ┌──────────▼───────────┐
                              │   VLM Analysis       │
                              │   (Claude/Qwen2-VL)  │
                              └──────────┬───────────┘
                                         │
                              ┌──────────▼───────────┐
                              │   BEHAVIORAL OUTPUT  │
                              │   (JSON Timeline)    │
                              └──────────────────────┘
```

---

## 4. Component Details

### 4.1 Audio Processing

#### Transcription (mlx_whisper - LOCAL)
```python
import mlx_whisper

result = mlx_whisper.transcribe(
    "audio.wav",
    path_or_hf_repo="mlx-community/whisper-large-v3-turbo",
    word_timestamps=True,  # CRITICAL: Enables alignment
    language="en"
)
# Output: segments with word-level timestamps
```

**Performance**: 206x RTF on Apple Silicon (already validated in your setup)

#### Speaker Diarization (pyannote - LOCAL)
```python
from pyannote.audio import Pipeline
import torch

pipeline = Pipeline.from_pretrained(
    "pyannote/speaker-diarization-3.1",
    use_auth_token="HF_TOKEN"
)
pipeline.to(torch.device("cuda"))  # Or mps for Mac

diarization = pipeline("audio.wav")

# Output: speaker segments with timestamps
for turn, _, speaker in diarization.itertracks(yield_label=True):
    print(f"[{turn.start:.1f}s -> {turn.end:.1f}s] {speaker}")
```

**Accuracy**: 7.8-21.7% DER depending on dataset (SOTA)
**License**: MIT (free)
**GPU Memory**: ~2-4GB

#### Alignment Strategy
```python
def align_transcript_to_speakers(whisper_result, diarization):
    """
    Merge word timestamps with speaker labels.
    """
    aligned = []
    for segment in whisper_result['segments']:
        for word in segment.get('words', []):
            start, end = word['start'], word['end']
            # Find speaker at word midpoint
            mid = (start + end) / 2
            speaker = get_speaker_at_time(diarization, mid)
            aligned.append({
                'word': word['word'],
                'start': start,
                'end': end,
                'speaker': speaker
            })
    return aligned
```

### 4.2 Video Processing

#### Scene Detection (PySceneDetect - LOCAL)
```python
from scenedetect import detect, ContentDetector, AdaptiveDetector

# Content-aware detection (best for cuts)
scenes = detect("video.mp4", ContentDetector(threshold=27.0))

# Adaptive detection (better for camera movement)
scenes = detect("video.mp4", AdaptiveDetector())

# Output: List of (start_time, end_time) tuples
```

#### Intelligent Keyframe Extraction
```python
def extract_keyframes(video_path: str, strategy: str = "adaptive") -> list:
    """
    Extract frames intelligently based on:
    1. Scene changes (mandatory)
    2. Face changes (when faces detected)
    3. Periodic sampling (fallback)
    """
    keyframes = []

    # 1. Scene boundaries (always extract)
    scenes = detect(video_path, ContentDetector())
    for scene_start, scene_end in scenes:
        keyframes.append(extract_frame(video_path, scene_start))

    # 2. Face-based sampling within scenes
    # Sample more frequently when faces are present
    for scene_start, scene_end in scenes:
        if has_faces(scene_start):
            # 1 fps when faces present
            keyframes.extend(sample_fps(scene_start, scene_end, fps=1))
        else:
            # 0.2 fps for non-face scenes
            keyframes.extend(sample_fps(scene_start, scene_end, fps=0.2))

    return deduplicate_by_timestamp(keyframes)
```

**Cost Optimization**:
| Strategy | Frames/min | Tokens/min (Claude) | Cost/hour |
|----------|------------|---------------------|-----------|
| Every frame (30fps) | 1,800 | 2,880,000 | $8.64 |
| 1 fps | 60 | 96,000 | $0.29 |
| Scene-based (~0.2fps) | 12 | 19,200 | $0.06 |
| **Adaptive (faces)** | **20-40** | **32-64K** | **$0.10-0.19** |

#### ffmpeg Commands
```bash
# Extract audio
ffmpeg -i video.mp4 -vn -acodec pcm_s16le -ar 16000 -ac 1 audio.wav

# Extract frame at timestamp
ffmpeg -ss 00:01:23.456 -i video.mp4 -frames:v 1 -q:v 2 frame.jpg

# Extract keyframes only (I-frames)
ffmpeg -i video.mp4 -vf "select='eq(pict_type,I)'" -vsync vfr keyframe_%04d.jpg

# Extract at 1 fps
ffmpeg -i video.mp4 -vf fps=1 frame_%04d.jpg
```

### 4.3 Emotion/Behavioral Analysis

#### DeepFace (LOCAL - Free)
```python
from deepface import DeepFace

# Analyze single frame
result = DeepFace.analyze(
    img_path="frame.jpg",
    actions=['emotion', 'age', 'gender'],
    detector_backend='retinaface'  # Best accuracy
)

# Output:
# {
#   'emotion': {'angry': 0.1, 'happy': 0.8, 'sad': 0.05, ...},
#   'dominant_emotion': 'happy',
#   'age': 32,
#   'gender': {'Man': 0.95, 'Woman': 0.05}
# }
```

**Emotions Detected**: angry, disgust, fear, happy, sad, surprise, neutral
**Accuracy**: Competitive with SOTA, varies by detector backend
**Speed**: ~50-200ms/frame depending on backend

#### MediaPipe Face Mesh (LOCAL - Free)
```python
import mediapipe as mp

mp_face_mesh = mp.solutions.face_mesh
face_mesh = mp_face_mesh.FaceMesh(
    max_num_faces=4,
    refine_landmarks=True,  # 478 landmarks including iris
    min_detection_confidence=0.5
)

# Process frame
results = face_mesh.process(cv2.cvtColor(frame, cv2.COLOR_BGR2RGB))

# 468 landmark points for:
# - Eye tracking (gaze direction)
# - Lip movement (speech detection)
# - Eyebrow position (surprise/concern)
# - Jaw position (tension indicators)
```

#### Hume.ai (API - Paid)
```python
import hume

# Expression Measurement API
async with hume.HumeStreamClient(api_key="KEY") as client:
    config = hume.FaceConfig()
    async with client.connect([config]) as socket:
        result = await socket.send_file("frame.jpg")

# Output: 48 emotion categories with confidence scores
# Including: Admiration, Adoration, Aesthetic Appreciation,
# Amusement, Anxiety, Awe, Awkwardness, Boredom, Calmness...
```

**Features**:
- 48 emotion categories (vs DeepFace's 7)
- Vocal prosody analysis
- Language sentiment
- 600+ behavioral tags

**Pricing**: Contact for enterprise (typically $0.001-0.01 per analysis)

### 4.4 Action Recognition

#### TimeSformer (LOCAL - DGX Spark)
```python
from transformers import AutoImageProcessor, TimesformerForVideoClassification
import torch

processor = AutoImageProcessor.from_pretrained(
    "facebook/timesformer-base-finetuned-k400"
)
model = TimesformerForVideoClassification.from_pretrained(
    "facebook/timesformer-base-finetuned-k400"
).to("cuda")

# Process 8-frame clip
frames = [load_frame(f) for f in frame_paths[:8]]
inputs = processor(frames, return_tensors="pt").to("cuda")

with torch.no_grad():
    outputs = model(**inputs)
    action = model.config.id2label[outputs.logits.argmax(-1).item()]
```

**Actions**: 400 Kinetics-400 categories
**Input**: 8 frames @ 224x224
**Speed**: ~50ms/clip on GPU

### 4.5 Vision-Language Models (VLM)

#### Local: Qwen2-VL-7B (DGX Spark)
```python
from transformers import Qwen2VLForConditionalGeneration, AutoProcessor
import torch

model = Qwen2VLForConditionalGeneration.from_pretrained(
    "Qwen/Qwen2-VL-7B-Instruct",
    torch_dtype=torch.bfloat16,
    attn_implementation="flash_attention_2",
    device_map="auto"
)

# Video analysis (up to 20+ minutes)
messages = [{
    "role": "user",
    "content": [
        {"type": "video", "video": "file:///path/to/video.mp4", "fps": 1.0},
        {"type": "text", "text": "Analyze the behavioral cues in this video..."}
    ]
}]
```

**Pros**: Free, 20+ min video support, Apache 2.0 license
**Cons**: Requires ~16GB VRAM, no audio understanding

#### Local: LLaVA-NeXT-Video-32B (DGX Spark)
```python
# Via SGLang for optimal inference
# Best open-source video understanding
```

**Pros**: SOTA open-source video benchmarks
**Cons**: 32B requires significant VRAM (~40GB)

#### API: Claude Vision
```python
import anthropic

client = anthropic.Anthropic()

# Send keyframe with context
response = client.messages.create(
    model="claude-sonnet-4-5",
    max_tokens=1024,
    messages=[{
        "role": "user",
        "content": [
            {"type": "image", "source": {"type": "base64", "data": frame_b64, "media_type": "image/jpeg"}},
            {"type": "text", "text": f"""
            Analyze this frame from a behavioral interview.

            Context:
            - Timestamp: {timestamp}s
            - Current speaker: {speaker}
            - Recent transcript: "{transcript_context}"
            - Detected emotion (DeepFace): {emotion}

            Analyze:
            1. Body language and posture
            2. Facial expressions
            3. Congruence with speech
            4. Behavioral indicators (confidence, stress, deception cues)
            """}
        ]
    }]
)
```

**Pricing** (Claude Sonnet 4.5):
| Image Size | Tokens | Cost/Image | Cost/1K Images |
|------------|--------|------------|----------------|
| 200x200 | ~54 | $0.00016 | $0.16 |
| 1000x1000 | ~1,334 | $0.004 | $4.00 |
| 1092x1092 | ~1,590 | $0.0048 | $4.80 |

**Token Formula**: `tokens = (width * height) / 750`

---

## 5. Sampling Strategies

### 5.1 Adaptive Sampling Algorithm
```python
class AdaptiveSampler:
    """
    Cost-efficient frame sampling based on content analysis.
    """

    def __init__(self):
        self.base_fps = 0.2  # 1 frame per 5 seconds minimum
        self.face_fps = 1.0   # 1 fps when faces present
        self.speech_fps = 2.0 # 2 fps during active speech
        self.action_fps = 4.0 # 4 fps during detected action

    def get_sample_rate(self, timestamp: float, context: dict) -> float:
        """
        Determine sampling rate based on content.
        """
        fps = self.base_fps

        # Increase for faces
        if context.get('has_faces'):
            fps = max(fps, self.face_fps)

        # Increase during speech
        if context.get('is_speaking'):
            fps = max(fps, self.speech_fps)

        # Increase for significant actions
        if context.get('action_confidence', 0) > 0.7:
            fps = max(fps, self.action_fps)

        # Always sample scene boundaries
        if context.get('is_scene_boundary'):
            return float('inf')  # Force sample

        return fps
```

### 5.2 Cost Comparison (1-hour video)

| Strategy | Frames | Claude Cost | Local Cost | Total |
|----------|--------|-------------|------------|-------|
| Full (30fps) | 108,000 | $518.40 | $0 | $518.40 |
| Fixed 1fps | 3,600 | $17.28 | $0 | $17.28 |
| Scene-only | ~200 | $0.96 | $0 | $0.96 |
| **Adaptive** | ~500-1000 | $2.40-4.80 | $0 | $2.40-4.80 |
| **Local-only** | N/A | $0 | GPU time | ~$0.10 |

---

## 6. Behavioral Analysis Features

### 6.1 Microexpression Detection
```python
def detect_microexpressions(frames: list, fps: float = 30) -> list:
    """
    Microexpressions last 1/25 to 1/5 second.
    Requires high-fps capture to detect.
    """
    microexpressions = []
    prev_emotions = None

    for i, frame in enumerate(frames):
        emotions = DeepFace.analyze(frame, actions=['emotion'])

        if prev_emotions:
            # Check for rapid emotion change
            for emotion, score in emotions['emotion'].items():
                prev_score = prev_emotions['emotion'].get(emotion, 0)
                delta = abs(score - prev_score)

                # Significant change in < 0.2 seconds
                if delta > 0.3 and (i / fps) < 0.2:
                    microexpressions.append({
                        'timestamp': i / fps,
                        'emotion': emotion,
                        'intensity_change': delta,
                        'type': 'microexpression'
                    })

        prev_emotions = emotions

    return microexpressions
```

**Note**: True microexpression detection requires 60+ fps video. Most analysis
will focus on macro expressions and behavioral patterns.

### 6.2 Deception Indicators (Research-Based)
```python
DECEPTION_INDICATORS = {
    'verbal': {
        'increased_pauses': 'Audio analysis - longer gaps between words',
        'pitch_changes': 'Vocal prosody - higher pitch under stress',
        'speech_rate': 'Speaking faster or slower than baseline',
        'detail_reduction': 'Less specific details in responses',
    },
    'nonverbal': {
        'gaze_aversion': 'MediaPipe - reduced eye contact',
        'self_touching': 'Action recognition - face/neck touching',
        'micro_shrug': 'Shoulder movement detection',
        'asymmetric_expressions': 'Face mesh landmark analysis',
        'blink_rate': 'MediaPipe - increased blinking',
    },
    'incongruence': {
        'emotion_speech_mismatch': 'Happy words with sad expression',
        'timing_delays': 'Emotional reaction delayed from stimulus',
    }
}
```

**Important Caveat**: These are research indicators, not definitive lie detection.
Accuracy of deception detection is controversial (chance to ~70% in research).

### 6.3 Behavioral Timeline Output
```json
{
  "video_id": "interview_001",
  "duration": 3600.0,
  "segments": [
    {
      "start": 0.0,
      "end": 15.5,
      "speaker": "SPEAKER_00",
      "transcript": "Thank you for having me today...",
      "emotion": {
        "dominant": "neutral",
        "scores": {"neutral": 0.65, "happy": 0.25, "nervous": 0.10}
      },
      "behavioral_notes": [
        {"type": "gaze", "direction": "interviewer", "confidence": 0.85},
        {"type": "posture", "state": "open", "confidence": 0.72}
      ],
      "flags": []
    },
    {
      "start": 45.2,
      "end": 52.8,
      "speaker": "SPEAKER_00",
      "transcript": "I never had any issues with the previous team...",
      "emotion": {
        "dominant": "nervous",
        "scores": {"nervous": 0.55, "neutral": 0.30, "defensive": 0.15}
      },
      "behavioral_notes": [
        {"type": "gaze_aversion", "timestamp": 46.1, "duration": 1.2},
        {"type": "self_touch", "area": "neck", "timestamp": 47.3},
        {"type": "speech_rate", "change": "-15%", "baseline_ref": "0:00-0:30"}
      ],
      "flags": ["potential_incongruence", "stress_indicators"]
    }
  ]
}
```

---

## 7. DGX Spark Deployment

### 7.1 Offload Strategy
```
Local (Mac):                    DGX Spark:
├── ffmpeg extraction           ├── Whisper large-v3-turbo (if needed)
├── PySceneDetect              ├── pyannote diarization
├── Basic face detection       ├── Qwen2-VL-7B inference
└── Coordination               ├── TimeSformer action recognition
                               └── Batch VLM analysis
```

### 7.2 GPU Memory Allocation
| Model | VRAM Required | nvfp4 Quantized |
|-------|---------------|-----------------|
| Whisper large-v3-turbo | ~3GB | ~1.5GB |
| pyannote speaker-diarization | ~2GB | N/A |
| Qwen2-VL-7B | ~16GB | ~8GB |
| TimeSformer-base | ~4GB | ~2GB |
| **Total Concurrent** | **~25GB** | **~12GB** |

GB10 has 128GB unified memory - headroom for batch processing.

### 7.3 Remote Inference Script
```bash
#!/bin/bash
# run_analysis.sh - Execute on DGX Spark

# Activate environment
source /opt/conda/etc/profile.d/conda.sh
conda activate multimodal

# Process video
python analyze_video.py \
    --video /data/input/interview.mp4 \
    --output /data/output/analysis.json \
    --use-nvfp4 \
    --batch-size 16
```

---

## 8. Implementation Phases

### Phase 1: Core Pipeline (Week 1)
- [ ] Audio extraction + Whisper transcription
- [ ] pyannote diarization integration
- [ ] Word-speaker alignment
- [ ] Basic keyframe extraction

### Phase 2: Video Analysis (Week 2)
- [ ] Scene detection integration
- [ ] DeepFace emotion analysis
- [ ] MediaPipe face mesh tracking
- [ ] Adaptive sampling implementation

### Phase 3: VLM Integration (Week 3)
- [ ] Qwen2-VL local inference setup
- [ ] Claude Vision API integration
- [ ] Cost optimization (API fallback only)
- [ ] Behavioral prompt engineering

### Phase 4: Behavioral Analysis (Week 4)
- [ ] Incongruence detection
- [ ] Deception indicator framework
- [ ] Timeline generation
- [ ] Output format finalization

---

## 9. Cost Summary

### Per-Hour Video Analysis
| Tier | Components | Cost/Hour |
|------|------------|-----------|
| **Local-Only** | Whisper + pyannote + Qwen2-VL + DeepFace | ~$0.10 (GPU) |
| **Hybrid** | Local + Claude for flagged moments (~100 frames) | ~$0.50-2.00 |
| **Premium** | Local + Claude for all keyframes (~500 frames) | ~$2.50-5.00 |
| **Full API** | Claude for everything | ~$50-100+ |

### Recommended Configuration
**Hybrid Local-First** approach:
1. Process everything locally first
2. Flag suspicious/important moments
3. Send flagged frames to Claude for deep analysis
4. Use Hume.ai for prosody analysis on flagged segments

Expected cost: **$1-3 per hour of video** with quality comparable to full API.

---

## 10. Files to Create

```
~/.claude/
├── bin/
│   └── analyze-video           # CLI entry point
├── lib/
│   └── multimodal/
│       ├── __init__.py
│       ├── audio/
│       │   ├── transcribe.py   # Whisper wrapper
│       │   ├── diarize.py      # pyannote integration
│       │   └── align.py        # Word-speaker alignment
│       ├── video/
│       │   ├── extract.py      # Frame extraction
│       │   ├── scenes.py       # Scene detection
│       │   └── sample.py       # Adaptive sampling
│       ├── emotion/
│       │   ├── deepface.py     # Local emotion detection
│       │   ├── mediapipe.py    # Face mesh tracking
│       │   └── hume.py         # Hume.ai API client
│       ├── vlm/
│       │   ├── qwen.py         # Local VLM inference
│       │   ├── claude.py       # Claude Vision API
│       │   └── prompts.py      # Behavioral analysis prompts
│       ├── behavioral/
│       │   ├── analyze.py      # Main analysis orchestration
│       │   ├── indicators.py   # Deception/stress indicators
│       │   └── timeline.py     # Output generation
│       └── utils/
│           ├── ffmpeg.py       # ffmpeg wrappers
│           └── gpu.py          # DGX Spark offload
└── data/
    └── models/
        └── multimodal/         # Cached model weights
```

---

## 11. References

- [pyannote/speaker-diarization-3.1](https://huggingface.co/pyannote/speaker-diarization-3.1) - MIT, 7.8-21.7% DER
- [Qwen2-VL-7B](https://huggingface.co/Qwen/Qwen2-VL-7B-Instruct) - Apache 2.0, 20+ min video
- [DeepFace](https://github.com/serengil/deepface) - MIT, 7 emotions
- [TimeSformer](https://huggingface.co/facebook/timesformer-base-finetuned-k400) - CC-BY-NC, 400 actions
- [PySceneDetect](https://github.com/Breakthrough/PySceneDetect) - BSD-3, scene detection
- [Claude Vision](https://docs.anthropic.com/en/docs/build-with-claude/vision) - ~$0.004/1MP image
- [Hume.ai](https://hume.ai) - 48 emotions, prosody analysis

---

*Generated: 2025-01-25 | Estimated Implementation: 4 weeks | ROI: 50-100x vs manual analysis*
