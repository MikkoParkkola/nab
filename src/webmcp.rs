//! `WebMCP` Discovery
//!
//! Chrome 146+ lets websites advertise structured MCP tool definitions that clients can
//! discover before falling back to HTML scraping.  This module provides **pure parsing and
//! URL-construction logic** — no I/O is performed here.  Callers supply raw bytes/strings
//! obtained through their own HTTP stack.
//!
//! ## Discovery flow
//!
//! 1. Fetch `{base}/.well-known/mcp.json` — standard well-known manifest.
//! 2. If not found, parse `<link rel="mcp" href="…">` from the page `<head>`.
//! 3. Fetch the manifest URL, parse with [`McpManifest::from_json`].
//! 4. Return [`DiscoveryResult::Found`]; caller skips HTML extraction.
//!
//! ## References
//!
//! - Chrome `WebMCP` design doc: <https://docs.google.com/document/d/1rtU1fRPS0bMqd9abMG_hc6K9OAI6soUy3Kh00toAgyk>

use serde::Deserialize;

// ─── Public types ────────────────────────────────────────────────────────────

/// An MCP tool exposed by a website.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    /// Machine-readable tool name (e.g. `"search"`, `"add_to_cart"`).
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema string for the tool's input parameters, if provided.
    pub input_schema: Option<String>,
}

/// A parsed `WebMCP` manifest returned by a site.
#[derive(Debug, Clone, PartialEq)]
pub struct McpManifest {
    /// Human-readable name of the site / tool provider.
    pub name: String,
    /// Human-readable description of what the site offers.
    pub description: String,
    /// URL of the MCP server that can actually invoke the tools.
    pub server_url: Option<String>,
    /// Advertised tools.
    pub tools: Vec<McpTool>,
}

/// Outcome of a discovery attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryResult {
    /// A valid manifest was found and parsed.
    Found(McpManifest),
    /// No `WebMCP` manifest could be located.
    NotFound,
    /// A manifest was located but could not be parsed.
    Error(String),
}

// ─── Wire-format (private serde types) ───────────────────────────────────────

#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "serverUrl")]
    server_url: Option<String>,
    #[serde(default)]
    tools: Vec<RawTool>,
}

#[derive(Deserialize)]
struct RawTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Option<serde_json::Value>,
}

// ─── Core logic ──────────────────────────────────────────────────────────────

impl McpManifest {
    /// Parse a `WebMCP` manifest from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `json` is not valid JSON or does not conform to the
    /// expected manifest shape.
    ///
    /// # Example
    ///
    /// ```rust
    /// use nab::webmcp::McpManifest;
    ///
    /// let json = r#"{"name":"Acme","description":"Shop","tools":[]}"#;
    /// let manifest = McpManifest::from_json(json).unwrap();
    /// assert_eq!(manifest.name, "Acme");
    /// ```
    pub fn from_json(json: &str) -> Result<Self, String> {
        let raw: RawManifest =
            serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
        Ok(Self::from_raw(raw))
    }

    fn from_raw(raw: RawManifest) -> Self {
        Self {
            name: raw.name,
            description: raw.description,
            server_url: raw.server_url,
            tools: raw.tools.into_iter().map(McpTool::from_raw).collect(),
        }
    }
}

impl McpTool {
    fn from_raw(raw: RawTool) -> Self {
        let input_schema = raw
            .input_schema
            .map(|v| serde_json::to_string(&v).unwrap_or_default());
        Self {
            name: raw.name,
            description: raw.description,
            input_schema,
        }
    }
}

// ─── Discovery helpers ───────────────────────────────────────────────────────

/// Construct the well-known manifest URL for `base_url`.
///
/// Strips any trailing path/query from `base_url` so the result is always
/// `{scheme}://{host}/.well-known/mcp.json`.
///
/// Returns `None` when `base_url` cannot be parsed.
///
/// # Example
///
/// ```rust
/// use nab::webmcp::well_known_url;
///
/// assert_eq!(
///     well_known_url("https://example.com/some/page"),
///     Some("https://example.com/.well-known/mcp.json".to_owned()),
/// );
/// ```
#[must_use]
pub fn well_known_url(base_url: &str) -> Option<String> {
    let parsed = url::Url::parse(base_url).ok()?;
    let origin = parsed.origin().ascii_serialization();
    Some(format!("{origin}/.well-known/mcp.json"))
}

/// Extract the `href` value from the first `<link rel="mcp" href="…">` tag
/// found in `html`.
///
/// The search is case-insensitive for `rel` and `href` attribute names.
/// Returns `None` when no matching `<link>` is present.
///
/// # Example
///
/// ```rust
/// use nab::webmcp::extract_link_href;
///
/// let html = r#"<html><head>
///     <link rel="mcp" href="/mcp-manifest.json">
/// </head></html>"#;
/// assert_eq!(extract_link_href(html), Some("/mcp-manifest.json".to_owned()));
/// ```
#[must_use]
pub fn extract_link_href(html: &str) -> Option<String> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let selector = Selector::parse(r#"link[rel~="mcp"]"#).ok()?;
    document
        .select(&selector)
        .find_map(|el| el.value().attr("href").map(str::to_owned))
}

/// Resolve a manifest `href` against an origin URL.
///
/// Handles both absolute hrefs (returned unchanged) and relative hrefs
/// (resolved against `base_url`'s origin).
///
/// Returns `None` when either URL cannot be parsed.
///
/// # Example
///
/// ```rust
/// use nab::webmcp::resolve_manifest_url;
///
/// assert_eq!(
///     resolve_manifest_url("https://example.com/page", "/manifest.json"),
///     Some("https://example.com/manifest.json".to_owned()),
/// );
/// assert_eq!(
///     resolve_manifest_url("https://example.com", "https://cdn.example.com/mcp.json"),
///     Some("https://cdn.example.com/mcp.json".to_owned()),
/// );
/// ```
#[must_use]
pub fn resolve_manifest_url(base_url: &str, href: &str) -> Option<String> {
    let base = url::Url::parse(base_url).ok()?;
    let resolved = base.join(href).ok()?;
    Some(resolved.to_string())
}

/// Attempt to build a [`DiscoveryResult`] from raw manifest bytes.
///
/// Called by the fetch layer after a successful HTTP GET of the manifest URL.
/// Parses the JSON and returns [`DiscoveryResult::Found`] or
/// [`DiscoveryResult::Error`].
#[must_use]
pub fn parse_manifest_bytes(bytes: &[u8]) -> DiscoveryResult {
    match std::str::from_utf8(bytes) {
        Ok(json) => match McpManifest::from_json(json) {
            Ok(manifest) => DiscoveryResult::Found(manifest),
            Err(msg) => DiscoveryResult::Error(msg),
        },
        Err(e) => DiscoveryResult::Error(format!("invalid UTF-8: {e}")),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── McpManifest::from_json ────────────────────────────────────────────────

    #[test]
    fn parse_minimal_manifest_succeeds() {
        // GIVEN: bare-minimum manifest with only required-ish fields
        let json = r#"{"name":"Shop","description":"Buy things","tools":[]}"#;
        // WHEN
        let manifest = McpManifest::from_json(json).unwrap();
        // THEN
        assert_eq!(manifest.name, "Shop");
        assert_eq!(manifest.description, "Buy things");
        assert!(manifest.tools.is_empty());
        assert!(manifest.server_url.is_none());
    }

    #[test]
    fn parse_manifest_with_server_url() {
        // GIVEN: manifest with optional serverUrl
        let json =
            r#"{"name":"X","description":"Y","serverUrl":"https://mcp.example.com","tools":[]}"#;
        // WHEN
        let manifest = McpManifest::from_json(json).unwrap();
        // THEN
        assert_eq!(
            manifest.server_url.as_deref(),
            Some("https://mcp.example.com")
        );
    }

    #[test]
    fn parse_manifest_with_multiple_tools() {
        // GIVEN: manifest with two tools
        let json = r#"{
            "name":"Docs",
            "description":"Documentation site",
            "tools":[
                {"name":"search","description":"Full-text search"},
                {"name":"toc","description":"Table of contents"}
            ]
        }"#;
        // WHEN
        let manifest = McpManifest::from_json(json).unwrap();
        // THEN
        assert_eq!(manifest.tools.len(), 2);
        assert_eq!(manifest.tools[0].name, "search");
        assert_eq!(manifest.tools[1].name, "toc");
    }

    #[test]
    fn parse_tool_with_input_schema() {
        // GIVEN: tool that includes an inputSchema
        let json = r#"{
            "name":"Site",
            "description":"",
            "tools":[{
                "name":"search",
                "description":"Search",
                "inputSchema":{"type":"object","properties":{"q":{"type":"string"}}}
            }]
        }"#;
        // WHEN
        let manifest = McpManifest::from_json(json).unwrap();
        // THEN: schema is serialised back to a JSON string
        let schema = manifest.tools[0].input_schema.as_deref().unwrap();
        assert!(schema.contains("\"type\""));
        assert!(schema.contains("object"));
    }

    #[test]
    fn parse_empty_object_returns_defaults() {
        // GIVEN: entirely empty JSON object
        let json = r#"{}"#;
        // WHEN
        let manifest = McpManifest::from_json(json).unwrap();
        // THEN: all defaults apply
        assert_eq!(manifest.name, "");
        assert_eq!(manifest.description, "");
        assert!(manifest.tools.is_empty());
        assert!(manifest.server_url.is_none());
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        // GIVEN: malformed JSON
        // WHEN
        let result = McpManifest::from_json("not json at all");
        // THEN
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.starts_with("invalid JSON:"), "unexpected: {msg}");
    }

    #[test]
    fn parse_wrong_type_returns_error() {
        // GIVEN: JSON array instead of object
        // WHEN
        let result = McpManifest::from_json(r#"["search","toc"]"#);
        // THEN
        assert!(result.is_err());
    }

    // ── well_known_url ────────────────────────────────────────────────────────

    #[test]
    fn well_known_url_strips_path() {
        // GIVEN: URL with deep path
        // WHEN / THEN
        assert_eq!(
            well_known_url("https://example.com/docs/getting-started"),
            Some("https://example.com/.well-known/mcp.json".to_owned()),
        );
    }

    #[test]
    fn well_known_url_handles_root() {
        assert_eq!(
            well_known_url("https://example.com"),
            Some("https://example.com/.well-known/mcp.json".to_owned()),
        );
    }

    #[test]
    fn well_known_url_preserves_non_standard_port() {
        assert_eq!(
            well_known_url("http://localhost:8080/api"),
            Some("http://localhost:8080/.well-known/mcp.json".to_owned()),
        );
    }

    #[test]
    fn well_known_url_returns_none_for_invalid() {
        assert!(well_known_url("not a url").is_none());
    }

    // ── extract_link_href ─────────────────────────────────────────────────────

    #[test]
    fn extract_link_href_finds_mcp_link() {
        // GIVEN: minimal HTML with a <link rel="mcp"> tag
        let html = r#"<html><head><link rel="mcp" href="/mcp.json"></head></html>"#;
        // WHEN / THEN
        assert_eq!(extract_link_href(html), Some("/mcp.json".to_owned()),);
    }

    #[test]
    fn extract_link_href_returns_none_when_absent() {
        // GIVEN: HTML without any mcp link
        let html = r#"<html><head><link rel="stylesheet" href="/style.css"></head></html>"#;
        // WHEN / THEN
        assert!(extract_link_href(html).is_none());
    }

    #[test]
    fn extract_link_href_returns_absolute_url() {
        // GIVEN: link with absolute href
        let html = r#"<link rel="mcp" href="https://cdn.example.com/mcp-manifest.json">"#;
        // WHEN / THEN
        assert_eq!(
            extract_link_href(html),
            Some("https://cdn.example.com/mcp-manifest.json".to_owned()),
        );
    }

    // ── resolve_manifest_url ──────────────────────────────────────────────────

    #[test]
    fn resolve_manifest_url_relative_path() {
        assert_eq!(
            resolve_manifest_url("https://example.com/page", "/mcp.json"),
            Some("https://example.com/mcp.json".to_owned()),
        );
    }

    #[test]
    fn resolve_manifest_url_absolute_href_passthrough() {
        assert_eq!(
            resolve_manifest_url("https://example.com", "https://cdn.example.com/mcp.json"),
            Some("https://cdn.example.com/mcp.json".to_owned()),
        );
    }

    #[test]
    fn resolve_manifest_url_invalid_base_returns_none() {
        assert!(resolve_manifest_url("not-a-url", "/mcp.json").is_none());
    }

    // ── parse_manifest_bytes ──────────────────────────────────────────────────

    #[test]
    fn parse_manifest_bytes_valid_json() {
        // GIVEN
        let bytes = br#"{"name":"Acme","description":"","tools":[]}"#;
        // WHEN
        let result = parse_manifest_bytes(bytes);
        // THEN
        assert!(matches!(result, DiscoveryResult::Found(_)));
        if let DiscoveryResult::Found(m) = result {
            assert_eq!(m.name, "Acme");
        }
    }

    #[test]
    fn parse_manifest_bytes_invalid_json_returns_error() {
        let result = parse_manifest_bytes(b"garbage");
        assert!(matches!(result, DiscoveryResult::Error(_)));
    }

    #[test]
    fn parse_manifest_bytes_invalid_utf8_returns_error() {
        // GIVEN: non-UTF-8 bytes
        let bytes: &[u8] = &[0xFF, 0xFE, 0x00];
        // WHEN
        let result = parse_manifest_bytes(bytes);
        // THEN
        assert!(matches!(result, DiscoveryResult::Error(ref msg) if msg.contains("UTF-8")));
    }
}
