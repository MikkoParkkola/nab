//! Rung-3 live smoke test — renders a real page through an EXTERNAL Chrome over
//! CDP using the task engine's browser primitive (`BrowserLogin::render_markdown`).
//!
//! Ignored by default (needs a running Chrome with `--remote-debugging-port`).
//! Run against your browser:
//!
//! ```bash
//! NAB_BROWSER_CDP_PORT=9222 cargo test --features browser --test rung3_browser_live -- --ignored --nocapture
//! ```
#![cfg(feature = "browser")]

#[tokio::test]
#[ignore = "requires a running Chrome on NAB_BROWSER_CDP_PORT (default 9222)"]
async fn renders_real_page_through_external_chrome() {
    let port = std::env::var("NAB_BROWSER_CDP_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok());

    let login = nab::BrowserLogin::connect(port)
        .await
        .expect("connect to Chrome — start it with --remote-debugging-port=9222");

    // example.com is tiny and stable; the render must return its heading text.
    let url = "https://example.com/";
    let markdown = login
        .render_markdown(url)
        .await
        .expect("render_markdown should return screened markdown");

    println!("[rung3] rendered {} chars of markdown", markdown.len());
    assert!(
        markdown.contains("Example Domain"),
        "rendered markdown should contain the page heading, got: {markdown:.200}"
    );
}
