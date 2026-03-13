//! Plugin configuration loaded from `~/.config/nab/plugins.toml`.
//!
//! Two plugin types are supported:
//!
//! - **`type = "binary"`** (default): an external binary that receives a URL on
//!   stdin and returns JSON on stdout.  Requires the `binary` field.
//! - **`type = "css"`**: a CSS-selector-based extractor built into nab.
//!   Requires `patterns`, `content.selector`, and optionally `metadata.*`.
//!
//! Existing binary-plugin entries that omit `type` continue to work unchanged.
//!
//! # Example
//!
//! ```toml
//! # Binary plugin (original format)
//! [[plugins]]
//! name    = "my-provider"
//! binary  = "/usr/local/bin/nab-plugin-example"
//! patterns = ["example\\.com/.*"]
//!
//! # CSS extractor (new)
//! [[plugins]]
//! name     = "internal-wiki"
//! type     = "css"
//! patterns = ["wiki\\.internal\\.corp/.*"]
//!
//! [plugins.content]
//! selector = "div.wiki-content"
//! remove   = ["div.sidebar", "nav.breadcrumbs"]
//!
//! [plugins.metadata]
//! title     = "h1.page-title"
//! author    = ".author-name"
//! published = "time.published"
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

// ─────────────────────────────────────────────────────────────────────────────
// Public config types
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for a single binary plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    /// Human-readable plugin name.
    pub name: String,
    /// Path to the plugin binary.
    pub binary: PathBuf,
    /// URL regex patterns this plugin handles.
    pub patterns: Vec<String>,
}

/// CSS `content` sub-table in a CSS plugin entry.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CssContentConfig {
    /// CSS selector for the main content container.
    #[serde(default)]
    pub selector: String,
    /// CSS selectors for elements to remove before extraction.
    #[serde(default)]
    pub remove: Vec<String>,
}

/// CSS `metadata` sub-table in a CSS plugin entry.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CssMetadataConfig {
    /// CSS selector for the page title element.
    pub title: Option<String>,
    /// CSS selector for the author element.
    pub author: Option<String>,
    /// CSS selector for the publication-date element.
    pub published: Option<String>,
}

/// Configuration for a CSS extractor plugin.
#[derive(Debug, Clone)]
pub struct CssPluginConfig {
    /// Human-readable extractor name.
    pub name: String,
    /// URL regex patterns this extractor handles.
    pub patterns: Vec<String>,
    /// Content extraction settings.
    pub content: CssContentConfig,
    /// Optional metadata selectors.
    pub metadata: CssMetadataConfig,
}

/// Result of loading `plugins.toml`, partitioned by plugin type.
#[derive(Debug, Default)]
pub struct LoadedPlugins {
    /// Binary (external-process) plugins.
    pub binary: Vec<PluginConfig>,
    /// CSS extractor plugins.
    pub css: Vec<CssPluginConfig>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal raw TOML types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PluginType {
    #[default]
    Binary,
    Css,
}

/// Raw TOML shape for a single `[[plugins]]` entry.
#[derive(Debug, Clone, Deserialize)]
struct RawPluginEntry {
    name: String,
    #[serde(rename = "type", default)]
    plugin_type: PluginType,
    // binary fields
    binary: Option<PathBuf>,
    #[serde(default)]
    patterns: Vec<String>,
    // css fields
    #[serde(default)]
    content: CssContentConfig,
    #[serde(default)]
    metadata: CssMetadataConfig,
}

/// Top-level TOML file structure.
#[derive(Debug, Clone, Deserialize, Default)]
struct PluginsFile {
    #[serde(default)]
    plugins: Vec<RawPluginEntry>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Load plugin configurations from `~/.config/nab/plugins.toml`.
///
/// Returns an empty vec if the file doesn't exist (plugins are optional).
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn load_plugins() -> Result<Vec<PluginConfig>> {
    Ok(load_all_plugins()?.binary)
}

/// Load all plugins (binary + CSS) from `~/.config/nab/plugins.toml`.
///
/// Returns empty lists if the file doesn't exist.
/// Invalid or incomplete entries are skipped with a `tracing::warn` log.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or is not valid TOML.
pub fn load_all_plugins() -> Result<LoadedPlugins> {
    let path = config_path();
    if !path.exists() {
        return Ok(LoadedPlugins::default());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let file: PluginsFile =
        toml::from_str(&content).with_context(|| format!("invalid TOML in {}", path.display()))?;

    let mut loaded = LoadedPlugins::default();

    for entry in file.plugins {
        match entry.plugin_type {
            PluginType::Binary => match entry.binary {
                Some(binary) => loaded.binary.push(PluginConfig {
                    name: entry.name,
                    binary,
                    patterns: entry.patterns,
                }),
                None => {
                    tracing::warn!(
                        "plugin '{}' has type=binary but no 'binary' field — skipping",
                        entry.name
                    );
                }
            },
            PluginType::Css => {
                if entry.content.selector.is_empty() {
                    tracing::warn!(
                        "CSS plugin '{}' has no content.selector — skipping",
                        entry.name
                    );
                    continue;
                }
                loaded.css.push(CssPluginConfig {
                    name: entry.name,
                    patterns: entry.patterns,
                    content: entry.content,
                    metadata: entry.metadata,
                });
            }
        }
    }

    Ok(loaded)
}

/// Return the path to the plugins config file.
fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nab")
        .join("plugins.toml")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_raw(toml_str: &str) -> PluginsFile {
        toml::from_str(toml_str).expect("valid TOML")
    }

    // ── backward compat: binary plugins ──────────────────────────────────────

    #[test]
    fn parse_empty_config() {
        let file = parse_raw("");
        assert!(file.plugins.is_empty());
    }

    #[test]
    fn parse_single_binary_plugin_no_type_field() {
        let toml_str = r#"
[[plugins]]
name    = "test-plugin"
binary  = "/usr/local/bin/nab-plugin-test"
patterns = ["example\\.com/.*"]
"#;
        let file = parse_raw(toml_str);
        assert_eq!(file.plugins.len(), 1);
        let p = &file.plugins[0];
        assert_eq!(p.name, "test-plugin");
        assert_eq!(
            p.binary,
            Some(PathBuf::from("/usr/local/bin/nab-plugin-test"))
        );
        assert_eq!(p.patterns, vec!["example\\.com/.*"]);
        assert_eq!(p.plugin_type, PluginType::Binary);
    }

    #[test]
    fn parse_explicit_type_binary() {
        let toml_str = r#"
[[plugins]]
name     = "explicit-binary"
type     = "binary"
binary   = "/usr/bin/plugin"
patterns = ["foo\\.com"]
"#;
        let file = parse_raw(toml_str);
        assert_eq!(file.plugins[0].plugin_type, PluginType::Binary);
    }

    #[test]
    fn parse_multiple_binary_plugins() {
        let toml_str = r#"
[[plugins]]
name    = "plugin-a"
binary  = "/usr/bin/a"
patterns = ["a\\.com/.*"]

[[plugins]]
name    = "plugin-b"
binary  = "/usr/bin/b"
patterns = ["b\\.com/.*", "c\\.org/.*"]
"#;
        let file = parse_raw(toml_str);
        assert_eq!(file.plugins.len(), 2);
        assert_eq!(file.plugins[1].patterns.len(), 2);
    }

    // ── CSS plugin parsing ────────────────────────────────────────────────────

    #[test]
    fn parse_css_plugin_minimal() {
        let toml_str = r#"
[[plugins]]
name     = "my-blog"
type     = "css"
patterns = ["myblog\\.com"]

[plugins.content]
selector = "article.post-content"
"#;
        let file = parse_raw(toml_str);
        assert_eq!(file.plugins.len(), 1);
        let p = &file.plugins[0];
        assert_eq!(p.plugin_type, PluginType::Css);
        assert_eq!(p.content.selector, "article.post-content");
        assert!(p.content.remove.is_empty());
        assert!(p.metadata.title.is_none());
    }

    #[test]
    fn parse_css_plugin_with_remove_selectors() {
        let toml_str = r#"
[[plugins]]
name     = "wiki"
type     = "css"
patterns = ["wiki\\.example\\.com"]

[plugins.content]
selector = "div.wiki-body"
remove   = ["nav", ".ads", ".sidebar"]
"#;
        let file = parse_raw(toml_str);
        let p = &file.plugins[0];
        assert_eq!(p.content.remove, vec!["nav", ".ads", ".sidebar"]);
    }

    #[test]
    fn parse_css_plugin_with_full_metadata() {
        let toml_str = r#"
[[plugins]]
name     = "docs"
type     = "css"
patterns = ["docs\\.example\\.com"]

[plugins.content]
selector = "main.content"

[plugins.metadata]
title     = "h1.page-title"
author    = ".author-name"
published = "time[datetime]"
"#;
        let file = parse_raw(toml_str);
        let p = &file.plugins[0];
        assert_eq!(p.metadata.title.as_deref(), Some("h1.page-title"));
        assert_eq!(p.metadata.author.as_deref(), Some(".author-name"));
        assert_eq!(p.metadata.published.as_deref(), Some("time[datetime]"));
    }

    #[test]
    fn parse_css_plugin_without_metadata_section() {
        let toml_str = r#"
[[plugins]]
name     = "no-meta"
type     = "css"
patterns = ["example\\.com"]

[plugins.content]
selector = "article"
"#;
        let file = parse_raw(toml_str);
        let p = &file.plugins[0];
        assert!(p.metadata.title.is_none());
        assert!(p.metadata.author.is_none());
        assert!(p.metadata.published.is_none());
    }

    // ── mixed binary + CSS ────────────────────────────────────────────────────

    #[test]
    fn parse_mixed_binary_and_css_entries() {
        let toml_str = r#"
[[plugins]]
name    = "binary-plugin"
binary  = "/usr/bin/bin-plugin"
patterns = ["bin\\.example\\.com"]

[[plugins]]
name     = "css-plugin"
type     = "css"
patterns = ["css\\.example\\.com"]

[plugins.content]
selector = "main"
"#;
        let file = parse_raw(toml_str);
        assert_eq!(file.plugins.len(), 2);
        assert_eq!(file.plugins[0].plugin_type, PluginType::Binary);
        assert_eq!(file.plugins[1].plugin_type, PluginType::Css);
    }

    // ── partition logic (mirrors load_all_plugins internals) ─────────────────

    fn loaded_from_str(toml_str: &str) -> LoadedPlugins {
        let file: PluginsFile = toml::from_str(toml_str).unwrap();
        let mut loaded = LoadedPlugins::default();
        for entry in file.plugins {
            match entry.plugin_type {
                PluginType::Binary => {
                    if let Some(binary) = entry.binary {
                        loaded.binary.push(PluginConfig {
                            name: entry.name,
                            binary,
                            patterns: entry.patterns,
                        });
                    }
                }
                PluginType::Css => {
                    if !entry.content.selector.is_empty() {
                        loaded.css.push(CssPluginConfig {
                            name: entry.name,
                            patterns: entry.patterns,
                            content: entry.content,
                            metadata: entry.metadata,
                        });
                    }
                }
            }
        }
        loaded
    }

    #[test]
    fn loaded_from_empty_config_has_no_plugins() {
        let l = loaded_from_str("");
        assert!(l.binary.is_empty());
        assert!(l.css.is_empty());
    }

    #[test]
    fn loaded_binary_plugin_is_in_binary_list() {
        let toml_str = r#"
[[plugins]]
name    = "bp"
binary  = "/usr/bin/bp"
patterns = ["bp\\.com"]
"#;
        let l = loaded_from_str(toml_str);
        assert_eq!(l.binary.len(), 1);
        assert!(l.css.is_empty());
        assert_eq!(l.binary[0].name, "bp");
    }

    #[test]
    fn loaded_css_plugin_is_in_css_list() {
        let toml_str = r#"
[[plugins]]
name     = "cp"
type     = "css"
patterns = ["cp\\.com"]

[plugins.content]
selector = "article"
"#;
        let l = loaded_from_str(toml_str);
        assert!(l.binary.is_empty());
        assert_eq!(l.css.len(), 1);
        assert_eq!(l.css[0].name, "cp");
        assert_eq!(l.css[0].content.selector, "article");
    }

    #[test]
    fn css_plugin_without_content_selector_is_skipped() {
        let toml_str = r#"
[[plugins]]
name     = "bad-css"
type     = "css"
patterns = ["bad\\.com"]

[plugins.content]
selector = ""
"#;
        let l = loaded_from_str(toml_str);
        assert!(l.css.is_empty());
    }

    #[test]
    fn mixed_plugins_correctly_partitioned() {
        let toml_str = r#"
[[plugins]]
name    = "bin1"
binary  = "/usr/bin/bin1"
patterns = ["bin1\\.com"]

[[plugins]]
name     = "css1"
type     = "css"
patterns = ["css1\\.com"]

[plugins.content]
selector = "main"

[[plugins]]
name    = "bin2"
binary  = "/usr/bin/bin2"
patterns = ["bin2\\.com"]
"#;
        let l = loaded_from_str(toml_str);
        assert_eq!(l.binary.len(), 2);
        assert_eq!(l.css.len(), 1);
        assert_eq!(l.binary[0].name, "bin1");
        assert_eq!(l.binary[1].name, "bin2");
        assert_eq!(l.css[0].name, "css1");
    }
}
