//! Structured content builders and server icon constants for `nab-mcp`.
//!
//! Provides zero-allocation helpers for building `structuredContent` JSON maps
//! and the embedded SVG icons used in the MCP `InitializeResult`.

// ─── Constants ───────────────────────────────────────────────────────────────

/// Default character limit for single-URL tool responses (fetch, submit, login).
pub(crate) const TOOL_TRUNCATION_LIMIT: usize = 4000;

/// Character limit for batch preview snippets (`fetch_batch` per-URL).
pub(crate) const BATCH_PREVIEW_LIMIT: usize = 500;

// ─── Truncation helper ────────────────────────────────────────────────────────

/// Truncate markdown to `max_chars`, appending `\n\n... [truncated]` if needed.
pub(crate) fn truncate_markdown(text: &str, max_chars: usize) -> String {
    if text.len() > max_chars {
        let at = text.floor_char_boundary(max_chars);
        format!("{}\n\n... [truncated]", &text[..at])
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

/// Parameters for building a `fetch` tool structured response.
pub(crate) struct FetchStructuredParams<'a> {
    pub url: &'a str,
    pub status: u16,
    pub content_type: &'a str,
    pub markdown: &'a str,
    pub timing_ms: f64,
    pub has_diff: bool,
    pub omitted_sections: usize,
    pub total_sections: usize,
    pub truncated: bool,
    pub full_tokens: usize,
}

/// Build the `structuredContent` map for the `fetch` tool response.
///
/// Includes focus and budget metadata fields when applicable.
pub(crate) fn build_fetch_structured_v2(
    p: &FetchStructuredParams<'_>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = build_structured([
        ("url", serde_json::Value::String(p.url.to_string())),
        ("status", serde_json::Value::Number(p.status.into())),
        (
            "content_type",
            serde_json::Value::String(p.content_type.to_string()),
        ),
        ("content", serde_json::Value::String(p.markdown.to_string())),
        (
            "timing_ms",
            serde_json::Value::Number(
                serde_json::Number::from_f64(p.timing_ms).unwrap_or(serde_json::Number::from(0)),
            ),
        ),
        ("has_diff", serde_json::Value::Bool(p.has_diff)),
    ]);

    // Focus metadata (only present when focus was used)
    if p.total_sections > 0 {
        map.insert(
            "omitted_sections".to_string(),
            serde_json::Value::Number(p.omitted_sections.into()),
        );
        map.insert(
            "total_sections".to_string(),
            serde_json::Value::Number(p.total_sections.into()),
        );
    }

    // Budget metadata (only present when truncation occurred)
    if p.truncated {
        map.insert("truncated".to_string(), serde_json::Value::Bool(true));
        map.insert(
            "full_tokens".to_string(),
            serde_json::Value::Number(p.full_tokens.into()),
        );
    }

    map
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
