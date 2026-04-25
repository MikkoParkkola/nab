// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Tests for [`ApiRuleProvider`] and related helpers.

use serde_json::json;

use super::*;
use crate::site::rules::config::{FallbackType, SiteRuleConfig};
use crate::site::rules::helpers::*;

fn make_provider(toml: &str) -> ApiRuleProvider {
    let cfg = SiteRuleConfig::from_toml(toml).expect("valid config");
    ApiRuleProvider::new(cfg).expect("valid provider")
}

fn twitter_provider() -> ApiRuleProvider {
    make_provider(include_str!("defaults/twitter.toml"))
}

fn youtube_provider() -> ApiRuleProvider {
    make_provider(include_str!("defaults/youtube.toml"))
}

fn wikipedia_provider() -> ApiRuleProvider {
    make_provider(include_str!("defaults/wikipedia.toml"))
}

// ── URL matching ──────────────────────────────────────────────────────────

#[test]
fn twitter_provider_matches_x_com_status() {
    let p = twitter_provider();
    assert!(p.matches("https://x.com/naval/status/1234567890"));
    assert!(p.matches("https://twitter.com/user/status/999"));
    assert!(p.matches("https://X.COM/User/status/123?ref=foo"));
}

#[test]
fn twitter_provider_does_not_match_profile_urls() {
    let p = twitter_provider();
    assert!(!p.matches("https://x.com/naval"));
    assert!(!p.matches("https://twitter.com/elonmusk"));
}

#[test]
fn youtube_provider_matches_watch_and_short_urls() {
    let p = youtube_provider();
    assert!(p.matches("https://youtube.com/watch?v=abc123"));
    assert!(p.matches("https://www.youtube.com/watch?v=XYZ"));
    assert!(p.matches("https://youtu.be/dQw4w9WgXcQ"));
}

#[test]
fn youtube_provider_does_not_match_channel_urls() {
    let p = youtube_provider();
    assert!(!p.matches("https://youtube.com/channel/UCxyz"));
    assert!(!p.matches("https://youtube.com/"));
}

#[test]
fn wikipedia_provider_matches_wiki_urls() {
    let p = wikipedia_provider();
    assert!(p.matches("https://en.wikipedia.org/wiki/Rust"));
    assert!(p.matches("https://fi.wikipedia.org/wiki/Helsinki"));
    assert!(p.matches("https://de.wikipedia.org/wiki/Test"));
}

#[test]
fn wikipedia_provider_does_not_match_non_wiki_paths() {
    let p = wikipedia_provider();
    assert!(!p.matches("https://en.wikipedia.org/w/index.php"));
    assert!(!p.matches("https://en.wikipedia.org/"));
}

// ── URL rewriting ──────────────────────────────────────────────────────────

#[test]
fn twitter_rewrite_constructs_fxtwitter_url() {
    let p = twitter_provider();
    let rewritten = p.rewrite_url("https://x.com/naval/status/1234567890");
    assert_eq!(
        rewritten,
        "https://api.fxtwitter.com/naval/status/1234567890"
    );
}

#[test]
fn twitter_rewrite_works_for_twitter_com() {
    let p = twitter_provider();
    let rewritten = p.rewrite_url("https://twitter.com/elonmusk/status/9876543210");
    assert_eq!(
        rewritten,
        "https://api.fxtwitter.com/elonmusk/status/9876543210"
    );
}

#[test]
fn youtube_rewrite_uses_oembed_url_encoding() {
    let p = youtube_provider();
    let original = "https://youtube.com/watch?v=dQw4w9WgXcQ";
    let rewritten = p.rewrite_url(original);
    assert!(rewritten.starts_with("https://www.youtube.com/oembed?url="));
    assert!(rewritten.contains("youtube.com"));
    assert!(rewritten.ends_with("&format=json"));
}

#[test]
fn wikipedia_rewrite_constructs_rest_api_url() {
    let p = wikipedia_provider();
    let rewritten = p.rewrite_url("https://en.wikipedia.org/wiki/Rust_(programming_language)");
    assert_eq!(
        rewritten,
        "https://en.wikipedia.org/api/rest_v1/page/summary/Rust_(programming_language)"
    );
}

// ── field extraction ───────────────────────────────────────────────────────

#[test]
fn twitter_extract_fields_from_json() {
    let p = twitter_provider();
    let json = json!({
        "tweet": {
            "author": {"name": "Naval", "screen_name": "naval"},
            "text": "Build wealth, not status.",
            "likes": 8800,
            "retweets": 1000,
            "replies": 344,
            "views": 3_800_000,
            "created_at": "Wed Feb 12 10:00:00 +0000 2025",
            "url": "https://x.com/naval/status/123"
        }
    });
    let fields = p.extract_fields(&json);
    assert_eq!(fields.get("author_name").map(String::as_str), Some("Naval"));
    assert_eq!(
        fields.get("author_handle").map(String::as_str),
        Some("naval")
    );
    assert_eq!(
        fields.get("text").map(String::as_str),
        Some("Build wealth, not status.")
    );
    assert_eq!(fields.get("likes").map(String::as_str), Some("8800"));
}

#[test]
fn wikipedia_extract_thumbnail_path() {
    let p = wikipedia_provider();
    let json = json!({
        "title": "Rust",
        "description": "A systems programming language",
        "extract": "Rust is a language.",
        "thumbnail": {
            "source": "https://upload.wikimedia.org/rust.png"
        },
        "content_urls": {
            "desktop": {
                "page": "https://en.wikipedia.org/wiki/Rust"
            }
        }
    });
    let fields = p.extract_fields(&json);
    assert_eq!(
        fields.get("thumbnail").map(String::as_str),
        Some("https://upload.wikimedia.org/rust.png")
    );
    assert_eq!(
        fields.get("page_url").map(String::as_str),
        Some("https://en.wikipedia.org/wiki/Rust")
    );
}

// ── metadata building ──────────────────────────────────────────────────────

#[test]
fn twitter_build_metadata_author_template() {
    let p = twitter_provider();
    let mut fields = HashMap::new();
    fields.insert("author_name".to_string(), "Naval".to_string());
    fields.insert("author_handle".to_string(), "naval".to_string());
    fields.insert(
        "url".to_string(),
        "https://x.com/naval/status/123".to_string(),
    );

    let meta = p.build_metadata(&fields, "https://x.com/naval/status/123");
    assert_eq!(meta.platform, "Twitter/X");
    assert_eq!(meta.author.as_deref(), Some("Naval (@naval)"));
    assert_eq!(meta.canonical_url, "https://x.com/naval/status/123");
}

#[test]
fn wikipedia_build_metadata_title_and_url() {
    let p = wikipedia_provider();
    let mut fields = HashMap::new();
    fields.insert(
        "title".to_string(),
        "Rust (programming language)".to_string(),
    );
    fields.insert(
        "page_url".to_string(),
        "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string(),
    );
    fields.insert("timestamp".to_string(), "2025-01-01T00:00:00Z".to_string());

    let meta = p.build_metadata(
        &fields,
        "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    );
    assert_eq!(meta.platform, "Wikipedia");
    assert_eq!(meta.title.as_deref(), Some("Rust (programming language)"));
    assert_eq!(
        meta.canonical_url,
        "https://en.wikipedia.org/wiki/Rust_(programming_language)"
    );
    assert_eq!(meta.published.as_deref(), Some("2025-01-01T00:00:00Z"));
}

// ── engagement building ────────────────────────────────────────────────────

#[test]
fn twitter_builds_engagement_from_fields() {
    let p = twitter_provider();
    let mut fields = HashMap::new();
    fields.insert("likes".to_string(), "8800".to_string());
    fields.insert("retweets".to_string(), "1000".to_string());
    fields.insert("replies".to_string(), "344".to_string());
    fields.insert("views".to_string(), "3800000".to_string());

    let meta = p.build_metadata(&fields, "https://x.com/u/status/1");
    let eng = meta.engagement.unwrap();
    assert_eq!(eng.likes, Some(8800));
    assert_eq!(eng.reposts, Some(1000));
    assert_eq!(eng.replies, Some(344));
    assert_eq!(eng.views, Some(3_800_000));
}

#[test]
fn youtube_has_no_engagement() {
    let p = youtube_provider();
    let fields = HashMap::new();
    let meta = p.build_metadata(&fields, "https://youtube.com/watch?v=xyz");
    assert!(meta.engagement.is_none());
}

// ── success_path (request.success_path guard) ─────────────────────────────

#[test]
fn twitter_success_path_is_configured() {
    // GIVEN: the twitter provider (loaded from embedded TOML)
    let p = twitter_provider();
    // THEN: success_path is set to ".tweet" to detect null-tweet envelopes
    assert_eq!(
        p.config.request.success_path.as_deref(),
        Some(".tweet"),
        "twitter rule must have success_path = \".tweet\" to handle FxTwitter \
             404 envelopes ({{\"tweet\":null}})"
    );
}

#[test]
fn twitter_extract_fields_returns_empty_for_null_tweet_envelope() {
    // GIVEN: FxTwitter error response — HTTP 200 but tweet is null
    let p = twitter_provider();
    let json = serde_json::json!({"code": 404, "message": "NOT_FOUND", "tweet": null});
    // WHEN: extracting fields directly (simulates what try_primary_json sees)
    let fields = p.extract_fields(&json);
    // THEN: empty — all paths start with .tweet which is null
    assert!(
        fields.is_empty(),
        "extract_fields must return empty map for null tweet; got: {fields:?}"
    );
}

#[test]
fn provider_with_success_path_skips_extraction_when_path_is_null() {
    // GIVEN: a provider config with success_path = ".data"
    let toml = r#"
[site]
name = "test_guard"
patterns = ["example\\.com/.*"]

[rewrite]
from = ".*"
to   = "https://api.example.com/data"

[request]
success_path = ".data"

[json]
title = ".data.title"

[template]
format = "{title}"
"#;
    let p = make_provider(toml);
    // THEN: success_path is set correctly
    assert_eq!(p.config.request.success_path.as_deref(), Some(".data"));
    // AND: extracting from a null-data envelope yields an empty map
    let json = serde_json::json!({"status": "error", "data": null});
    let fields = p.extract_fields(&json);
    assert!(fields.is_empty());
}

#[test]
fn provider_without_success_path_extracts_despite_sibling_nulls() {
    // GIVEN: a provider with NO success_path and a response where some fields are null
    let toml = r#"
[site]
name = "test_no_guard"
patterns = ["example\\.com/.*"]

[rewrite]
from = ".*"
to   = "https://api.example.com/"

[json]
title  = ".title"
author = ".author"

[template]
format = "{title} by {author}"
"#;
    let p = make_provider(toml);
    assert!(p.config.request.success_path.is_none());
    // WHEN: response has title but author is null
    let json = serde_json::json!({"title": "Hello", "author": null});
    let fields = p.extract_fields(&json);
    // THEN: title is extracted, author is absent (null → None in extract)
    assert_eq!(fields.get("title").map(String::as_str), Some("Hello"));
    assert!(!fields.contains_key("author"));
}

// ── parse_u64 ─────────────────────────────────────────────────────────────

#[test]
fn parse_u64_handles_integer_strings() {
    assert_eq!(parse_u64("42"), Some(42));
    assert_eq!(parse_u64("0"), Some(0));
    assert_eq!(parse_u64("1000000"), Some(1_000_000));
}

#[test]
fn parse_u64_handles_float_strings() {
    assert_eq!(parse_u64("42.0"), Some(42));
    assert_eq!(parse_u64("8800.0"), Some(8800));
}

#[test]
fn parse_u64_returns_none_for_non_numeric() {
    assert_eq!(parse_u64("n/a"), None);
    assert_eq!(parse_u64(""), None);
}

// ── Reddit provider ────────────────────────────────────────────────────────

fn reddit_provider() -> ApiRuleProvider {
    make_provider(include_str!("defaults/reddit.toml"))
}

#[test]
fn reddit_provider_matches_www_reddit_com_comments() {
    let p = reddit_provider();
    assert!(p.matches("https://www.reddit.com/r/rust/comments/abc123/some_title/"));
    assert!(p.matches("https://www.reddit.com/r/programming/comments/xyz789"));
}

#[test]
fn reddit_provider_matches_reddit_com_without_www() {
    let p = reddit_provider();
    assert!(p.matches("https://reddit.com/r/rust/comments/abc123"));
}

#[test]
fn reddit_provider_matches_old_reddit() {
    let p = reddit_provider();
    assert!(p.matches("https://old.reddit.com/r/rust/comments/abc123"));
    assert!(p.matches("https://OLD.REDDIT.COM/r/rust/COMMENTS/xyz"));
}

#[test]
fn reddit_provider_does_not_match_subreddit_listing() {
    let p = reddit_provider();
    assert!(!p.matches("https://reddit.com/r/rust"));
    assert!(!p.matches("https://reddit.com/r/rust/"));
    assert!(!p.matches("https://reddit.com/user/someone"));
}

#[test]
fn reddit_provider_does_not_match_other_sites() {
    let p = reddit_provider();
    assert!(!p.matches("https://x.com/user/status/123"));
    assert!(!p.matches("https://youtube.com/watch?v=abc"));
}

#[test]
fn reddit_rewrite_appends_json_suffix() {
    let p = reddit_provider();
    let rewritten = p.rewrite_url("https://www.reddit.com/r/rust/comments/abc123/some_title/");
    assert!(
        std::path::Path::new(&rewritten)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json")),
        "expected .json suffix, got: {rewritten}"
    );
    assert!(
        !rewritten.contains('?'),
        "query string should be stripped, got: {rewritten}"
    );
}

#[test]
fn reddit_rewrite_strips_query_string() {
    let p = reddit_provider();
    let rewritten = p.rewrite_url("https://reddit.com/r/rust/comments/abc123?utm_source=share");
    assert!(
        std::path::Path::new(&rewritten)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json")),
        "expected .json suffix, got: {rewritten}"
    );
    assert!(
        !rewritten.contains("utm_source"),
        "utm param should be gone, got: {rewritten}"
    );
}

#[test]
fn reddit_uses_standard_client_config() {
    use crate::site::rules::config::ClientKind;
    let p = reddit_provider();
    assert_eq!(p.config.request.client, ClientKind::Standard);
}

#[test]
fn reddit_extract_fields_from_api_array_response() {
    // GIVEN: a Reddit-style bare array JSON response
    let p = reddit_provider();
    let json = json!([
        {
            "data": {
                "children": [{
                    "data": {
                        "title": "Rust 2024 edition released",
                        "author": "rustacean42",
                        "score": 4200,
                        "num_comments": 350,
                        "selftext": "Big news for the Rust community.",
                        "url": "https://reddit.com/r/rust/comments/abc123",
                        "subreddit": "rust"
                    }
                }]
            }
        },
        {"data": {"children": []}}
    ]);
    // WHEN: extracting fields
    let fields = p.extract_fields(&json);
    // THEN: all key fields are present
    assert_eq!(
        fields.get("title").map(String::as_str),
        Some("Rust 2024 edition released")
    );
    assert_eq!(
        fields.get("author").map(String::as_str),
        Some("rustacean42")
    );
    assert_eq!(fields.get("score").map(String::as_str), Some("4200"));
    assert_eq!(fields.get("comments").map(String::as_str), Some("350"));
    assert_eq!(fields.get("subreddit").map(String::as_str), Some("rust"));
}

#[test]
fn reddit_build_metadata_sets_platform_and_author() {
    let p = reddit_provider();
    let mut fields = std::collections::HashMap::new();
    fields.insert("title".to_string(), "My Post".to_string());
    fields.insert("author".to_string(), "testuser".to_string());
    fields.insert(
        "url".to_string(),
        "https://reddit.com/r/rust/comments/x".to_string(),
    );
    fields.insert("subreddit".to_string(), "rust".to_string());

    let meta = p.build_metadata(&fields, "https://reddit.com/r/rust/comments/x");
    assert_eq!(meta.platform, "Reddit");
    assert_eq!(meta.author.as_deref(), Some("u/testuser"));
    assert_eq!(meta.title.as_deref(), Some("My Post"));
}

#[test]
fn parse_response_json_accepts_bare_array() {
    // GIVEN: Reddit-style bare array JSON body
    let body = r#"[{"data": {"children": []}}, {"data": {"children": []}}]"#;
    // WHEN: parsing
    let result = parse_response_json(body, "https://example.com");
    // THEN: parses as an array value without error
    assert!(result.is_ok());
    assert!(result.unwrap().is_array());
}

#[test]
fn parse_response_json_accepts_object() {
    let body = r#"{"tweet": {"text": "hello"}}"#;
    let result = parse_response_json(body, "https://example.com");
    assert!(result.is_ok());
    assert!(result.unwrap().is_object());
}

#[test]
fn parse_response_json_fails_on_invalid_json() {
    let body = "not json at all %%%";
    let result = parse_response_json(body, "https://example.com");
    assert!(result.is_err());
}

#[test]
fn parse_response_json_fails_on_html_body() {
    // GIVEN: HTML body — what Reddit returns when HTTP/2-without-ALPN is used
    let body = "<!DOCTYPE html><html><body>Just a moment...</body></html>";
    // WHEN: attempting to parse as JSON
    let result = parse_response_json(body, "https://www.reddit.com/r/rust.json");
    // THEN: error returned so the caller can surface it properly
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("failed to parse JSON"));
}

// ── reddit error-envelope and URL-rewrite edge cases ─────────────────────

#[test]
fn reddit_extract_fields_yields_empty_for_not_found_envelope() {
    // GIVEN: Reddit's error JSON (valid JSON but no listing structure).
    // Before error_for_status() was added, HTTP 404 responses were silently
    // treated as "no fields" rather than a hard error, making the diagnostic
    // message misleading ("check json paths" when the real issue is 404).
    let p = reddit_provider();
    let json = serde_json::json!({"message": "Not Found", "error": 404});
    // WHEN: extracting Reddit fields from the error envelope
    let fields = p.extract_fields(&json);
    // THEN: no fields extracted (the [0].data.children paths don't exist here)
    assert!(
        fields.is_empty(),
        "error envelope should yield no fields, got: {fields:?}"
    );
}

#[test]
fn reddit_rewrite_with_trailing_slash_produces_json_url() {
    // GIVEN: URL with trailing slash (common copy-paste from browser)
    let p = reddit_provider();
    let url = "https://www.reddit.com/r/rust/comments/1krtgr2/media_i_made_a_native_music_player_with_rust/";
    // WHEN: rewriting
    let rewritten = p.rewrite_url(url);
    // THEN: trailing slash consumed by regex, .json appended to slug
    assert_eq!(
        rewritten,
        "https://www.reddit.com/r/rust/comments/1krtgr2/media_i_made_a_native_music_player_with_rust.json"
    );
}

#[test]
fn reddit_rewrite_without_title_slug_produces_json_url() {
    // GIVEN: short URL with post ID but no title slug
    let p = reddit_provider();
    let url = "https://www.reddit.com/r/rust/comments/1krtgr2/";
    // WHEN: rewriting
    let rewritten = p.rewrite_url(url);
    // THEN: .json appended to post ID
    assert_eq!(
        rewritten,
        "https://www.reddit.com/r/rust/comments/1krtgr2.json"
    );
}

// ── json_path_is_non_null ─────────────────────────────────────────────────

#[test]
fn json_path_is_non_null_returns_true_for_existing_string() {
    // GIVEN: FxTwitter-style success response with a non-null tweet object
    let json = json!({"tweet": {"text": "hello", "likes": 42}});
    // WHEN/THEN: paths to real values return true
    assert!(json_path_is_non_null(&json, ".tweet.text"));
    assert!(json_path_is_non_null(&json, ".tweet.likes"));
}

#[test]
fn json_path_is_non_null_returns_false_for_null_value() {
    // GIVEN: FxTwitter-style error where tweet is null (tweet not found)
    let json = json!({"tweet": null, "code": 144});
    // WHEN/THEN: path to null returns false
    assert!(!json_path_is_non_null(&json, ".tweet"));
}

#[test]
fn json_path_is_non_null_returns_false_for_missing_path() {
    // GIVEN: JSON without the expected success field (Reddit 404 envelope)
    let json = json!({"message": "Not Found", "error": 404});
    // WHEN/THEN: paths that don't exist return false
    assert!(!json_path_is_non_null(&json, ".tweet"));
    assert!(!json_path_is_non_null(
        &json,
        "[0].data.children[0].data.title"
    ));
}

#[test]
fn json_path_is_non_null_returns_true_for_number_zero() {
    // GIVEN: JSON with a zero value — falsy in some langs, but not null in Rust
    let json = json!({"count": 0});
    // WHEN/THEN: zero is non-null
    assert!(json_path_is_non_null(&json, ".count"));
}

// ── stackoverflow provider (multi-fetch config) ───────────────────────────

fn stackoverflow_provider() -> ApiRuleProvider {
    make_provider(include_str!("defaults/stackoverflow.toml"))
}

#[test]
fn stackoverflow_provider_matches_question_urls() {
    let p = stackoverflow_provider();
    assert!(p.matches("https://stackoverflow.com/questions/12345/some-title"));
    assert!(p.matches("https://STACKOVERFLOW.COM/questions/99999/title"));
    assert!(p.matches("https://stackoverflow.com/questions/42/x?noredirect=1"));
}

#[test]
fn stackoverflow_provider_does_not_match_non_question_urls() {
    let p = stackoverflow_provider();
    assert!(!p.matches("https://stackoverflow.com/"));
    assert!(!p.matches("https://stackoverflow.com/tags/rust"));
    assert!(!p.matches("https://stackoverflow.com/questions/tagged/rust"));
    assert!(!p.matches("https://youtube.com/watch?v=abc"));
}

#[test]
fn stackoverflow_provider_rewrite_constructs_question_api_url() {
    let p = stackoverflow_provider();
    let url = "https://stackoverflow.com/questions/26946646/how-to-do-x";
    let rewritten = p.rewrite_url(url);
    assert!(rewritten.contains("api.stackexchange.com"));
    assert!(rewritten.contains("26946646"));
    assert!(rewritten.contains("site=stackoverflow"));
    assert!(rewritten.contains("filter=withbody"));
}

#[test]
fn stackoverflow_provider_has_one_additional_fetch() {
    let p = stackoverflow_provider();
    assert_eq!(p.config.additional_fetches.len(), 1);
    assert_eq!(p.additional_rewrite_froms.len(), 1);
}

#[test]
fn stackoverflow_additional_fetch_rewrite_constructs_answers_api_url() {
    let p = stackoverflow_provider();
    let url = "https://stackoverflow.com/questions/26946646/how-to-do-x";
    let af = &p.config.additional_fetches[0];
    let re = &p.additional_rewrite_froms[0];
    let api_url = re.replace(url, af.rewrite_to.as_str()).into_owned();
    assert!(api_url.contains("api.stackexchange.com"));
    assert!(api_url.contains("26946646"));
    assert!(api_url.contains("/answers"));
    assert!(api_url.contains("site=stackoverflow"));
}

#[test]
fn stackoverflow_extract_fields_from_question_json() {
    let p = stackoverflow_provider();
    let json = json!({
        "items": [{
            "title": "How to use Vec in Rust?",
            "body": "<p>I want a vector.</p>",
            "score": 42,
            "answer_count": 3,
            "view_count": 15000,
            "link": "https://stackoverflow.com/questions/12345",
            "creation_date": 1_700_000_000u64,
            "tags": ["rust", "vector"],
            "owner": {"display_name": "rustacean"}
        }]
    });
    let fields = p.extract_fields(&json);
    assert_eq!(
        fields.get("title").map(String::as_str),
        Some("How to use Vec in Rust?")
    );
    assert_eq!(fields.get("score").map(String::as_str), Some("42"));
    assert_eq!(fields.get("answer_count").map(String::as_str), Some("3"));
}

#[test]
fn stackoverflow_additional_fetch_prefix_applied() {
    // Verify that additional fetch fields are prefixed correctly
    // by exercising the apply_additional_fetches logic structurally.
    // We test prefix naming: if the additional fetch config has prefix "ans"
    // and field name "body", the merged key must be "ans_body".
    let p = stackoverflow_provider();
    let af = &p.config.additional_fetches[0];
    assert_eq!(af.prefix, "ans");
    // Confirm the json config has the expected fields
    assert!(af.json.0.contains_key("body"));
    assert!(af.json.0.contains_key("score"));
    assert!(af.json.0.contains_key("is_accepted"));
    assert!(af.json.0.contains_key("author"));
}

// ── auth config integration ───────────────────────────────────────────────

fn provider_with_auth(auth: &str) -> ApiRuleProvider {
    let toml = format!(
        r#"
[site]
name = "test-auth"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
auth = "{auth}"

[json]
title = ".title"

[template]
format = "{{{{title}}}}"
"#
    );
    make_provider(&toml)
}

#[test]
fn provider_with_auth_config_parses_successfully() {
    // GIVEN: a rule with auth = "env:SOME_TOKEN"
    // WHEN: provider is built
    let p = provider_with_auth("env:SOME_TOKEN");
    // THEN: auth field is stored in config
    assert_eq!(p.config.request.auth.as_deref(), Some("env:SOME_TOKEN"));
}

#[test]
fn provider_with_auth_stores_env_var_name() {
    // GIVEN: auth pointing to a custom env var
    let p = provider_with_auth("env:GITHUB_TOKEN");
    // WHEN: the stored auth string is parsed
    let auth_cfg = AuthConfig::parse(p.config.request.auth.as_deref().unwrap()).unwrap();
    // THEN: correct env var name is stored
    assert_eq!(auth_cfg.env_var, "GITHUB_TOKEN");
    assert!(auth_cfg.bearer);
    assert_eq!(auth_cfg.header_name, "Authorization");
}

#[test]
fn provider_with_custom_header_auth_stores_header_name() {
    // GIVEN: auth with custom header
    let p = provider_with_auth("env:MY_KEY:header=X-Custom-Auth");
    let auth_cfg = AuthConfig::parse(p.config.request.auth.as_deref().unwrap()).unwrap();
    assert_eq!(auth_cfg.header_name, "X-Custom-Auth");
    assert!(!auth_cfg.bearer);
}

#[test]
fn github_issues_provider_parses_and_has_auth() {
    // GIVEN: the embedded github-issues TOML rule
    let p = make_provider(include_str!("defaults/github-issues.toml"));
    // THEN: provider name is correct, auth is set
    assert_eq!(p.config.site.name, "github-issues");
    assert_eq!(p.config.request.auth.as_deref(), Some("env:GITHUB_TOKEN"));
}

#[test]
fn github_issues_provider_matches_issue_and_pr_urls() {
    let p = make_provider(include_str!("defaults/github-issues.toml"));
    assert!(p.matches("https://github.com/rust-lang/rust/issues/12345"));
    assert!(p.matches("https://github.com/owner/repo/pull/999"));
    assert!(p.matches("https://GITHUB.COM/owner/repo/issues/1"));
}

#[test]
fn github_issues_provider_does_not_match_repo_root() {
    let p = make_provider(include_str!("defaults/github-issues.toml"));
    assert!(!p.matches("https://github.com/rust-lang/rust"));
    assert!(!p.matches("https://github.com/owner/repo/tree/main"));
}

#[test]
fn github_issues_rewrite_constructs_api_url() {
    let p = make_provider(include_str!("defaults/github-issues.toml"));
    let rewritten = p.rewrite_url("https://github.com/rust-lang/rust/issues/12345");
    assert_eq!(
        rewritten,
        "https://api.github.com/repos/rust-lang/rust/issues/12345"
    );
}

#[test]
fn github_issues_rewrite_works_for_pull_requests() {
    let p = make_provider(include_str!("defaults/github-issues.toml"));
    // GitHub API exposes PRs under /issues/ endpoint
    let rewritten = p.rewrite_url("https://github.com/owner/repo/pull/42");
    assert_eq!(
        rewritten,
        "https://api.github.com/repos/owner/repo/issues/42"
    );
}

#[test]
fn github_issues_extract_fields_from_api_json() {
    // GIVEN: a GitHub issue API response
    let p = make_provider(include_str!("defaults/github-issues.toml"));
    let json = json!({
        "html_url": "https://github.com/rust-lang/rust/issues/12345",
        "title": "Some bug",
        "state": "open",
        "user": {"login": "contributor"},
        "body": "This is the issue body.",
        "comments": 5,
        "created_at": "2025-01-01T00:00:00Z",
        "labels": [{"name": "bug"}, {"name": "help wanted"}]
    });
    // WHEN: extracting fields
    let fields = p.extract_fields(&json);
    // THEN: key fields are present
    assert_eq!(fields.get("title").map(String::as_str), Some("Some bug"));
    assert_eq!(
        fields.get("author").map(String::as_str),
        Some("contributor")
    );
    assert_eq!(fields.get("state").map(String::as_str), Some("open"));
    assert_eq!(fields.get("comments").map(String::as_str), Some("5"));
}

// ── parse_css_attr_suffix ────────────────────────────────────────────────

#[test]
fn parse_css_attr_suffix_detects_attr() {
    // GIVEN: selector with ::attr(content) suffix
    let (css, attr) = parse_css_attr_suffix("meta[property='og:title']::attr(content)");
    assert_eq!(css, "meta[property='og:title']");
    assert_eq!(attr, Some("content"));
}

#[test]
fn parse_css_attr_suffix_no_suffix_returns_none() {
    // GIVEN: plain selector without ::attr
    let (css, attr) = parse_css_attr_suffix("h1.title");
    assert_eq!(css, "h1.title");
    assert!(attr.is_none());
}

#[test]
fn parse_css_attr_suffix_handles_href_attribute() {
    let (css, attr) = parse_css_attr_suffix("a.link::attr(href)");
    assert_eq!(css, "a.link");
    assert_eq!(attr, Some("href"));
}

#[test]
fn parse_css_attr_suffix_handles_malformed_no_closing_paren() {
    // GIVEN: ::attr( without closing )
    let (css, attr) = parse_css_attr_suffix("meta::attr(content");
    // THEN: treated as no attr suffix
    assert_eq!(css, "meta::attr(content");
    assert!(attr.is_none());
}

// ── extract_css_fields ───────────────────────────────────────────────────

#[test]
fn extract_css_fields_attribute_extraction() {
    // GIVEN: HTML with og:meta tags and a CSS map using ::attr(content)
    let html = r#"<html><head>
            <meta property="og:title" content="Test Title" />
            <meta property="og:description" content="A description" />
            <meta property="og:image" content="https://example.com/img.jpg" />
        </head><body></body></html>"#;
    let mut css_map = HashMap::new();
    css_map.insert(
        "title".to_string(),
        "meta[property='og:title']::attr(content)".to_string(),
    );
    css_map.insert(
        "description".to_string(),
        "meta[property='og:description']::attr(content)".to_string(),
    );
    css_map.insert(
        "image".to_string(),
        "meta[property='og:image']::attr(content)".to_string(),
    );
    // WHEN: extracting
    let fields = extract_css_fields(html, &css_map);
    // THEN: all three fields present
    assert_eq!(fields.get("title").map(String::as_str), Some("Test Title"));
    assert_eq!(
        fields.get("description").map(String::as_str),
        Some("A description")
    );
    assert_eq!(
        fields.get("image").map(String::as_str),
        Some("https://example.com/img.jpg")
    );
}

#[test]
fn extract_css_fields_text_content_extraction() {
    // GIVEN: HTML with an h1 and CSS map using text content (no ::attr)
    let html = "<html><body><h1>Page Heading</h1></body></html>";
    let mut css_map = HashMap::new();
    css_map.insert("title".to_string(), "h1".to_string());
    // WHEN: extracting
    let fields = extract_css_fields(html, &css_map);
    // THEN: text content of h1 is used
    assert_eq!(
        fields.get("title").map(String::as_str),
        Some("Page Heading")
    );
}

#[test]
fn extract_css_fields_missing_element_omitted() {
    // GIVEN: HTML without the targeted element
    let html = "<html><body><p>No heading here</p></body></html>";
    let mut css_map = HashMap::new();
    css_map.insert("title".to_string(), "h1".to_string());
    // WHEN: extracting
    let fields = extract_css_fields(html, &css_map);
    // THEN: field absent (no element found)
    assert!(!fields.contains_key("title"));
}

#[test]
fn extract_css_fields_empty_attr_value_omitted() {
    // GIVEN: meta tag with empty content attribute
    let html = r#"<html><head><meta property="og:title" content="" /></head></html>"#;
    let mut css_map = HashMap::new();
    css_map.insert(
        "title".to_string(),
        "meta[property='og:title']::attr(content)".to_string(),
    );
    // WHEN: extracting
    let fields = extract_css_fields(html, &css_map);
    // THEN: empty attribute value is omitted
    assert!(!fields.contains_key("title"));
}

#[test]
fn extract_css_fields_invalid_selector_logs_and_skips() {
    // GIVEN: an invalid CSS selector
    let html = "<html><body></body></html>";
    let mut css_map = HashMap::new();
    css_map.insert("title".to_string(), "[[[invalid".to_string());
    // WHEN: extracting
    let fields = extract_css_fields(html, &css_map);
    // THEN: field absent, no panic
    assert!(fields.is_empty());
}

// ── rewrite_url_with ─────────────────────────────────────────────────────

#[test]
fn rewrite_url_with_url_placeholder() {
    // GIVEN: template with {url}
    let re = regex::Regex::new(".*").unwrap();
    let result = rewrite_url_with(
        &re,
        "https://api.example.com?url={url}",
        "https://orig.com/page",
    );
    assert!(result.contains("https%3A%2F%2Forig.com%2Fpage"));
}

#[test]
fn rewrite_url_with_capture_group() {
    // GIVEN: template with capture group $1
    let re = regex::Regex::new(r"https://example\.com/items/(\d+)").unwrap();
    let result = rewrite_url_with(
        &re,
        "https://api.example.com/items/$1",
        "https://example.com/items/42",
    );
    assert_eq!(result, "https://api.example.com/items/42");
}

#[test]
fn rewrite_url_with_identity_passthrough() {
    // GIVEN: {url} template that passes original URL through (url-encoded)
    let re = regex::Regex::new(".*").unwrap();
    let result = rewrite_url_with(&re, "{url}", "https://example.com/page");
    assert_eq!(result, "https%3A%2F%2Fexample.com%2Fpage");
}

// ── instagram provider ───────────────────────────────────────────────────

fn instagram_provider() -> ApiRuleProvider {
    make_provider(include_str!("defaults/instagram.toml"))
}

#[test]
fn instagram_provider_matches_post_urls() {
    let p = instagram_provider();
    assert!(p.matches("https://instagram.com/p/ABC123xyz"));
    assert!(p.matches("https://www.instagram.com/p/XYZ789abc"));
    assert!(p.matches("https://INSTAGRAM.COM/p/test123"));
}

#[test]
fn instagram_provider_matches_reel_urls() {
    let p = instagram_provider();
    assert!(p.matches("https://instagram.com/reel/ABC123xyz"));
    assert!(p.matches("https://www.instagram.com/reel/XYZ789"));
}

#[test]
fn instagram_provider_does_not_match_profile_urls() {
    let p = instagram_provider();
    assert!(!p.matches("https://instagram.com/username"));
    assert!(!p.matches("https://instagram.com/"));
    assert!(!p.matches("https://youtube.com/watch?v=abc"));
}

#[test]
fn instagram_provider_has_one_html_fallback() {
    let p = instagram_provider();
    assert_eq!(p.config.fallback.len(), 1);
    assert_eq!(p.fallback_rewrite_froms.len(), 1);
    assert_eq!(p.config.fallback[0].fallback_type, FallbackType::Html);
}

#[test]
fn instagram_provider_fallback_css_has_og_selectors() {
    let p = instagram_provider();
    let css = &p.config.fallback[0].css;
    assert!(css.contains_key("title"));
    assert!(css.contains_key("description"));
    assert!(css.contains_key("image"));
    assert!(css["title"].contains("og:title"));
    assert!(css["image"].contains("og:image"));
}

// ── Twitter template: views is optional ───────────────────────────────────

#[test]
fn twitter_template_renders_engagement_line_without_views() {
    // GIVEN: FxTwitter response where views is null — a common case for older
    // tweets and accounts that haven't enabled view counts.
    let p = twitter_provider();
    let mut fields = HashMap::new();
    fields.insert("author_handle".to_string(), "jack".to_string());
    fields.insert("author_name".to_string(), "jack".to_string());
    fields.insert("text".to_string(), "just setting up my twttr".to_string());
    fields.insert("likes".to_string(), "290120".to_string());
    fields.insert("retweets".to_string(), "123262".to_string());
    fields.insert("replies".to_string(), "16455".to_string());
    // No "views" field — null in JSON → absent from map.
    fields.insert(
        "date".to_string(),
        "Tue Mar 21 20:50:14 +0000 2006".to_string(),
    );
    fields.insert(
        "url".to_string(),
        "https://x.com/jack/status/20".to_string(),
    );

    let markdown = template::render(
        &p.config.template.format,
        &fields,
        "https://x.com/jack/status/20",
    );

    // THEN: the engagement line is present (likes/retweets/replies)
    assert!(
        markdown.contains("290.1K likes"),
        "engagement line must render even without views; got:\n{markdown}"
    );
    assert!(
        markdown.contains("123.3K reposts"),
        "retweets must render; got:\n{markdown}"
    );
    // AND: the views line is silently omitted (not an error)
    assert!(
        !markdown.contains("views"),
        "views line must be omitted when views field is absent; got:\n{markdown}"
    );
    // AND: the rest of the template renders correctly
    assert!(markdown.contains("## @jack (jack)"));
    assert!(markdown.contains("just setting up my twttr"));
    assert!(markdown.contains("[View on X](https://x.com/jack/status/20)"));
}

#[test]
fn twitter_template_renders_views_line_when_views_present() {
    // GIVEN: a tweet with views populated (X Premium / newer tweets)
    let p = twitter_provider();
    let mut fields = HashMap::new();
    fields.insert("author_handle".to_string(), "rustlang".to_string());
    fields.insert(
        "author_name".to_string(),
        "The Rust Programming Language".to_string(),
    );
    fields.insert(
        "text".to_string(),
        "Rust 2024 edition is stable!".to_string(),
    );
    fields.insert("likes".to_string(), "8800".to_string());
    fields.insert("retweets".to_string(), "1200".to_string());
    fields.insert("replies".to_string(), "344".to_string());
    fields.insert("views".to_string(), "3800000".to_string());
    fields.insert(
        "date".to_string(),
        "Mon Nov 28 00:00:00 +0000 2024".to_string(),
    );
    fields.insert(
        "url".to_string(),
        "https://x.com/rustlang/status/1861000000000000000".to_string(),
    );

    let markdown = template::render(
        &p.config.template.format,
        &fields,
        "https://x.com/rustlang/status/1861000000000000000",
    );

    // THEN: views line is present
    assert!(
        markdown.contains("3.8M views"),
        "views line must render when views field is present; got:\n{markdown}"
    );
    // AND: engagement line also present
    assert!(markdown.contains("8.8K likes"));
}

// ── extract_items_array ─────────────────────────────────────────────────

#[test]
fn extract_items_array_returns_all_elements_for_root_array() {
    // GIVEN: a JSON root array with two objects
    let json = json!([{"title": "A"}, {"title": "B"}]);
    // WHEN: extracting with path "."
    let items = extract_items_array(&json, ".").unwrap();
    // THEN: both items are returned
    assert_eq!(items.len(), 2);
}

#[test]
fn extract_items_array_navigates_nested_dot_path() {
    // GIVEN: JSON with nested structure .data.results containing one item
    let json = json!({"data": {"results": [{"x": 1}]}});
    // WHEN: extracting with path ".data.results"
    let items = extract_items_array(&json, ".data.results").unwrap();
    // THEN: the single item is returned
    assert_eq!(items.len(), 1);
}

#[test]
fn extract_items_array_errors_on_missing_path() {
    // GIVEN: JSON that does not contain the requested path
    let json = json!({"other": 1});
    // WHEN: extracting with a missing path segment
    let err = extract_items_array(&json, ".items").unwrap_err();
    // THEN: error message references the missing segment
    assert!(
        err.to_string().contains("items"),
        "expected 'items' in error, got: {err}"
    );
}

#[test]
fn extract_items_array_errors_on_non_array_value() {
    // GIVEN: JSON where the target path resolves to a string, not an array
    let json = json!({"items": "not_array"});
    // WHEN: extracting with that path
    let err = extract_items_array(&json, ".items").unwrap_err();
    // THEN: error message references the path that did not resolve to an array
    assert!(
        err.to_string().contains(".items"),
        "expected path in error, got: {err}"
    );
}
