use nab::content::ContentRouter;
use nab::security::{DirectiveKind, IngestionPolicy, Severity, detect, sanitize_with_policy};

const WEBMCP_LINK_HTML: &str = include_str!("fixtures/webmcp/page-with-link.html");
const WELL_KNOWN_MANIFEST: &str = include_str!("fixtures/webmcp/well-known-mcp.json");

#[test]
fn detect_reports_webmcp_link_non_destructively() {
    let report = detect(WEBMCP_LINK_HTML);

    assert_eq!(report.webmcp_manifest_count, 1);
    assert!(report.samples.iter().any(|sample| {
        sample.kind == DirectiveKind::WebMcpManifest && sample.severity == Severity::Info
    }));

    let (cleaned, report) = sanitize_with_policy(
        WEBMCP_LINK_HTML,
        Some("https://example.com/docs"),
        &IngestionPolicy::default(),
    )
    .unwrap();
    assert_eq!(report.webmcp_manifest_count, 1);
    assert!(cleaned.contains(r#"rel="mcp""#));
}

#[test]
fn detect_reports_well_known_mcp_json_manifest() {
    let report = detect(WELL_KNOWN_MANIFEST);

    assert_eq!(report.webmcp_manifest_count, 1);
    assert!(report.samples.iter().any(|sample| {
        sample.kind == DirectiveKind::WebMcpManifest
            && sample.severity == Severity::Info
            && sample.excerpt.contains("Fixture Docs")
    }));
}

#[test]
fn strict_policy_refuses_unopted_webmcp_link() {
    let policy = IngestionPolicy {
        webmcp_strict: true,
        webmcp_opt_in: Vec::new(),
    };

    let error = sanitize_with_policy(WEBMCP_LINK_HTML, Some("https://example.com/docs"), &policy)
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("WebMCP manifest"));
    assert!(message.contains("NAB_WEBMCP_OPT_IN"));
}

#[test]
fn strict_policy_allows_explicitly_opted_host() {
    let policy = IngestionPolicy {
        webmcp_strict: true,
        webmcp_opt_in: vec!["example.com".to_owned()],
    };

    let (cleaned, report) =
        sanitize_with_policy(WEBMCP_LINK_HTML, Some("https://example.com/docs"), &policy).unwrap();

    assert_eq!(report.webmcp_manifest_count, 1);
    assert!(cleaned.contains(r#"href="/.well-known/mcp.json""#));
}

#[test]
fn strict_policy_refuses_unopted_well_known_manifest() {
    let policy = IngestionPolicy {
        webmcp_strict: true,
        webmcp_opt_in: Vec::new(),
    };

    let error = sanitize_with_policy(
        WELL_KNOWN_MANIFEST,
        Some("https://example.com/.well-known/mcp.json"),
        &policy,
    )
    .unwrap_err();

    assert!(error.to_string().contains("NAB_WEBMCP_OPT_IN"));
}

#[test]
fn content_router_converts_webmcp_link_by_default() {
    let router = ContentRouter::new();
    let conversion = router
        .convert_with_url(
            WEBMCP_LINK_HTML.as_bytes(),
            "text/html",
            Some("https://example.com/docs"),
        )
        .unwrap();

    assert!(conversion.markdown.contains("Human-readable page"));
}
