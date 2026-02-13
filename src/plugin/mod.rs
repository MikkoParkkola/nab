//! External plugin system for custom `SiteProvider` implementations.
//!
//! Plugins are external binaries that receive a URL and return structured content.
//! Protocol: binary receives JSON on stdin, returns JSON on stdout.
//!
//! # Configuration
//!
//! Plugins are defined in `~/.config/nab/plugins.toml`:
//!
//! ```toml
//! [[plugins]]
//! name = "my-provider"
//! binary = "/usr/local/bin/nab-plugin-example"
//! patterns = ["example\\.com/.*", "test\\.org/.*"]
//! ```
//!
//! # Protocol
//!
//! Input (JSON on stdin):
//! ```json
//! {"url": "https://example.com/page"}
//! ```
//!
//! Output (JSON on stdout):
//! ```json
//! {"markdown": "# Page\n\nContent...", "metadata": {"title": "Page"}}
//! ```

pub mod config;
pub mod runner;

pub use config::PluginConfig;
pub use runner::PluginRunner;
