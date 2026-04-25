// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Tests for `LinkedIn` extraction — URL classification, helpers, and parsing.

use super::LinkedInProvider;
use super::helpers::{
    extract_csrf_token, extract_username_from_url, strip_html, strip_html_comment,
};
use super::oembed::format_oembed_markdown;
use super::types::LinkedInOEmbed;
use super::url::{LinkedInUrlKind, classify_linkedin_url};
use crate::site::SiteProvider;

// ── URL Classification ──────────────────────────────────────────────────────

#[test]
fn classifies_profile_urls() {
    assert_eq!(
        classify_linkedin_url("https://www.linkedin.com/in/mikko-parkkola/"),
        Some(LinkedInUrlKind::Profile)
    );
    assert_eq!(
        classify_linkedin_url("https://linkedin.com/in/someuser"),
        Some(LinkedInUrlKind::Profile)
    );
}

#[test]
fn classifies_company_urls() {
    assert_eq!(
        classify_linkedin_url("https://www.linkedin.com/company/anthropic/"),
        Some(LinkedInUrlKind::Company)
    );
}

#[test]
fn classifies_post_urls() {
    assert_eq!(
        classify_linkedin_url("https://www.linkedin.com/posts/someuser_topic-activity-123456789"),
        Some(LinkedInUrlKind::Post)
    );
}

#[test]
fn classifies_pulse_urls() {
    assert_eq!(
        classify_linkedin_url("https://www.linkedin.com/pulse/some-article-title-author"),
        Some(LinkedInUrlKind::Pulse)
    );
}

#[test]
fn classifies_feed_update_urls() {
    assert_eq!(
        classify_linkedin_url(
            "https://www.linkedin.com/feed/update/urn:li:activity:7654321098765432109"
        ),
        Some(LinkedInUrlKind::FeedUpdate)
    );
}

#[test]
fn classifies_activity_urls() {
    assert_eq!(
        classify_linkedin_url("https://www.linkedin.com/in/mikko-parkkola/recent-activity/all/"),
        Some(LinkedInUrlKind::Activity)
    );
}

#[test]
fn handles_query_params() {
    assert_eq!(
        classify_linkedin_url("https://www.linkedin.com/in/user?utm_source=share"),
        Some(LinkedInUrlKind::Profile)
    );
}

#[test]
fn rejects_non_linkedin_urls() {
    assert_eq!(
        classify_linkedin_url("https://youtube.com/watch?v=abc"),
        None
    );
    assert_eq!(
        classify_linkedin_url("https://twitter.com/user/status/123"),
        None
    );
}

#[test]
fn rejects_bare_linkedin() {
    assert_eq!(classify_linkedin_url("https://www.linkedin.com/"), None);
    // /in/ without a username segment
    assert_eq!(classify_linkedin_url("https://www.linkedin.com/in/"), None);
}

// ── URL Matching ────────────────────────────────────────────────────────────

#[test]
fn matches_all_linkedin_url_kinds() {
    let provider = LinkedInProvider;
    assert!(provider.matches("https://www.linkedin.com/in/someuser"));
    assert!(provider.matches("https://www.linkedin.com/company/somecompany"));
    assert!(provider.matches("https://www.linkedin.com/posts/user_title-123"));
    assert!(provider.matches("https://www.linkedin.com/pulse/article-title"));
    assert!(provider.matches("https://www.linkedin.com/feed/update/urn:li:activity:123"));
    assert!(provider.matches("https://www.linkedin.com/in/user/recent-activity/all/"));
}

#[test]
fn does_not_match_non_linkedin() {
    let provider = LinkedInProvider;
    assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    assert!(!provider.matches("https://twitter.com/user/status/123"));
}

// ── Kind Properties ─────────────────────────────────────────────────────────

#[test]
fn auth_required_kinds() {
    assert!(LinkedInUrlKind::Profile.requires_auth());
    assert!(LinkedInUrlKind::Company.requires_auth());
    assert!(LinkedInUrlKind::Activity.requires_auth());
    assert!(!LinkedInUrlKind::Post.requires_auth());
    assert!(!LinkedInUrlKind::Pulse.requires_auth());
}

#[test]
fn oembed_fallback_kinds() {
    assert!(LinkedInUrlKind::Post.has_oembed_fallback());
    assert!(LinkedInUrlKind::Pulse.has_oembed_fallback());
    assert!(LinkedInUrlKind::FeedUpdate.has_oembed_fallback());
    assert!(!LinkedInUrlKind::Profile.has_oembed_fallback());
}

// ── HTML Stripping ──────────────────────────────────────────────────────────

#[test]
fn strip_html_removes_tags() {
    assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
}

#[test]
fn strip_html_decodes_entities() {
    assert_eq!(strip_html("&amp; &lt; &gt;"), "& < >");
}

// ── oEmbed Formatting ───────────────────────────────────────────────────────

#[test]
fn format_oembed_with_full_data() {
    let oembed = LinkedInOEmbed {
        title: Some("The Future of Rust".to_string()),
        author_name: Some("Jane Engineer".to_string()),
        author_url: Some("https://www.linkedin.com/in/janeengineer".to_string()),
        thumbnail_url: Some("https://media.linkedin.com/thumb.jpg".to_string()),
        html: Some("<p>Great insights on systems programming.</p>".to_string()),
    };

    let url = "https://www.linkedin.com/posts/janeengineer_rust-123";
    let md = format_oembed_markdown(&oembed, url);

    assert!(md.contains("## The Future of Rust"));
    assert!(md.contains("by Jane Engineer"));
    assert!(md.contains("![LinkedIn post](https://media.linkedin.com/thumb.jpg)"));
    assert!(md.contains("Great insights on systems programming."));
    assert!(md.contains("[View on LinkedIn]"));
}

#[test]
fn format_oembed_with_minimal_data() {
    let oembed = LinkedInOEmbed {
        title: None,
        author_name: Some("John Doe".to_string()),
        author_url: None,
        thumbnail_url: None,
        html: None,
    };

    let url = "https://www.linkedin.com/posts/johndoe_post-456";
    let md = format_oembed_markdown(&oembed, url);

    assert!(!md.contains("##"));
    assert!(md.contains("by John Doe"));
    assert!(!md.contains("!["));
    assert!(md.contains("[View on LinkedIn]"));
}

// ── Voyager helpers ─────────────────────────────────────────────────────────

#[test]
fn extract_csrf_token_with_quotes() {
    // GIVEN: JSESSIONID stored with surrounding quotes (standard LinkedIn format)
    let cookies = r#"li_at=AQEDARabcd; JSESSIONID="ajax:1234567890""#;
    // WHEN
    let token = extract_csrf_token(cookies);
    // THEN: quotes stripped
    assert_eq!(token, Some("ajax:1234567890".to_string()));
}

#[test]
fn extract_csrf_token_without_quotes() {
    // GIVEN: JSESSIONID without surrounding quotes (some client formats)
    let cookies = "li_at=AQEDARabcd; JSESSIONID=ajax:9876543210";
    // WHEN
    let token = extract_csrf_token(cookies);
    // THEN: value returned as-is
    assert_eq!(token, Some("ajax:9876543210".to_string()));
}

#[test]
fn extract_csrf_token_missing_jsessionid() {
    // GIVEN: cookies without JSESSIONID
    let cookies = "li_at=AQEDARabcd; lang=en";
    // WHEN / THEN: None returned
    assert_eq!(extract_csrf_token(cookies), None);
}

#[test]
fn extract_csrf_token_case_insensitive_key() {
    // GIVEN: key casing varies
    let cookies = r#"Jsessionid="ajax:5555""#;
    assert_eq!(extract_csrf_token(cookies), Some("ajax:5555".to_string()));
}

#[test]
fn extract_username_simple_profile_url() {
    // GIVEN: canonical profile URL
    let url = "https://www.linkedin.com/in/mikko-parkkola/";
    // WHEN
    let username = extract_username_from_url(url);
    // THEN
    assert_eq!(username, Some("mikko-parkkola".to_string()));
}

#[test]
fn extract_username_strips_query_string() {
    // GIVEN: URL with query params
    let url = "https://www.linkedin.com/in/someuser?utm_source=share";
    assert_eq!(extract_username_from_url(url), Some("someuser".to_string()));
}

#[test]
fn extract_username_activity_subpath() {
    // GIVEN: activity subpath after username
    let url = "https://www.linkedin.com/in/johndoe/recent-activity/all/";
    assert_eq!(extract_username_from_url(url), Some("johndoe".to_string()));
}

#[test]
fn extract_username_non_profile_url() {
    // GIVEN: company URL (no /in/ segment)
    let url = "https://www.linkedin.com/company/anthropic/";
    assert_eq!(extract_username_from_url(url), None);
}

// ── Voyager parsers (impersonate feature) ────────────────────────────────────

#[cfg(feature = "impersonate")]
#[test]
fn parse_voyager_profile_full_response() {
    use super::helpers::parse_voyager_profile;
    use super::types::VoyagerProfileResponse;

    // GIVEN: full Voyager profile JSON
    let json = r#"{
        "firstName": "Jane",
        "lastName": "Engineer",
        "headline": "Staff Engineer at Acme Corp",
        "summary": "Passionate about distributed systems and Rust.",
        "industryName": "Computer Software",
        "geoLocationName": "San Francisco, California"
    }"#;
    let profile: VoyagerProfileResponse = serde_json::from_str(json).unwrap();

    // WHEN
    let md = parse_voyager_profile(&profile);

    // THEN
    assert!(md.contains("## Jane Engineer"));
    assert!(md.contains("Staff Engineer at Acme Corp"));
    assert!(md.contains("Location: San Francisco, California"));
    assert!(md.contains("Industry: Computer Software"));
    assert!(md.contains("### About"));
    assert!(md.contains("Passionate about distributed systems and Rust."));
}

#[cfg(feature = "impersonate")]
#[test]
fn parse_voyager_profile_minimal_response() {
    use super::helpers::parse_voyager_profile;
    use super::types::VoyagerProfileResponse;

    // GIVEN: partial response — only first name available
    let json = r#"{"firstName": "Jane"}"#;
    let profile: VoyagerProfileResponse = serde_json::from_str(json).unwrap();

    // WHEN
    let md = parse_voyager_profile(&profile);

    // THEN: no panic, renders what's available
    assert!(md.contains("## Jane"));
    assert!(!md.contains("Industry:"));
    assert!(!md.contains("### About"));
}

#[cfg(feature = "impersonate")]
#[test]
fn parse_voyager_profile_empty_summary_omitted() {
    use super::helpers::parse_voyager_profile;
    use super::types::VoyagerProfileResponse;

    // GIVEN: summary present but blank
    let json = r#"{"firstName": "Bob", "summary": "   "}"#;
    let profile: VoyagerProfileResponse = serde_json::from_str(json).unwrap();

    // WHEN
    let md = parse_voyager_profile(&profile);

    // THEN: About section not emitted for blank summary
    assert!(!md.contains("### About"));
}

#[cfg(feature = "impersonate")]
#[test]
fn parse_voyager_activity_with_posts() {
    use super::helpers::parse_voyager_activity;
    use super::types::VoyagerActivityResponse;

    // GIVEN: activity feed with two posts
    let json = r#"{
        "elements": [
            {
                "value": {
                    "commentary": {
                        "text": { "text": "First post content here." }
                    }
                }
            },
            {
                "value": {
                    "commentary": {
                        "text": { "text": "Second post content here." }
                    }
                }
            }
        ]
    }"#;
    let activity: VoyagerActivityResponse = serde_json::from_str(json).unwrap();

    // WHEN
    let md = parse_voyager_activity(&activity);

    // THEN
    assert!(md.contains("First post content here."));
    assert!(md.contains("Second post content here."));
    assert_eq!(md.matches("---").count(), 2);
}

#[cfg(feature = "impersonate")]
#[test]
fn parse_voyager_activity_skips_elements_without_commentary() {
    use super::helpers::parse_voyager_activity;
    use super::types::VoyagerActivityResponse;

    // GIVEN: mix of posts with and without commentary (e.g., share-only items)
    let json = r#"{
        "elements": [
            { "value": null },
            {
                "value": {
                    "commentary": {
                        "text": { "text": "Real post text." }
                    }
                }
            }
        ]
    }"#;
    let activity: VoyagerActivityResponse = serde_json::from_str(json).unwrap();

    // WHEN
    let md = parse_voyager_activity(&activity);

    // THEN: only the real post is rendered
    assert!(md.contains("Real post text."));
    assert_eq!(md.matches("---").count(), 1);
}

#[cfg(feature = "impersonate")]
#[test]
fn parse_voyager_activity_empty_feed() {
    use super::helpers::parse_voyager_activity;
    use super::types::VoyagerActivityResponse;

    // GIVEN: empty elements array
    let json = r#"{"elements": []}"#;
    let activity: VoyagerActivityResponse = serde_json::from_str(json).unwrap();

    // WHEN
    let md = parse_voyager_activity(&activity);

    // THEN: empty string, no separator
    assert!(md.trim().is_empty());
}

// ── HTML Parser (impersonate feature) ───────────────────────────────────────

#[cfg(feature = "impersonate")]
#[test]
fn parses_json_ld_profile() {
    use super::auth::parse_linkedin_html;

    let html = r#"
    <html>
    <head>
        <script type="application/ld+json">
        {
            "@type": "Person",
            "name": "Mikko Parkkola",
            "description": "Building things with Rust and AI",
            "image": "https://media.linkedin.com/photo.jpg"
        }
        </script>
    </head>
    <body></body>
    </html>
    "#;

    let content = parse_linkedin_html(
        html,
        "https://linkedin.com/in/mikko",
        LinkedInUrlKind::Profile,
    )
    .unwrap();
    assert!(content.markdown.contains("## Mikko Parkkola"));
    assert!(
        content
            .markdown
            .contains("Building things with Rust and AI")
    );
    assert_eq!(content.metadata.platform, "LinkedIn (Profile)");
}

#[cfg(feature = "impersonate")]
#[test]
fn falls_back_to_selectors() {
    use super::auth::parse_linkedin_html;

    let html = r#"
    <html>
    <head>
        <title>Mikko Parkkola | LinkedIn</title>
        <meta property="og:description" content="Rust developer and AI enthusiast">
        <meta property="og:image" content="https://media.linkedin.com/photo.jpg">
    </head>
    <body>
        <h1>Mikko Parkkola</h1>
        <div class="text-body-medium">Senior Engineer at Some Company</div>
    </body>
    </html>
    "#;

    let content = parse_linkedin_html(
        html,
        "https://linkedin.com/in/mikko",
        LinkedInUrlKind::Profile,
    )
    .unwrap();
    assert!(content.markdown.contains("## Mikko Parkkola"));
    assert!(content.markdown.contains("Senior Engineer at Some Company"));
}

#[cfg(feature = "impersonate")]
#[test]
fn og_description_fallback() {
    use super::auth::parse_linkedin_html;

    let html = r#"
    <html>
    <head>
        <meta property="og:description" content="This is the only content available">
    </head>
    <body></body>
    </html>
    "#;

    let content = parse_linkedin_html(
        html,
        "https://linkedin.com/in/user",
        LinkedInUrlKind::Profile,
    )
    .unwrap();
    assert!(
        content
            .markdown
            .contains("This is the only content available")
    );
}

// ── strip_html_comment ──────────────────────────────────────────────────────

#[test]
fn strip_comment_removes_html_comment_wrapper() {
    // GIVEN: string wrapped in HTML comment (LinkedIn's <code> element format)
    let input = r#"<!--{"firstName":"Jane"}-->"#;
    // WHEN
    let result = strip_html_comment(input);
    // THEN: JSON is exposed without wrappers
    assert_eq!(result, r#"{"firstName":"Jane"}"#);
}

#[test]
fn strip_comment_trims_whitespace_inside_comment() {
    // GIVEN: comment wrapper with surrounding whitespace
    let input = "<!--  {\"key\":\"value\"}  -->";
    let result = strip_html_comment(input);
    assert_eq!(result, r#"{"key":"value"}"#);
}

#[test]
fn strip_comment_passthrough_when_no_comment_wrapper() {
    // GIVEN: plain JSON (no comment wrapper)
    let input = r#"{"firstName":"Jane"}"#;
    let result = strip_html_comment(input);
    assert_eq!(result, input);
}

#[test]
fn strip_comment_passthrough_empty_string() {
    assert_eq!(strip_html_comment(""), "");
}

// ── extract_post_text ───────────────────────────────────────────────────────

#[cfg(feature = "impersonate")]
#[test]
fn extract_post_text_voyager_nested_shape() {
    use super::auth::extract_post_text;

    // GIVEN: {"commentary": {"text": {"text": "actual text"}}} (Voyager activity shape)
    let mut map = serde_json::Map::new();
    let mut commentary = serde_json::Map::new();
    let mut text_inner = serde_json::Map::new();
    text_inner.insert("text".into(), serde_json::json!("Voyager nested post text"));
    commentary.insert("text".into(), serde_json::Value::Object(text_inner));
    map.insert("commentary".into(), serde_json::Value::Object(commentary));

    // WHEN
    let result = extract_post_text(&map);
    // THEN
    assert_eq!(result.as_deref(), Some("Voyager nested post text"));
}

#[cfg(feature = "impersonate")]
#[test]
fn extract_post_text_flat_commentary_text() {
    use super::auth::extract_post_text;

    // GIVEN: {"commentary": {"text": "flat text"}}
    let mut map = serde_json::Map::new();
    let mut commentary = serde_json::Map::new();
    commentary.insert("text".into(), serde_json::json!("Flat commentary text"));
    map.insert("commentary".into(), serde_json::Value::Object(commentary));

    let result = extract_post_text(&map);
    assert_eq!(result.as_deref(), Some("Flat commentary text"));
}

#[cfg(feature = "impersonate")]
#[test]
fn extract_post_text_string_commentary() {
    use super::auth::extract_post_text;

    // GIVEN: {"commentary": "direct string"}
    let mut map = serde_json::Map::new();
    map.insert("commentary".into(), serde_json::json!("Direct string post"));

    let result = extract_post_text(&map);
    assert_eq!(result.as_deref(), Some("Direct string post"));
}

#[cfg(feature = "impersonate")]
#[test]
fn extract_post_text_returns_none_when_absent() {
    use super::auth::extract_post_text;

    // GIVEN: object with no commentary field
    let mut map = serde_json::Map::new();
    map.insert("firstName".into(), serde_json::json!("Jane"));

    let result = extract_post_text(&map);
    assert!(result.is_none());
}

#[cfg(feature = "impersonate")]
#[test]
fn extract_post_text_skips_blank_commentary() {
    use super::auth::extract_post_text;

    // GIVEN: commentary with only whitespace
    let mut map = serde_json::Map::new();
    map.insert("commentary".into(), serde_json::json!("   "));

    let result = extract_post_text(&map);
    assert!(result.is_none());
}

// ── looks_like_profile ──────────────────────────────────────────────────────

#[cfg(feature = "impersonate")]
#[test]
fn looks_like_profile_with_two_profile_keys() {
    use super::auth::looks_like_profile;

    // GIVEN: object with firstName + headline (2 profile keys — minimum threshold)
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"firstName":"Jane","headline":"Staff Engineer"}"#).unwrap();
    assert!(looks_like_profile(&map));
}

#[cfg(feature = "impersonate")]
#[test]
fn looks_like_profile_rejects_single_profile_key() {
    use super::auth::looks_like_profile;

    // GIVEN: only one profile key present — insufficient signal
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"firstName":"Jane","unrelated":"data"}"#).unwrap();
    assert!(!looks_like_profile(&map));
}

#[cfg(feature = "impersonate")]
#[test]
fn looks_like_profile_rejects_non_profile_object() {
    use super::auth::looks_like_profile;

    // GIVEN: unrelated JSON object
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"color":"blue","size":42}"#).unwrap();
    assert!(!looks_like_profile(&map));
}

// ── <code> tag JSON extraction (integration) ────────────────────────────────

#[cfg(feature = "impersonate")]
#[test]
fn code_tag_extraction_profile_data() {
    use super::auth::parse_linkedin_html;

    // GIVEN: HTML with LinkedIn-style <code> element containing profile JSON
    let profile_json = r#"{"firstName":"Jane","lastName":"Engineer","headline":"Staff Engineer at Acme","summary":"Building systems in Rust.","geoLocationName":"Helsinki, Finland","industryName":"Computer Software"}"#;
    let html = format!(
        r#"<!DOCTYPE html><html><head></head><body>
        <code style="display:none" id="bpr-guid-1"><!—{profile_json}—></code>
        <code style="display:none" id="bpr-guid-2"><!--{profile_json}--></code>
        </body></html>"#
    );

    // WHEN
    let content = parse_linkedin_html(
        &html,
        "https://linkedin.com/in/janeengineer",
        LinkedInUrlKind::Profile,
    )
    .unwrap();

    // THEN: profile fields appear in markdown
    assert!(
        content.markdown.contains("## Jane Engineer"),
        "Missing name: {}",
        content.markdown
    );
    assert!(
        content.markdown.contains("Staff Engineer at Acme"),
        "Missing headline: {}",
        content.markdown
    );
    assert!(
        content.markdown.contains("Helsinki, Finland"),
        "Missing location: {}",
        content.markdown
    );
    assert!(
        content.markdown.contains("Computer Software"),
        "Missing industry: {}",
        content.markdown
    );
    assert!(
        content.markdown.contains("Building systems in Rust."),
        "Missing summary: {}",
        content.markdown
    );
    assert_eq!(content.metadata.platform, "LinkedIn (Profile)");
}

#[cfg(feature = "impersonate")]
#[test]
fn code_tag_extraction_post_commentary() {
    use super::auth::parse_linkedin_html;

    // GIVEN: HTML with <code> elements containing post commentary JSON
    let post_json = r#"{"commentary":{"text":{"text":"Just shipped Rust HTTP/3 client."}},"actor":{"name":"Jane Engineer"}}"#;
    let html = format!(
        r#"<!DOCTYPE html><html><head></head><body>
        <code style="display:none" id="bpr-guid-1"><!--{post_json}--></code>
        </body></html>"#
    );

    // WHEN
    let content = parse_linkedin_html(
        &html,
        "https://linkedin.com/posts/jane_rust-123",
        LinkedInUrlKind::Post,
    )
    .unwrap();

    // THEN
    assert!(
        content
            .markdown
            .contains("Just shipped Rust HTTP/3 client."),
        "Missing post text: {}",
        content.markdown
    );
}

#[cfg(feature = "impersonate")]
#[test]
fn code_tag_extraction_deduplicates_posts() {
    use super::auth::parse_linkedin_html;

    // GIVEN: same post JSON appears in two different <code> elements
    let post_json = r#"{"commentary":"Unique post text."}"#;
    let html = format!(
        r"<!DOCTYPE html><html><head></head><body>
        <code><!--{post_json}--></code>
        <code><!--{post_json}--></code>
        </body></html>"
    );

    // WHEN
    let content = parse_linkedin_html(
        &html,
        "https://linkedin.com/posts/user_post-123",
        LinkedInUrlKind::Post,
    )
    .unwrap();

    // THEN: appears exactly once
    assert_eq!(
        content.markdown.matches("Unique post text.").count(),
        1,
        "Expected 1 occurrence but got more: {}",
        content.markdown
    );
}

#[cfg(feature = "impersonate")]
#[test]
fn code_tag_extraction_falls_through_to_json_ld_when_empty() {
    use super::auth::parse_linkedin_html;

    // GIVEN: <code> elements with no useful data, but JSON-LD present
    let html = r#"<!DOCTYPE html><html><head>
        <script type="application/ld+json">
        {"@type":"Person","name":"Fallback Person","description":"JSON-LD description"}
        </script>
    </head><body>
        <code><!--{"irrelevant":"noise"}--></code>
    </body></html>"#;

    // WHEN
    let content = parse_linkedin_html(
        html,
        "https://linkedin.com/in/fallback",
        LinkedInUrlKind::Profile,
    )
    .unwrap();

    // THEN: JSON-LD fallback used
    assert!(
        content.markdown.contains("## Fallback Person"),
        "Expected JSON-LD fallback: {}",
        content.markdown
    );
    assert!(
        content.markdown.contains("JSON-LD description"),
        "Expected JSON-LD desc: {}",
        content.markdown
    );
}

#[cfg(feature = "impersonate")]
#[test]
fn code_tag_extraction_handles_malformed_json_gracefully() {
    use super::auth::parse_linkedin_html;

    // GIVEN: <code> elements with broken JSON mixed with valid JSON-LD
    let html = r#"<!DOCTYPE html><html><head>
        <meta property="og:description" content="og fallback works">
    </head><body>
        <code><!--{broken json--></code>
        <code><!--not json at all--></code>
    </body></html>"#;

    // WHEN — should not panic, falls through to og:description
    let content = parse_linkedin_html(
        html,
        "https://linkedin.com/in/user",
        LinkedInUrlKind::Profile,
    )
    .unwrap();

    // THEN: og fallback used
    assert!(
        content.markdown.contains("og fallback works"),
        "Expected og fallback: {}",
        content.markdown
    );
}

#[cfg(feature = "impersonate")]
#[test]
fn code_tag_extraction_nested_profile_in_object() {
    use super::auth::parse_linkedin_html;

    // GIVEN: profile data nested inside a larger wrapper object
    let html = r#"<!DOCTYPE html><html><head></head><body>
        <code><!--{"data":{"profile":{"firstName":"Nested","lastName":"Profile","headline":"CTO at Example","summary":"Led engineering teams for a decade."}}}--></code>
    </body></html>"#;

    // WHEN
    let content = parse_linkedin_html(
        html,
        "https://linkedin.com/in/nested",
        LinkedInUrlKind::Profile,
    )
    .unwrap();

    // THEN: recursive scan finds the nested profile
    assert!(
        content.markdown.contains("## Nested Profile"),
        "Missing nested profile: {}",
        content.markdown
    );
    assert!(
        content.markdown.contains("CTO at Example"),
        "Missing headline: {}",
        content.markdown
    );
}

#[cfg(feature = "impersonate")]
#[test]
fn code_tag_profile_without_industry_still_renders() {
    use super::auth::parse_linkedin_html;

    // GIVEN: profile missing industryName
    let html = r#"<!DOCTYPE html><html><head></head><body>
        <code><!--{"firstName":"Alice","lastName":"Smith","headline":"Engineer"}--></code>
    </body></html>"#;

    let content = parse_linkedin_html(
        html,
        "https://linkedin.com/in/alice",
        LinkedInUrlKind::Profile,
    )
    .unwrap();

    assert!(content.markdown.contains("## Alice Smith"));
    assert!(content.markdown.contains("Engineer"));
    assert!(!content.markdown.contains("Industry:"));
}
