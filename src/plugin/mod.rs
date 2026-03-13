//! External plugin system for custom `SiteProvider` implementations.
//!
//! Two plugin types are supported:
//!
//! - **Binary plugins**: External binaries that receive a URL on stdin and return
//!   JSON on stdout.  Protocol unchanged from v0.4.
//! - **CSS extractor plugins** (`type = "css"`): In-process extractors defined
//!   entirely in `plugins.toml`.  No external binary required.
//!
//! # Configuration
//!
//! Plugins are defined in `~/.config/nab/plugins.toml`:
//!
//! ```toml
//! # Binary plugin (original format, unchanged)
//! [[plugins]]
//! name = "my-provider"
//! binary = "/usr/local/bin/nab-plugin-example"
//! patterns = ["example\\.com/.*", "test\\.org/.*"]
//!
//! # CSS extractor (new in v0.5)
//! [[plugins]]
//! name     = "internal-wiki"
//! type     = "css"
//! patterns = ["wiki\\.internal\\.corp/.*"]
//!
//! [plugins.content]
//! selector = "div.wiki-content"
//! remove   = ["nav", ".ads"]
//!
//! [plugins.metadata]
//! title = "h1.page-title"
//! ```
//!
//! # Binary plugin protocol
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

pub use config::{CssPluginConfig, LoadedPlugins, PluginConfig, load_all_plugins};
pub use runner::PluginRunner;
