// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Config-driven site rule engine.
//!
//! Loads TOML rule files from two sources (in priority order):
//!
//! 1. **User overrides** from `~/.config/nab/sites/{name}.toml` (highest priority)
//! 2. **Embedded defaults** compiled into the binary via `include_str!()`
//!
//! When a user override exists for a rule name, the embedded default for that
//! same name is skipped.  This lets users customise any built-in rule without
//! recompiling.
//!
//! Rule-based providers are intended to be loaded **before** hardcoded Rust
//! providers in [`SiteRouter`] so that user overrides take full effect.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::rules::load_site_rules;
//!
//! let providers = load_site_rules();
//! println!("Loaded {} rule-based providers", providers.len());
//! ```
//!
//! [`SiteRouter`]: crate::site::SiteRouter

pub mod config;
mod helpers;
pub mod json_path;
pub mod provider;
pub mod template;

use std::collections::HashSet;
use std::path::PathBuf;

use provider::ApiRuleProvider;

use super::SiteProvider;
use crate::site::rules::config::{RuleEngine, SiteConfig, SiteRuleConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Embedded defaults
// ─────────────────────────────────────────────────────────────────────────────

/// Returns all embedded default rule (name, `toml_content`) pairs.
pub fn embedded_rules() -> Vec<(&'static str, &'static str)> {
    vec![
        ("twitter", include_str!("defaults/twitter.toml")),
        ("youtube", include_str!("defaults/youtube.toml")),
        ("wikipedia", include_str!("defaults/wikipedia.toml")),
        ("mastodon", include_str!("defaults/mastodon.toml")),
        ("reddit", include_str!("defaults/reddit.toml")),
        ("stackoverflow", include_str!("defaults/stackoverflow.toml")),
        ("instagram", include_str!("defaults/instagram.toml")),
        ("github-issues", include_str!("defaults/github-issues.toml")),
        ("hackernews-item", include_str!("defaults/hackernews.toml")),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Load all rule-based providers, applying user overrides where present.
///
/// Processing order:
/// 1. Scan `~/.config/nab/sites/` for `*.toml` files — these become user
///    override providers.  The file stem (e.g., `twitter` for `twitter.toml`)
///    is treated as the rule name.
/// 2. Load embedded defaults for any name **not** already covered by a user
///    override.
///
/// Invalid rule files log a warning and are skipped; they never prevent valid
/// rules from loading.
///
/// Returns providers ordered: user overrides first, then embedded defaults.
pub fn load_site_rules() -> Vec<Box<dyn SiteProvider>> {
    let (overrides, overridden_names) = load_user_overrides();
    let defaults = load_embedded_defaults(&overridden_names);

    overrides.into_iter().chain(defaults).collect()
}

/// Return the configured engine for the first site rule that matches `url`.
///
/// User rules in `~/.config/nab/sites/*.toml` are checked before embedded
/// defaults. Browser-engine rules may contain only a `[site]` section; they
/// act as routing directives rather than API providers.
#[must_use]
pub fn engine_for_url(url: &str) -> Option<RuleEngine> {
    let (user_sites, overridden_names) = load_user_site_configs();

    if let Some(engine) = matching_engine(url, user_sites.iter()) {
        return Some(engine);
    }

    let embedded_sites = embedded_rules()
        .into_iter()
        .filter(|(name, _)| !overridden_names.contains(*name))
        .filter_map(|(name, toml)| match SiteRuleConfig::site_from_toml(toml) {
            Ok(site) => Some(site),
            Err(e) => {
                tracing::warn!("Skipping invalid embedded site rule '{name}': {e}");
                None
            }
        })
        .collect::<Vec<_>>();

    matching_engine(url, embedded_sites.iter())
}

/// Returns the set of rule names provided by embedded defaults.
///
/// Useful for [`SiteRouter`] to skip hardcoded Rust providers whose name
/// appears in this set (meaning the rule engine already handles them).
///
/// [`SiteRouter`]: crate::site::SiteRouter
pub fn rule_overridden_names() -> HashSet<String> {
    embedded_rules()
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Load user-supplied TOML overrides from `~/.config/nab/sites/`.
///
/// Returns `(providers, overridden_names)`.
fn load_user_overrides() -> (Vec<Box<dyn SiteProvider>>, HashSet<String>) {
    let sites_dir = user_sites_dir();
    let mut providers: Vec<Box<dyn SiteProvider>> = Vec::new();
    let mut names: HashSet<String> = HashSet::new();

    let Ok(entries) = std::fs::read_dir(&sites_dir) else {
        // Directory doesn't exist — no overrides.
        return (providers, names);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        match parse_rule_file(&path) {
            Ok(ParsedRuleFile::Provider { name, provider }) => {
                tracing::debug!(
                    "Loaded user site rule override: {name} from {}",
                    path.display()
                );
                names.insert(name);
                providers.push(provider);
            }
            Ok(ParsedRuleFile::DirectiveOnly { name, engine }) => {
                tracing::debug!(
                    "Loaded user site rule directive: {name} engine={} from {}",
                    engine.as_str(),
                    path.display()
                );
                names.insert(name);
            }
            Err(e) => {
                tracing::warn!("Skipping invalid site rule '{}': {e}", path.display());
            }
        }
    }

    (providers, names)
}

/// Load embedded default rules, skipping names in `overridden_names`.
fn load_embedded_defaults(overridden_names: &HashSet<String>) -> Vec<Box<dyn SiteProvider>> {
    embedded_rules()
        .into_iter()
        .filter(|(name, _)| !overridden_names.contains(*name))
        .filter_map(|(name, toml)| match parse_and_build(toml) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("Failed to load embedded rule '{name}': {e}");
                None
            }
        })
        .collect()
}

enum ParsedRuleFile {
    Provider {
        name: String,
        provider: Box<dyn SiteProvider>,
    },
    DirectiveOnly {
        name: String,
        engine: RuleEngine,
    },
}

/// Read a TOML file at `path`, parse it, and return the provider or directive.
fn parse_rule_file(path: &std::path::Path) -> anyhow::Result<ParsedRuleFile> {
    let toml = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read error: {e}"))?;
    parse_rule_toml(&toml)
}

/// Parse TOML content and return the provider or directive.
fn parse_rule_toml(toml: &str) -> anyhow::Result<ParsedRuleFile> {
    let site = SiteRuleConfig::site_from_toml(toml)?;
    if site.engine.is_browser() {
        return Ok(ParsedRuleFile::DirectiveOnly {
            name: site.name,
            engine: site.engine,
        });
    }

    let config = SiteRuleConfig::from_toml(toml)?;
    let name = config.site.name.clone();
    let provider = ApiRuleProvider::new(config)?;
    Ok(ParsedRuleFile::Provider {
        name,
        provider: Box::new(provider),
    })
}

/// Parse TOML content and build a boxed [`SiteProvider`].
pub(crate) fn parse_and_build(toml: &str) -> anyhow::Result<Box<dyn SiteProvider>> {
    let config = SiteRuleConfig::from_toml(toml)?;
    let provider = ApiRuleProvider::new(config)?;
    Ok(Box::new(provider))
}

/// Load user-supplied `[site]` sections, including browser-only directives.
fn load_user_site_configs() -> (Vec<SiteConfig>, HashSet<String>) {
    let sites_dir = user_sites_dir();
    let mut sites = Vec::new();
    let mut names = HashSet::new();

    let Ok(entries) = std::fs::read_dir(&sites_dir) else {
        return (sites, names);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let Ok(toml) = std::fs::read_to_string(&path) else {
            tracing::warn!("Skipping unreadable site rule '{}'", path.display());
            continue;
        };
        match SiteRuleConfig::site_from_toml(&toml) {
            Ok(site) => {
                names.insert(site.name.clone());
                sites.push(site);
            }
            Err(e) => tracing::warn!("Skipping invalid site rule '{}': {e}", path.display()),
        }
    }

    (sites, names)
}

fn matching_engine<'a>(
    url: &str,
    sites: impl IntoIterator<Item = &'a SiteConfig>,
) -> Option<RuleEngine> {
    for site in sites {
        for pattern in &site.patterns {
            if regex::Regex::new(pattern).is_ok_and(|re| re.is_match(url)) {
                return Some(site.engine);
            }
        }
    }
    None
}

/// Return `~/.config/nab/sites/`.
fn user_sites_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nab")
        .join("sites")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_rules_returns_nine_entries() {
        let rules = embedded_rules();
        assert_eq!(rules.len(), 9);
        let names: Vec<&str> = rules.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"twitter"));
        assert!(names.contains(&"youtube"));
        assert!(names.contains(&"wikipedia"));
        assert!(names.contains(&"mastodon"));
        assert!(names.contains(&"reddit"));
        assert!(names.contains(&"stackoverflow"));
        assert!(names.contains(&"instagram"));
        assert!(names.contains(&"github-issues"));
        assert!(names.contains(&"hackernews-item"));
    }

    #[test]
    fn embedded_rules_toml_content_is_non_empty() {
        for (name, toml) in embedded_rules() {
            assert!(!toml.is_empty(), "embedded rule '{name}' has empty content");
        }
    }

    #[test]
    fn embedded_rules_all_parse_successfully() {
        for (name, toml) in embedded_rules() {
            SiteRuleConfig::from_toml(toml)
                .unwrap_or_else(|e| panic!("embedded rule '{name}' failed to parse: {e}"));
        }
    }

    #[test]
    fn load_site_rules_returns_all_providers_no_user_overrides() {
        // Without any user config files, we get all embedded rules.
        let providers = load_site_rules();
        // At minimum we get the 8 embedded defaults (user may have overrides, but
        // the test env should not have them).
        assert!(providers.len() >= 9);
    }

    #[test]
    fn load_site_rules_providers_match_correct_urls() {
        let providers = load_site_rules();

        let twitter = providers.iter().find(|p| p.name() == "twitter");
        let youtube = providers.iter().find(|p| p.name() == "youtube");
        let wikipedia = providers.iter().find(|p| p.name() == "wikipedia");

        assert!(twitter.is_some(), "twitter provider should be loaded");
        assert!(youtube.is_some(), "youtube provider should be loaded");
        assert!(wikipedia.is_some(), "wikipedia provider should be loaded");

        assert!(twitter.unwrap().matches("https://x.com/user/status/123"));
        assert!(youtube.unwrap().matches("https://youtube.com/watch?v=abc"));
        assert!(
            wikipedia
                .unwrap()
                .matches("https://en.wikipedia.org/wiki/Rust")
        );

        let mastodon = providers.iter().find(|p| p.name() == "mastodon");
        assert!(mastodon.is_some(), "mastodon provider should be loaded");
        assert!(
            mastodon
                .unwrap()
                .matches("https://mastodon.social/@user/123456789")
        );

        let reddit = providers.iter().find(|p| p.name() == "reddit");
        assert!(reddit.is_some(), "reddit provider should be loaded");
        assert!(
            reddit
                .unwrap()
                .matches("https://www.reddit.com/r/rust/comments/abc123/some_title/")
        );

        let stackoverflow = providers.iter().find(|p| p.name() == "stackoverflow");
        assert!(
            stackoverflow.is_some(),
            "stackoverflow provider should be loaded"
        );
        assert!(
            stackoverflow
                .unwrap()
                .matches("https://stackoverflow.com/questions/12345/title")
        );

        let github_issues = providers.iter().find(|p| p.name() == "github-issues");
        assert!(
            github_issues.is_some(),
            "github-issues provider should be loaded"
        );
        assert!(
            github_issues
                .unwrap()
                .matches("https://github.com/rust-lang/rust/issues/12345")
        );

        let hackernews_item = providers.iter().find(|p| p.name() == "hackernews-item");
        assert!(
            hackernews_item.is_some(),
            "hackernews-item provider should be loaded"
        );
        assert!(
            hackernews_item
                .unwrap()
                .matches("https://news.ycombinator.com/item?id=12345")
        );
    }

    #[test]
    fn rule_overridden_names_contains_all_embedded_names() {
        let names = rule_overridden_names();
        assert!(names.contains("twitter"));
        assert!(names.contains("youtube"));
        assert!(names.contains("wikipedia"));
        assert!(names.contains("mastodon"));
        assert!(names.contains("reddit"));
        assert!(names.contains("stackoverflow"));
        assert!(names.contains("instagram"));
        assert!(names.contains("github-issues"));
        assert!(names.contains("hackernews-item"));
    }

    #[test]
    fn load_embedded_defaults_skips_overridden_name() {
        // Simulate "twitter" being overridden by user.
        let mut overridden = HashSet::new();
        overridden.insert("twitter".to_string());

        let defaults = load_embedded_defaults(&overridden);
        assert!(!defaults.iter().any(|p| p.name() == "twitter"));
        // Other eight still present.
        assert!(defaults.iter().any(|p| p.name() == "youtube"));
        assert!(defaults.iter().any(|p| p.name() == "wikipedia"));
        assert!(defaults.iter().any(|p| p.name() == "mastodon"));
        assert!(defaults.iter().any(|p| p.name() == "reddit"));
        assert!(defaults.iter().any(|p| p.name() == "stackoverflow"));
        assert!(defaults.iter().any(|p| p.name() == "instagram"));
        assert!(defaults.iter().any(|p| p.name() == "github-issues"));
        assert!(defaults.iter().any(|p| p.name() == "hackernews-item"));
    }

    #[test]
    fn load_embedded_defaults_empty_overrides_loads_all() {
        let defaults = load_embedded_defaults(&HashSet::new());
        assert_eq!(defaults.len(), 9);
    }

    #[test]
    fn parse_and_build_succeeds_for_all_embedded_rules() {
        for (name, toml) in embedded_rules() {
            parse_and_build(toml)
                .unwrap_or_else(|e| panic!("embedded rule '{name}' failed to build: {e}"));
        }
    }

    #[test]
    fn parse_and_build_fails_for_invalid_toml() {
        let result = parse_and_build("not valid toml %%%");
        assert!(result.is_err());
    }

    #[test]
    fn browser_engine_rule_parses_as_directive_only() {
        let toml = r#"
[site]
name = "linkedin-browser"
engine = "browser"
patterns = ["(?i)linkedin\\.com/in/"]
"#;

        match parse_rule_toml(toml).expect("browser directive") {
            ParsedRuleFile::DirectiveOnly { name, engine } => {
                assert_eq!(name, "linkedin-browser");
                assert_eq!(engine, RuleEngine::Browser);
            }
            ParsedRuleFile::Provider { .. } => panic!("browser rule must not build API provider"),
        }
    }

    #[test]
    fn matching_engine_returns_browser_for_matching_pattern() {
        let site = SiteConfig {
            name: "linkedin-browser".to_string(),
            engine: RuleEngine::Browser,
            patterns: vec![r"(?i)linkedin\.com/in/".to_string()],
        };

        assert_eq!(
            matching_engine("https://www.linkedin.com/in/example", [&site]),
            Some(RuleEngine::Browser)
        );
        assert_eq!(matching_engine("https://example.com", [&site]), None);
    }

    #[test]
    fn parse_and_build_rejects_browser_engine_api_provider() {
        let toml = r#"
[site]
name = "browser-api"
engine = "browser"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]
title = ".title"

[template]
format = "{title}"
"#;

        match parse_and_build(toml) {
            Ok(_) => panic!("browser engine rule must not build an API provider"),
            Err(err) => assert!(err.to_string().contains("engine='browser'")),
        }
    }

    #[test]
    fn user_sites_dir_returns_path_under_config() {
        let dir = user_sites_dir();
        assert!(dir.ends_with("nab/sites"));
    }
}
