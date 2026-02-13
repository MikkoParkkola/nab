//! Plugin configuration loaded from `~/.config/nab/plugins.toml`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Configuration for a single plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    /// Human-readable plugin name.
    pub name: String,
    /// Path to the plugin binary.
    pub binary: PathBuf,
    /// URL regex patterns this plugin handles.
    pub patterns: Vec<String>,
}

/// Top-level plugins configuration file.
#[derive(Debug, Clone, Deserialize, Default)]
struct PluginsFile {
    #[serde(default)]
    plugins: Vec<PluginConfig>,
}

/// Load plugin configurations from `~/.config/nab/plugins.toml`.
///
/// Returns an empty vec if the file doesn't exist (plugins are optional).
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn load_plugins() -> Result<Vec<PluginConfig>> {
    let path = config_path();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let file: PluginsFile =
        toml::from_str(&content).with_context(|| format!("invalid TOML in {}", path.display()))?;

    Ok(file.plugins)
}

/// Return the path to the plugins config file.
fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nab")
        .join("plugins.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_config() {
        let toml_str = "";
        let file: PluginsFile = toml::from_str(toml_str).unwrap();
        assert!(file.plugins.is_empty());
    }

    #[test]
    fn parse_single_plugin() {
        let toml_str = r#"
[[plugins]]
name = "test-plugin"
binary = "/usr/local/bin/nab-plugin-test"
patterns = ["example\\.com/.*"]
"#;
        let file: PluginsFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.plugins.len(), 1);
        assert_eq!(file.plugins[0].name, "test-plugin");
        assert_eq!(
            file.plugins[0].binary,
            PathBuf::from("/usr/local/bin/nab-plugin-test")
        );
        assert_eq!(file.plugins[0].patterns, vec!["example\\.com/.*"]);
    }

    #[test]
    fn parse_multiple_plugins() {
        let toml_str = r#"
[[plugins]]
name = "plugin-a"
binary = "/usr/bin/a"
patterns = ["a\\.com/.*"]

[[plugins]]
name = "plugin-b"
binary = "/usr/bin/b"
patterns = ["b\\.com/.*", "c\\.org/.*"]
"#;
        let file: PluginsFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.plugins.len(), 2);
        assert_eq!(file.plugins[1].patterns.len(), 2);
    }
}
