//! Visual frame analysis
//!
//! Analyzes keyframes for emotions, actions, gaze direction, etc.
//! Supports local models (via Python) and Claude Vision API fallback.

use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;

use super::{AnalysisError, ExtractedFrame, Result};

/// Vision backend selection
#[derive(Debug, Clone, Default)]
pub enum VisionBackend {
    /// Local models (requires GPU for reasonable speed)
    #[default]
    Local,
    /// Claude Vision API
    ClaudeApi { api_key: String },
    /// Hybrid: local first, API fallback
    Hybrid { api_key: String },
}

/// Visual analysis result for a frame
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualAnalysis {
    pub timestamp: f64,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaze: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emotion: Option<EmotionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faces: Option<Vec<FaceAnalysis>>,
}

/// Emotion detection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmotionResult {
    pub primary: String,
    pub confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
}

/// Face-specific analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceAnalysis {
    pub bbox: [f32; 4], // x, y, width, height (normalized 0-1)
    pub emotion: EmotionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaze: Option<String>,
}

/// Vision analyzer
pub struct VisionAnalyzer {
    backend: VisionBackend,
    dgx_host: Option<String>,
}

impl VisionAnalyzer {
    pub fn new(backend: VisionBackend, dgx_host: Option<String>) -> Result<Self> {
        Ok(Self { backend, dgx_host })
    }

    /// Analyze multiple frames
    pub async fn analyze_frames(&self, frames: &[ExtractedFrame]) -> Result<Vec<VisualAnalysis>> {
        let mut results = Vec::with_capacity(frames.len());

        for frame in frames {
            let analysis = self.analyze_frame(frame).await?;
            results.push(analysis);
        }

        Ok(results)
    }

    /// Analyze a single frame
    pub async fn analyze_frame(&self, frame: &ExtractedFrame) -> Result<VisualAnalysis> {
        match &self.backend {
            VisionBackend::Local => self.analyze_local(frame).await,
            VisionBackend::ClaudeApi { api_key } => self.analyze_claude(frame, api_key).await,
            VisionBackend::Hybrid { api_key } => match self.analyze_local(frame).await {
                Ok(result) => Ok(result),
                Err(_) => self.analyze_claude(frame, api_key).await,
            },
        }
    }

    /// Local analysis using Python models
    async fn analyze_local(&self, frame: &ExtractedFrame) -> Result<VisualAnalysis> {
        // Use DGX if available
        if let Some(host) = &self.dgx_host {
            return self.analyze_remote(frame, host).await;
        }

        let script = format!(
            r#"
import json
import cv2
from deepface import DeepFace

image_path = "{image_path}"

# Analyze faces and emotions
try:
    faces = DeepFace.analyze(
        image_path,
        actions=['emotion'],
        enforce_detection=False,
        silent=True
    )

    if not isinstance(faces, list):
        faces = [faces]

    face_results = []
    primary_emotion = None
    primary_confidence = 0.0

    for face in faces:
        region = face.get("region", {{}})
        emotions = face.get("emotion", {{}})

        # Get top emotion
        top_emotion = max(emotions, key=emotions.get)
        confidence = emotions[top_emotion] / 100.0

        if confidence > primary_confidence:
            primary_emotion = top_emotion
            primary_confidence = confidence

        # Normalize bbox to 0-1
        img = cv2.imread(image_path)
        h, w = img.shape[:2]

        bbox = [
            region.get("x", 0) / w,
            region.get("y", 0) / h,
            region.get("w", 0) / w,
            region.get("h", 0) / h,
        ]

        face_results.append({{
            "bbox": bbox,
            "emotion": {{
                "primary": top_emotion,
                "confidence": confidence,
            }}
        }})

    result = {{
        "timestamp": {timestamp},
        "action": "present" if face_results else "none",
        "emotion": {{
            "primary": primary_emotion or "neutral",
            "confidence": primary_confidence,
        }} if primary_emotion else None,
        "faces": face_results if face_results else None,
    }}

except Exception as e:
    result = {{
        "timestamp": {timestamp},
        "action": "unknown",
        "emotion": None,
    }}

print(json.dumps(result))
"#,
            image_path = frame.path.display(),
            timestamp = frame.timestamp
        );

        let output = Command::new("python3")
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Vision(format!(
                "Local vision analysis failed: {stderr}"
            )));
        }

        let analysis: VisualAnalysis = serde_json::from_slice(&output.stdout)?;
        Ok(analysis)
    }

    /// Remote analysis on DGX Spark
    async fn analyze_remote(&self, frame: &ExtractedFrame, host: &str) -> Result<VisualAnalysis> {
        let remote_path = format!("/tmp/microfetch_frame_{}.jpg", std::process::id());

        // Copy frame to DGX
        let scp_status = Command::new("scp")
            .args([
                frame.path.to_str().unwrap(),
                &format!("{host}:{remote_path}"),
            ])
            .status()
            .await?;

        if !scp_status.success() {
            return Err(AnalysisError::Vision(
                "Failed to copy frame to DGX".to_string(),
            ));
        }

        let script = format!(
            r#"
import json
import cv2
from deepface import DeepFace

image_path = "{remote_path}"

try:
    faces = DeepFace.analyze(
        image_path,
        actions=['emotion'],
        enforce_detection=False,
        silent=True
    )

    if not isinstance(faces, list):
        faces = [faces]

    face_results = []
    primary_emotion = None
    primary_confidence = 0.0

    for face in faces:
        region = face.get("region", {{}})
        emotions = face.get("emotion", {{}})

        top_emotion = max(emotions, key=emotions.get)
        confidence = emotions[top_emotion] / 100.0

        if confidence > primary_confidence:
            primary_emotion = top_emotion
            primary_confidence = confidence

        img = cv2.imread(image_path)
        h, w = img.shape[:2]

        bbox = [
            region.get("x", 0) / w,
            region.get("y", 0) / h,
            region.get("w", 0) / w,
            region.get("h", 0) / h,
        ]

        face_results.append({{
            "bbox": bbox,
            "emotion": {{
                "primary": top_emotion,
                "confidence": confidence,
            }}
        }})

    result = {{
        "timestamp": {timestamp},
        "action": "present" if face_results else "none",
        "emotion": {{
            "primary": primary_emotion or "neutral",
            "confidence": primary_confidence,
        }} if primary_emotion else None,
        "faces": face_results if face_results else None,
    }}

except Exception as e:
    result = {{
        "timestamp": {timestamp},
        "action": "unknown",
    }}

print(json.dumps(result))
"#,
            remote_path = remote_path,
            timestamp = frame.timestamp
        );

        let output = Command::new("ssh")
            .args([host, "python3", "-c", &format!("'{script}'")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        // Clean up
        let _ = Command::new("ssh")
            .args([host, "rm", "-f", &remote_path])
            .status()
            .await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Vision(format!(
                "Remote vision analysis failed: {stderr}"
            )));
        }

        let analysis: VisualAnalysis = serde_json::from_slice(&output.stdout)?;
        Ok(analysis)
    }

    /// Analyze using Claude Vision API
    async fn analyze_claude(
        &self,
        frame: &ExtractedFrame,
        api_key: &str,
    ) -> Result<VisualAnalysis> {
        // Read image and base64 encode
        let image_data = tokio::fs::read(&frame.path).await?;
        let base64_image = base64_encode(&image_data);

        let prompt = r#"Analyze this video frame. Provide a JSON response with:
1. "action": what is the person/people doing (e.g., "talking", "waving", "reading", "walking")
2. "gaze": where are they looking (e.g., "camera", "left", "right", "down", "other person")
3. "emotion": {"primary": emotion name, "confidence": 0.0-1.0}
4. "scene": brief scene description
5. "objects": list of notable objects

Return ONLY valid JSON, no markdown."#;

        let request_body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 500,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/jpeg",
                            "data": base64_image
                        }
                    },
                    {
                        "type": "text",
                        "text": prompt
                    }
                ]
            }]
        });

        let client = reqwest::Client::new();
        let response = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| AnalysisError::Vision(e.to_string()))?;

        if !response.status().is_success() {
            let error = response.text().await.unwrap_or_default();
            return Err(AnalysisError::Vision(format!("Claude API error: {error}")));
        }

        let api_response: serde_json::Value = response
            .json()
            .await
            .map_err(|e| AnalysisError::Vision(e.to_string()))?;

        // Extract content from Claude response
        let content = api_response["content"][0]["text"]
            .as_str()
            .ok_or_else(|| AnalysisError::Vision("Invalid API response".to_string()))?;

        // Parse JSON from response
        let parsed: serde_json::Value = serde_json::from_str(content)
            .map_err(|e| AnalysisError::Vision(format!("Failed to parse Claude response: {e}")))?;

        let emotion = parsed.get("emotion").and_then(|e| {
            Some(EmotionResult {
                primary: e.get("primary")?.as_str()?.to_string(),
                confidence: e.get("confidence")?.as_f64()? as f32,
                secondary: None,
            })
        });

        Ok(VisualAnalysis {
            timestamp: frame.timestamp,
            action: parsed["action"].as_str().unwrap_or("unknown").to_string(),
            gaze: parsed["gaze"].as_str().map(String::from),
            emotion,
            objects: parsed["objects"].as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            }),
            scene: parsed["scene"].as_str().map(String::from),
            faces: None,
        })
    }
}

/// Base64 encode bytes
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result = Vec::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        let combined = (b0 << 16) | (b1 << 8) | b2;

        result.push(ALPHABET[(combined >> 18) & 0x3F]);
        result.push(ALPHABET[(combined >> 12) & 0x3F]);

        if chunk.len() > 1 {
            result.push(ALPHABET[(combined >> 6) & 0x3F]);
        } else {
            result.push(b'=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[combined & 0x3F]);
        } else {
            result.push(b'=');
        }
    }

    String::from_utf8(result).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visual_analysis_serialization() {
        let analysis = VisualAnalysis {
            timestamp: 1.5,
            action: "talking".to_string(),
            gaze: Some("camera".to_string()),
            emotion: Some(EmotionResult {
                primary: "happy".to_string(),
                confidence: 0.85,
                secondary: None,
            }),
            objects: Some(vec!["microphone".to_string(), "desk".to_string()]),
            scene: Some("interview setting".to_string()),
            faces: None,
        };

        let json = serde_json::to_string_pretty(&analysis).unwrap();
        assert!(json.contains("talking"));
        assert!(json.contains("happy"));
    }

    #[test]
    fn test_base64_encode() {
        let data = b"Hello, World!";
        let encoded = base64_encode(data);
        assert_eq!(encoded, "SGVsbG8sIFdvcmxkIQ==");
    }
}
