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
pub mod otp;
pub mod output;
pub mod spa;
pub mod stream;
pub mod submit;
pub mod validate;

pub use analyze::cmd_analyze;
pub use annotate::{AnnotateConfig, cmd_annotate};
pub use auth::cmd_auth;
pub use bench::cmd_bench;
pub use context::cmd_context;
pub use cookies::cmd_cookies;
pub use export_rules::{cmd_export_rules, cmd_list_rules};
pub use fetch::{FetchConfig, cmd_fetch};
pub use fingerprint::cmd_fingerprint;
pub use login::{LoginConfig, cmd_login};
pub use otp::cmd_otp;
pub use spa::{SpaConfig, cmd_spa};
pub use stream::{StreamCmdConfig, cmd_stream};
pub use submit::{SubmitConfig, cmd_submit};
pub use validate::cmd_validate;

/// Extract the host/domain from a URL, returning an empty string on failure.
///
/// Replaces the 8× repeated `url::Url::parse(...).ok().and_then(|u| u.host_str().map(…))`
/// pattern scattered across `cmd/` subcommands.
pub fn extract_domain(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Build a `Referer` header value from a URL (scheme + host + "/").
///
/// Returns `None` if the URL cannot be parsed.
pub fn build_referer(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .map(|parsed| format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or("")))
}
