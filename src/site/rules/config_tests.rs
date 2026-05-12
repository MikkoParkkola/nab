// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Tests for [`SiteRuleConfig`] and related config types.

use super::*;

fn twitter_toml() -> &'static str {
    include_str!("defaults/twitter.toml")
}

fn youtube_toml() -> &'static str {
    include_str!("defaults/youtube.toml")
}

fn wikipedia_toml() -> &'static str {
    include_str!("defaults/wikipedia.toml")
}

fn stackoverflow_toml() -> &'static str {
    include_str!("defaults/stackoverflow.toml")
}

#[test]
fn parse_twitter_toml_succeeds() {
    let cfg = SiteRuleConfig::from_toml(twitter_toml()).unwrap();
    assert_eq!(cfg.site.name, "twitter");
    assert_eq!(cfg.site.engine, RuleEngine::Api);
    assert_eq!(cfg.site.patterns.len(), 1);
    assert!(cfg.site.patterns[0].contains("status"));
}

#[test]
fn site_engine_defaults_to_api() {
    let cfg = SiteRuleConfig::from_toml(youtube_toml()).unwrap();
    assert_eq!(cfg.site.engine, RuleEngine::Api);
}

#[test]
fn site_engine_browser_parses_from_site_section_only() {
    let toml_str = r#"
[site]
name = "linkedin-browser"
engine = "browser"
patterns = ["(?i)linkedin\\.com/in/"]
"#;

    let site = SiteRuleConfig::site_from_toml(toml_str).unwrap();
    assert_eq!(site.name, "linkedin-browser");
    assert_eq!(site.engine, RuleEngine::Browser);
    assert!(site.engine.is_browser());
}

#[test]
fn site_engine_rejects_unknown_value() {
    let toml_str = r#"
[site]
name = "bad-engine"
engine = "spaceship"
patterns = ["example\\.com"]
"#;

    let err = SiteRuleConfig::site_from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("failed to parse site rule TOML"));
}

#[test]
fn parse_youtube_toml_succeeds() {
    let cfg = SiteRuleConfig::from_toml(youtube_toml()).unwrap();
    assert_eq!(cfg.site.name, "youtube");
    assert_eq!(cfg.site.patterns.len(), 2);
    assert_eq!(
        cfg.rewrite.to,
        "https://www.youtube.com/oembed?url={url}&format=json"
    );
}

#[test]
fn parse_wikipedia_toml_succeeds() {
    let cfg = SiteRuleConfig::from_toml(wikipedia_toml()).unwrap();
    assert_eq!(cfg.site.name, "wikipedia");
    assert!(cfg.request.headers.contains_key("User-Agent"));
    assert_eq!(cfg.metadata.platform, "Wikipedia");
}

#[test]
fn parse_twitter_json_fields_all_present() {
    let cfg = SiteRuleConfig::from_toml(twitter_toml()).unwrap();
    let fields = &cfg.json.0;
    assert_eq!(
        fields.get("author_name").map(String::as_str),
        Some(".tweet.author.name")
    );
    assert_eq!(fields.get("text").map(String::as_str), Some(".tweet.text"));
    assert_eq!(
        fields.get("likes").map(String::as_str),
        Some(".tweet.likes")
    );
}

#[test]
fn parse_twitter_engagement_fields() {
    let cfg = SiteRuleConfig::from_toml(twitter_toml()).unwrap();
    assert_eq!(cfg.engagement.likes.as_deref(), Some("likes"));
    assert_eq!(cfg.engagement.reposts.as_deref(), Some("retweets"));
    assert_eq!(cfg.engagement.replies.as_deref(), Some("replies"));
    assert_eq!(cfg.engagement.views.as_deref(), Some("views"));
}

#[test]
fn validate_rejects_empty_name() {
    let toml_str = r#"
[site]
name = ""
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("name"));
}

#[test]
fn validate_rejects_empty_patterns() {
    let toml_str = r#"
[site]
name = "test"
patterns = []

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("patterns"));
}

#[test]
fn validate_rejects_invalid_pattern_regex() {
    let toml_str = r#"
[site]
name = "bad"
patterns = ["[invalid"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(
        err.to_string().contains("pattern")
            || err.to_string().contains("regex")
            || err.to_string().contains("invalid")
    );
}

#[test]
fn validate_rejects_invalid_rewrite_from_regex() {
    let toml_str = r#"
[site]
name = "bad"
patterns = ["foo\\.com"]

[rewrite]
from = "[invalid"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("rewrite") || err.to_string().contains("invalid"));
}

#[test]
fn validate_rejects_empty_template_format() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = ""
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("template"));
}

#[test]
fn request_config_defaults_to_empty() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert!(cfg.request.headers.is_empty());
    assert!(cfg.request.accept.is_none());
}

#[test]
fn request_config_client_defaults_to_default_variant() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.request.client, ClientKind::Default);
}

#[test]
fn request_config_client_parses_standard_variant() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
client = "standard"

[json]

[template]
format = "hello"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.request.client, ClientKind::Standard);
}

#[test]
fn request_config_client_parses_default_explicit() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
client = "default"

[json]

[template]
format = "hello"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.request.client, ClientKind::Default);
}

#[test]
fn parse_reddit_toml_succeeds() {
    let cfg = SiteRuleConfig::from_toml(include_str!("defaults/reddit.toml")).unwrap();
    assert_eq!(cfg.site.name, "reddit");
    assert_eq!(cfg.request.client, ClientKind::Standard);
    assert!(cfg.request.headers.contains_key("User-Agent"));
    // Verify key JSON paths are present
    let fields = &cfg.json.0;
    assert!(fields.contains_key("title"));
    assert!(fields.contains_key("author"));
    assert!(fields.contains_key("score"));
    assert!(fields.contains_key("comments"));
}

#[test]
fn additional_fetches_default_to_empty() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert!(cfg.additional_fetches.is_empty());
}

#[test]
fn parse_additional_fetch_with_json_fields() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com/q/(\\d+)"]

[rewrite]
from = "(?i)https?://example\\.com/q/(\\d+)"
to = "https://api.example.com/questions/$1"

[json]
title = ".items[0].title"

[template]
format = "{title}"

[[fetch_additional]]
prefix = "ans"
rewrite_from = "(?i)https?://example\\.com/q/(\\d+)"
rewrite_to = "https://api.example.com/questions/$1/answers"
accept = "application/json"

[fetch_additional.json]
body  = ".items[0].body"
score = ".items[0].score"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.additional_fetches.len(), 1);
    let af = &cfg.additional_fetches[0];
    assert_eq!(af.prefix, "ans");
    assert_eq!(
        af.rewrite_to,
        "https://api.example.com/questions/$1/answers"
    );
    assert_eq!(af.accept.as_deref(), Some("application/json"));
    assert_eq!(
        af.json.0.get("body").map(String::as_str),
        Some(".items[0].body")
    );
    assert_eq!(
        af.json.0.get("score").map(String::as_str),
        Some(".items[0].score")
    );
}

#[test]
fn parse_multiple_additional_fetches() {
    let toml_str = r#"
[site]
name = "multi"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com/primary"

[json]
title = ".title"

[template]
format = "{title}"

[[fetch_additional]]
prefix = "first"
rewrite_from = ".*"
rewrite_to = "https://api.example.com/first"

[fetch_additional.json]
x = ".x"

[[fetch_additional]]
prefix = "second"
rewrite_from = ".*"
rewrite_to = "https://api.example.com/second"

[fetch_additional.json]
y = ".y"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.additional_fetches.len(), 2);
    assert_eq!(cfg.additional_fetches[0].prefix, "first");
    assert_eq!(cfg.additional_fetches[1].prefix, "second");
}

#[test]
fn validate_rejects_empty_additional_fetch_prefix() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"

[[fetch_additional]]
prefix = ""
rewrite_from = ".*"
rewrite_to = "https://api.example.com/extra"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("prefix"));
}

#[test]
fn validate_rejects_invalid_additional_fetch_regex() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"

[[fetch_additional]]
prefix = "ans"
rewrite_from = "[invalid"
rewrite_to = "https://api.example.com/extra"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("fetch_additional") || err.to_string().contains("invalid"));
}

#[test]
fn parse_stackoverflow_toml_succeeds() {
    let cfg = SiteRuleConfig::from_toml(stackoverflow_toml()).unwrap();
    assert_eq!(cfg.site.name, "stackoverflow");
    assert_eq!(cfg.additional_fetches.len(), 1);
    let af = &cfg.additional_fetches[0];
    assert_eq!(af.prefix, "ans");
    assert!(af.json.0.contains_key("body"));
    assert!(af.json.0.contains_key("score"));
}

// ── AuthConfig unit tests ─────────────────────────────────────────────────

#[test]
fn auth_config_parses_env_bearer() {
    let cfg = AuthConfig::parse("env:GITHUB_TOKEN").unwrap();
    assert_eq!(cfg.env_var, "GITHUB_TOKEN");
    assert_eq!(cfg.header_name, "Authorization");
    assert!(cfg.bearer);
}

#[test]
fn auth_config_parses_env_custom_header() {
    let cfg = AuthConfig::parse("env:MY_KEY:header=X-Api-Key").unwrap();
    assert_eq!(cfg.env_var, "MY_KEY");
    assert_eq!(cfg.header_name, "X-Api-Key");
    assert!(!cfg.bearer);
}

#[test]
fn auth_config_parse_rejects_missing_env_prefix() {
    let err = AuthConfig::parse("token:GITHUB_TOKEN").unwrap_err();
    assert!(err.to_string().contains("env:"));
}

#[test]
fn auth_config_parse_rejects_bad_suffix() {
    let err = AuthConfig::parse("env:VAR:notaheader=x").unwrap_err();
    assert!(err.to_string().contains("header="));
}

#[test]
fn auth_config_parse_rejects_empty_header_name() {
    let err = AuthConfig::parse("env:VAR:header=").unwrap_err();
    assert!(err.to_string().contains("empty"));
}

#[test]
fn auth_config_resolve_returns_bearer_when_var_set() {
    unsafe { std::env::set_var("NAB_TEST_TOKEN_BEARER_CFG", "secret123") };
    let cfg = AuthConfig::parse("env:NAB_TEST_TOKEN_BEARER_CFG").unwrap();
    let resolved = cfg.resolve();
    unsafe { std::env::remove_var("NAB_TEST_TOKEN_BEARER_CFG") };
    assert_eq!(
        resolved,
        Some(("Authorization".to_string(), "Bearer secret123".to_string()))
    );
}

#[test]
fn auth_config_resolve_returns_custom_header_when_var_set() {
    unsafe { std::env::set_var("NAB_TEST_TOKEN_CUSTOM_CFG", "myapikey") };
    let cfg = AuthConfig::parse("env:NAB_TEST_TOKEN_CUSTOM_CFG:header=X-Api-Key").unwrap();
    let resolved = cfg.resolve();
    unsafe { std::env::remove_var("NAB_TEST_TOKEN_CUSTOM_CFG") };
    assert_eq!(
        resolved,
        Some(("X-Api-Key".to_string(), "myapikey".to_string()))
    );
}

#[test]
fn auth_config_resolve_returns_none_when_var_absent() {
    unsafe { std::env::remove_var("NAB_TEST_TOKEN_ABSENT_XYZ_CFG") };
    let cfg = AuthConfig::parse("env:NAB_TEST_TOKEN_ABSENT_XYZ_CFG").unwrap();
    assert!(cfg.resolve().is_none());
}

#[test]
fn request_config_auth_field_parses_from_toml() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
auth = "env:MY_TOKEN"

[json]

[template]
format = "hello"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.request.auth.as_deref(), Some("env:MY_TOKEN"));
}

#[test]
fn validate_rejects_malformed_auth_string() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
auth = "token:GITHUB_TOKEN"

[json]

[template]
format = "hello"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(err.to_string().contains("auth") || err.to_string().contains("env:"));
}

// ── FallbackConfig / FallbackType tests ───────────────────────────────────

#[test]
fn fallback_type_as_str_json() {
    assert_eq!(FallbackType::Json.as_str(), "json");
}

#[test]
fn fallback_type_as_str_html() {
    assert_eq!(FallbackType::Html.as_str(), "html");
}

#[test]
fn fallback_type_default_is_json() {
    assert_eq!(FallbackType::default(), FallbackType::Json);
}

#[test]
fn fallback_defaults_to_empty_vec() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert!(cfg.fallback.is_empty());
}

#[test]
fn parse_html_fallback_succeeds() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com/oembed?url={url}"

[json]
title = ".title"

[template]
format = "{title}"

[[fallback]]
rewrite_from = ".*"
rewrite_to = "{url}"
type = "html"

[fallback.css]
title = "meta[property='og:title']::attr(content)"
description = "meta[property='og:description']::attr(content)"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.fallback.len(), 1);
    let fb = &cfg.fallback[0];
    assert_eq!(fb.fallback_type, FallbackType::Html);
    assert_eq!(fb.rewrite_to, "{url}");
    assert_eq!(
        fb.css.get("title").map(String::as_str),
        Some("meta[property='og:title']::attr(content)")
    );
    assert_eq!(
        fb.css.get("description").map(String::as_str),
        Some("meta[property='og:description']::attr(content)")
    );
}

#[test]
fn parse_json_fallback_succeeds() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com/primary"

[json]
title = ".title"

[template]
format = "{title}"

[[fallback]]
rewrite_from = ".*"
rewrite_to = "https://api.example.com/fallback"

[fallback.json]
title = ".data.title"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.fallback.len(), 1);
    let fb = &cfg.fallback[0];
    assert_eq!(fb.fallback_type, FallbackType::Json);
    assert_eq!(
        fb.json.0.get("title").map(String::as_str),
        Some(".data.title")
    );
    assert!(fb.css.is_empty());
}

#[test]
fn parse_multiple_fallbacks_in_order() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]
title = ".title"

[template]
format = "{title}"

[[fallback]]
rewrite_from = ".*"
rewrite_to = "https://fallback1.example.com"

[fallback.json]
title = ".title"

[[fallback]]
rewrite_from = ".*"
rewrite_to = "{url}"
type = "html"

[fallback.css]
title = "h1"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.fallback.len(), 2);
    assert_eq!(cfg.fallback[0].fallback_type, FallbackType::Json);
    assert_eq!(cfg.fallback[1].fallback_type, FallbackType::Html);
}

#[test]
fn validate_rejects_invalid_fallback_regex() {
    let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"

[[fallback]]
rewrite_from = "[invalid"
rewrite_to = "{url}"
type = "html"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(
        err.to_string().contains("fallback") || err.to_string().contains("invalid"),
        "unexpected error: {err}"
    );
}

#[test]
fn parse_instagram_toml_succeeds() {
    let cfg = SiteRuleConfig::from_toml(include_str!("defaults/instagram.toml")).unwrap();
    assert_eq!(cfg.site.name, "instagram");
    assert!(cfg.json.0.contains_key("author_name"));
    assert!(cfg.json.0.contains_key("thumbnail_url"));
    assert_eq!(cfg.fallback.len(), 1);
    let fb = &cfg.fallback[0];
    assert_eq!(fb.fallback_type, FallbackType::Html);
    assert!(fb.css.contains_key("title"));
    assert!(fb.css.contains_key("description"));
    assert!(fb.css.contains_key("image"));
}

// ── ConcurrentFetchConfig tests ───────────────────────────────────────────

/// Minimal valid TOML with a single `[[fetch_concurrent]]` block.
fn concurrent_toml() -> &'static str {
    r#"
[site]
name = "hn"
patterns = ["news\\.ycombinator\\.com"]

[rewrite]
from = "(?i)https?://news\\.ycombinator\\.com.*"
to = "https://hacker-news.firebaseio.com/v0/topstories.json"

[json]

[template]
format = "{story_0_title}"

[[fetch_concurrent]]
prefix = "story"
rewrite_from = "(?i)https?://news\\.ycombinator\\.com.*"
rewrite_to = "https://hacker-news.firebaseio.com/v0/item/{id}.json"
items_path = "."

[fetch_concurrent.json]
title = ".title"
url   = ".url"
"#
}

#[test]
fn parse_concurrent_fetch_succeeds() {
    let cfg = SiteRuleConfig::from_toml(concurrent_toml()).unwrap();
    assert_eq!(cfg.concurrent_fetches.len(), 1);
    let cf = &cfg.concurrent_fetches[0];
    assert_eq!(cf.prefix, "story");
    assert_eq!(cf.items_path, ".");
    assert!(cf.json.0.contains_key("title"));
    assert!(cf.json.0.contains_key("url"));
}

#[test]
fn concurrent_fetch_item_limit_defaults() {
    let cfg = SiteRuleConfig::from_toml(concurrent_toml()).unwrap();
    let cf = &cfg.concurrent_fetches[0];
    assert_eq!(cf.item_limit(), 10);
}

#[test]
fn concurrent_fetch_custom_item_limit() {
    let toml_str = r#"
[site]
name = "hn"
patterns = ["news\\.ycombinator\\.com"]

[rewrite]
from = "(?i)https?://news\\.ycombinator\\.com.*"
to = "https://hacker-news.firebaseio.com/v0/topstories.json"

[json]

[template]
format = "{story_0_title}"

[[fetch_concurrent]]
prefix = "story"
rewrite_from = "(?i)https?://news\\.ycombinator\\.com.*"
rewrite_to = "https://hacker-news.firebaseio.com/v0/item/{id}.json"
items_path = "."
max_items = 5

[fetch_concurrent.json]
title = ".title"
"#;
    let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
    assert_eq!(cfg.concurrent_fetches[0].item_limit(), 5);
}

#[test]
fn validate_rejects_empty_concurrent_fetch_prefix() {
    let toml_str = r#"
[site]
name = "hn"
patterns = ["news\\.ycombinator\\.com"]

[rewrite]
from = ".*"
to = "https://hacker-news.firebaseio.com/v0/topstories.json"

[json]

[template]
format = "{story_0_title}"

[[fetch_concurrent]]
prefix = ""
rewrite_from = ".*"
rewrite_to = "https://hacker-news.firebaseio.com/v0/item/{id}.json"
items_path = "."

[fetch_concurrent.json]
title = ".title"
"#;
    let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
    assert!(
        err.to_string().contains("prefix"),
        "expected 'prefix' in error, got: {err}"
    );
}
