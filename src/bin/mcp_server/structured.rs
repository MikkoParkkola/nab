//! Structured content builders and server icon constants for `nab-mcp`.
//!
//! Provides zero-allocation helpers for building `structuredContent` JSON maps
//! and the embedded SVG icons used in the MCP `InitializeResult`.

// ─── Truncation helper ────────────────────────────────────────────────────────

/// Truncate markdown to `max_chars`, appending `\n\n... [truncated]` if needed.
pub(crate) fn truncate_markdown(text: &str, max_chars: usize) -> String {
    if text.len() > max_chars {
        format!("{}\n\n... [truncated]", &text[..max_chars])
    } else {
        text.to_string()
    }
}

// ─── structured_content helpers ───────────────────────────────────────────────

/// Build a `structuredContent` map from a fixed-size array of `(key, value)` pairs.
///
/// This is a zero-allocation helper for the common case of building a flat JSON
/// object with a known set of fields at compile time.
pub(crate) fn build_structured<const N: usize>(
    fields: [(&'static str, serde_json::Value); N],
) -> serde_json::Map<String, serde_json::Value> {
    fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// Build the `structuredContent` map for the `fetch` tool response.
pub(crate) fn build_fetch_structured(
    url: &str,
    status: u16,
    content_type: &str,
    markdown: &str,
    timing_ms: f64,
) -> serde_json::Map<String, serde_json::Value> {
    build_structured([
        ("url", serde_json::Value::String(url.to_string())),
        ("status", serde_json::Value::Number(status.into())),
        (
            "content_type",
            serde_json::Value::String(content_type.to_string()),
        ),
        (
            "content",
            serde_json::Value::String(truncate_markdown(markdown, 4000)),
        ),
        (
            "timing_ms",
            serde_json::Value::Number(
                serde_json::Number::from_f64(timing_ms).unwrap_or(serde_json::Number::from(0)),
            ),
        ),
    ])
}

// ─── Server icon ─────────────────────────────────────────────────────────────

/// Inline SVG globe icon for light backgrounds (~200 bytes).
///
/// Embedded as a `data:` URI — no external URL required.
/// The SVG renders a simple wireframe globe (circle + meridian ellipse + equator).
pub(crate) const GLOBE_SVG_LIGHT: &str = concat!(
    "data:image/svg+xml;base64,",
    "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAzMiAzMiI+",
    "PGNpcmNsZSBjeD0iMTYiIGN5PSIxNiIgcj0iMTQiIGZpbGw9Im5vbmUiIHN0cm9rZT0iIzMzMyIgc3",
    "Ryb2tlLXdpZHRoPSIxLjUiLz48ZWxsaXBzZSBjeD0iMTYiIGN5PSIxNiIgcng9IjYiIHJ5PSIxNCIg",
    "ZmlsbD0ibm9uZSIgc3Ryb2tlPSIjMzMzIiBzdHJva2Utd2lkdGg9IjEuNSIvPjxsaW5lIHgxPSIyIiB",
    "5MT0iMTYiIHgyPSIzMCIgeTI9IjE2IiBzdHJva2U9IiMzMzMiIHN0cm9rZS13aWR0aD0iMS41Ii8+PC",
    "9zdmc+"
);

/// Inline SVG globe icon for dark backgrounds (~200 bytes).
pub(crate) const GLOBE_SVG_DARK: &str = concat!(
    "data:image/svg+xml;base64,",
    "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAzMiAzMiI+",
    "PGNpcmNsZSBjeD0iMTYiIGN5PSIxNiIgcj0iMTQiIGZpbGw9Im5vbmUiIHN0cm9rZT0iI2VlZSIgc3",
    "Ryb2tlLXdpZHRoPSIxLjUiLz48ZWxsaXBzZSBjeD0iMTYiIGN5PSIxNiIgcng9IjYiIHJ5PSIxNCIg",
    "ZmlsbD0ibm9uZSIgc3Ryb2tlPSIjZWVlIiBzdHJva2Utd2lkdGg9IjEuNSIvPjxsaW5lIHgxPSIyIiB",
    "5MT0iMTYiIHgyPSIzMCIgeTI9IjE2IiBzdHJva2U9IiNlZWUiIHN0cm9rZS13aWR0aD0iMS41Ii8+PC",
    "9zdmc+"
);

/// Build the server icon list: one light-theme and one dark-theme globe SVG.
///
/// Both icons use scalable SVG with `sizes: ["any"]` so clients can render them
/// at any resolution.  The data URIs embed the image inline — no external
/// requests are needed.
pub(crate) fn server_icons() -> Vec<rust_mcp_sdk::schema::Icon> {
    use rust_mcp_sdk::schema::{Icon, IconTheme};
    vec![
        Icon {
            src: GLOBE_SVG_LIGHT.to_string(),
            mime_type: Some("image/svg+xml".to_string()),
            sizes: vec!["any".to_string()],
            theme: Some(IconTheme::Light),
        },
        Icon {
            src: GLOBE_SVG_DARK.to_string(),
            mime_type: Some("image/svg+xml".to_string()),
            sizes: vec!["any".to_string()],
            theme: Some(IconTheme::Dark),
        },
    ]
}
