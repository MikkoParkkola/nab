//! Plugin runner that implements [`SiteProvider`] for external binaries.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::http_client::AcceleratedClient;
use crate::site::{SiteContent, SiteMetadata, SiteProvider};

use super::config::PluginConfig;

/// JSON sent to the plugin on stdin.
#[derive(Serialize)]
struct PluginInput {
    url: String,
}

/// JSON expected from the plugin on stdout.
#[derive(Deserialize)]
struct PluginOutput {
    markdown: String,
    #[serde(default)]
    metadata: PluginMetadata,
}

/// Optional metadata returned by a plugin.
#[derive(Deserialize, Default)]
struct PluginMetadata {
    title: Option<String>,
    author: Option<String>,
    published: Option<String>,
}

/// Runs an external plugin binary as a [`SiteProvider`].
///
/// The plugin receives `{"url": "..."}` on stdin and must return
/// `{"markdown": "...", "metadata": {...}}` on stdout within 30 seconds.
pub struct PluginRunner {
    config: PluginConfig,
    patterns: Vec<Regex>,
}

impl PluginRunner {
    /// Create a runner from a plugin configuration.
    ///
    /// Compiles all URL patterns as regexes.
    ///
    /// # Errors
    ///
    /// Returns an error if any URL pattern is not a valid regex.
    pub fn new(config: PluginConfig) -> Result<Self> {
        let patterns = config
            .patterns
            .iter()
            .map(|p| {
                Regex::new(p)
                    .with_context(|| format!("invalid pattern '{p}' in plugin '{}'", config.name))
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self { config, patterns })
    }
}

#[async_trait]
impl SiteProvider for PluginRunner {
    fn name(&self) -> &'static str {
        // Leak the name so we can return &'static str.
        // Plugins are loaded once at startup, so this is fine.
        Box::leak(self.config.name.clone().into_boxed_str())
    }

    fn matches(&self, url: &str) -> bool {
        self.patterns.iter().any(|re| re.is_match(url))
    }

    async fn extract(&self, url: &str, _client: &AcceleratedClient) -> Result<SiteContent> {
        let binary = &self.config.binary;
        let plugin_name = &self.config.name;

        if !binary.exists() {
            bail!(
                "plugin '{plugin_name}' binary not found at {}",
                binary.display()
            );
        }

        let input = serde_json::to_string(&PluginInput {
            url: url.to_string(),
        })?;

        // Spawn the plugin binary in a blocking task since it does process I/O.
        let binary = binary.clone();
        let plugin_name = plugin_name.clone();
        let plugin_name_outer = plugin_name.clone();
        let url_owned = url.to_string();

        let output = tokio::task::spawn_blocking(move || -> Result<PluginOutput> {
            let mut child = Command::new(&binary)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("failed to spawn plugin '{plugin_name}'"))?;

            // Write input to stdin
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(input.as_bytes())
                    .with_context(|| format!("failed to write to plugin '{plugin_name}' stdin"))?;
            }

            // Wait for completion
            let result = child
                .wait_with_output()
                .with_context(|| format!("plugin '{plugin_name}' failed"))?;

            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                bail!(
                    "plugin '{plugin_name}' exited with {}: {}",
                    result.status,
                    stderr.trim()
                );
            }

            let stdout = String::from_utf8(result.stdout)
                .with_context(|| format!("plugin '{plugin_name}' output is not valid UTF-8"))?;

            serde_json::from_str::<PluginOutput>(&stdout).with_context(|| {
                format!(
                    "plugin '{plugin_name}' returned invalid JSON: {}",
                    &stdout[..stdout.len().min(200)]
                )
            })
        })
        .await
        .with_context(|| format!("plugin '{plugin_name_outer}' task panicked"))??;

        Ok(SiteContent {
            markdown: output.markdown,
            metadata: SiteMetadata {
                author: output.metadata.author,
                title: output.metadata.title,
                published: output.metadata.published,
                platform: format!("plugin:{}", self.config.name),
                canonical_url: url_owned,
                media_urls: Vec::new(),
                engagement: None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config(patterns: Vec<&str>) -> PluginConfig {
        PluginConfig {
            name: "test".to_string(),
            binary: PathBuf::from("/nonexistent"),
            patterns: patterns.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn matches_url_patterns() {
        let runner =
            PluginRunner::new(test_config(vec![r"example\.com/.*", r"test\.org/page"])).unwrap();
        assert!(runner.matches("https://example.com/anything"));
        assert!(runner.matches("https://test.org/page"));
        assert!(!runner.matches("https://other.com/page"));
    }

    #[test]
    fn rejects_invalid_regex() {
        let result = PluginRunner::new(test_config(vec![r"[invalid"]));
        assert!(result.is_err());
    }

    #[test]
    fn name_returns_config_name() {
        let runner = PluginRunner::new(test_config(vec![r".*"])).unwrap();
        assert_eq!(runner.name(), "test");
    }
}
