pub mod analyze;
pub mod annotate;
pub mod auth;
pub mod bench;
pub mod context;
pub mod cookies;
pub mod export_rules;
pub mod fetch;
pub mod fetch_batch;
pub mod fingerprint;
pub mod login;
pub mod mcp;
pub mod models;
pub mod otp;
pub mod output;
pub mod provider;
pub mod spa;
pub mod stream;
pub mod submit;
pub mod validate;
pub mod watch;

pub use analyze::{AnalyzeConfig, cmd_analyze};
pub use annotate::{AnnotateConfig, cmd_annotate};
pub use auth::cmd_auth;
pub use bench::cmd_bench;
pub use context::cmd_context;
pub use cookies::cmd_cookies;
pub use export_rules::{cmd_export_rules, cmd_list_rules};
pub use fetch::{FetchConfig, cmd_fetch};
pub use fingerprint::cmd_fingerprint;
pub use login::{LoginConfig, cmd_login};
pub use mcp::{
    InstallConfig as McpInstallConfig, ServeConfig as McpServeConfig, cmd_mcp_install,
    cmd_mcp_serve,
};
pub use models::{cmd_models_fetch, cmd_models_list, cmd_models_update, cmd_models_verify};
pub use otp::cmd_otp;
pub use provider::{cmd_provider_install, cmd_provider_list, cmd_provider_remove};
pub use spa::{SpaConfig, cmd_spa};
pub use stream::{StreamCmdConfig, cmd_stream};
pub use submit::{SubmitConfig, cmd_submit};
pub use validate::cmd_validate;
pub use watch::{
    WatchAddConfig, WatchListConfig, WatchListFormat, WatchLogsConfig, cmd_watch_add,
    cmd_watch_list, cmd_watch_logs, cmd_watch_remove,
};

/// Extract the host/domain from a URL, returning an empty string on failure.
///
/// Replaces the 8× repeated `url::Url::parse(...).ok().and_then(|u| u.host_str().map(…))`
/// pattern scattered across `cmd/` subcommands.
pub fn extract_domain(url: &str) -> String {
    nab::util::extract_domain(url)
}

/// Build a `Referer` header value from a URL (scheme + host + "/").
///
/// Returns `None` if the URL cannot be parsed.
pub fn build_referer(url: &str) -> Option<String> {
    nab::util::build_referer(url)
}

/// Resolve browser name from cookie flag value.
///
/// Returns `None` for `"none"`, auto-detects for `"auto"`, or passes
/// through the explicit browser name.
pub fn resolve_browser_name(cookies: &str) -> Option<String> {
    match cookies.to_lowercase().as_str() {
        "none" => None,
        "auto" => Some(
            nab::detect_default_browser()
                .map_or_else(|_| "chrome".to_string(), |b| b.as_str().to_string()),
        ),
        _ => Some(cookies.to_string()),
    }
}

/// Resolve [`CookieSource`] from browser name string.
pub fn resolve_cookie_source(browser: &str) -> nab::CookieSource {
    nab::CookieSource::from_browser_name(browser)
}

/// Return `Some(s)` if non-empty, `None` otherwise.
///
/// Useful when passing optional cookie/header strings to APIs that take `Option<&str>`.
pub fn non_empty(s: &str) -> Option<&str> {
    if s.is_empty() { None } else { Some(s) }
}

/// Resolve the cookie header for a domain, given the `--cookies` flag value.
///
/// Convenience wrapper combining [`resolve_browser_name`], [`resolve_cookie_source`],
/// and `get_cookie_header`. Returns an empty string when cookies are disabled or
/// unavailable.
pub fn resolve_cookie_header(cookies: &str, domain: &str) -> String {
    let browser = resolve_browser_name(cookies);
    nab::util::resolve_cookie_header_for_domain(domain, browser.as_deref())
}
