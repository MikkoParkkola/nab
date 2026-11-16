//! SPA / structured-data extraction for JS-rendered pages.
//!
//! Extracts article content from embedded JSON bundles in single-page
//! applications (Next.js, Nuxt, Redux) and from Schema.org JSON-LD
//! structured data.
//!
//! # Extraction Pipeline
//!
//! 1. `<script id="__NEXT_DATA__">` (Next.js SSR data)
//! 2. `<script id="__NUXT_DATA__">` / `<script id="__nuxt-data">` (Nuxt.js SSR data)
//! 3. `<script type="application/ld+json">` (Schema.org structured data)
//! 4. Inline `<script>` variable assignments (`window.__NEXT_DATA__ = {...}`)
//! 5. Hidden `<code>` elements with JSON (LinkedIn-style SPA hydration)
//! 6. Pre-fetched API response envelopes (`{status: 200, body: "{...}"}`) in any JSON
//!
//! # Next.js MDX Content Chunk Recovery
//!
//! Some Next.js sites (e.g., blogs using MDX) embed only metadata in
//! `__NEXT_DATA__` and load article content lazily from webpack chunks.
//! [`discover_nextjs_content_chunks`] parses the webpack runtime to find
//! content chunk URLs, and [`extract_jsx_text_content`] extracts readable
//! text from the compiled JSX.  The async fetch layer can use these to
//! make secondary requests when thin content is detected.

mod spa;

// Re-export the public API — callers use `content::spa_extract::*`.
pub use spa::helpers::{find_content_by_key, find_longest_string};
pub use spa::inline::{
    extract_balanced_json, extract_inline_script_json, unwrap_api_response_bodies,
};
pub use spa::jsonld::extract_jsonld_content;
pub use spa::nextjs::{
    discover_nextjs_content_chunks, extract_jsx_text_content, extract_nextjs_content,
    is_nextjs_metadata_only, resolve_content_chunk_urls, resolve_content_chunk_urls_for_slug,
};

/// Which kind of extractor produced an SPA candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaSource {
    /// Page data from a framework-specific block: Next.js, Nuxt, `SvelteKit`,
    /// Gatsby, Angular Universal, JSON-LD `articleBody`, an inline
    /// `window.__NEXT_DATA__ = {...}` assignment, or hidden `<code>` JSON.
    /// Trusted to beat server-rendered HTML.
    Framework,
    /// An app-wide store assignment (`window.__PRELOADED_STATE__`,
    /// `__APOLLO_STATE__`, `__NUXT__` and the like) or the catch-all scraper's
    /// longest string from any `= {...}` assignment. Either can be a bio, a
    /// keyword list, query variables or CSS, so it only fills a thin result.
    GenericAssignment,
}

/// Text recovered from embedded SPA data, with the extractor that found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaCandidate {
    /// Rendered markdown text.
    pub text: String,
    /// Which extractor produced it.
    pub source: SpaSource,
}

/// Try to extract article content from SPA JSON bundles embedded in HTML.
///
/// Modern single-page applications (Next.js, Nuxt, `SvelteKit`, Gatsby, Angular
/// Universal, etc.) embed serialized server-side render state in `<script>` tags.
/// This function extracts that state and recursively searches for the longest
/// text content field.
///
/// Returns `Some(markdown)` if a substantial content field is found (>200 chars),
/// `None` otherwise.
pub fn extract_spa_data(html: &str) -> Option<String> {
    extract_spa_candidate(html).map(|candidate| candidate.text)
}

/// Like [`extract_spa_data`], but also reports which extractor produced the text.
pub fn extract_spa_candidate(html: &str) -> Option<SpaCandidate> {
    let framework = |text: String| SpaCandidate {
        text,
        source: SpaSource::Framework,
    };
    let document = scraper::Html::parse_document(html);

    // Try __NEXT_DATA__ (Next.js) — highest priority, most structured
    if let Some(content) = spa::nextjs::try_extract_script_json(&document, "script#__NEXT_DATA__") {
        return Some(framework(content));
    }

    // Try __NUXT_DATA__ / __NUXT_STATE__ (Nuxt.js)
    for selector in &["script#__NUXT_DATA__", "script#__nuxt-data"] {
        if let Some(content) = spa::nextjs::try_extract_script_json(&document, selector) {
            return Some(framework(content));
        }
    }

    // Try SvelteKit fetched data blocks
    if let Some(content) = spa::sveltekit::extract_sveltekit_data(&document) {
        return Some(framework(content));
    }

    // Try Gatsby SSR data blocks
    if let Some(content) = spa::gatsby::extract_gatsby_data(&document, html) {
        return Some(framework(content));
    }

    // Try Angular Universal transfer state
    if let Some(content) = spa::angular::extract_angular_universal_state(&document) {
        return Some(framework(content));
    }

    // Try JSON-LD structured data (Schema.org — widely used by modern blogs)
    if let Some(content) = spa::jsonld::extract_jsonld_content(&document) {
        return Some(framework(content));
    }

    // Try inline script variable assignments (window.__NEXT_DATA__ = {...}, etc.)
    if let Some((text, source)) = spa::inline::extract_inline_script_json_with_source(html) {
        return Some(SpaCandidate { text, source });
    }

    // Try hidden <code> elements with JSON (LinkedIn-style SPA hydration).
    if let Some(content) = spa::inline::extract_hidden_code_json(&document) {
        return Some(framework(content));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_spa_data_detects_sveltekit() {
        // GIVEN: a full HTML page that only carries SvelteKit prefetch blocks
        let article = "SvelteKit integration test: this content must be long enough to pass the \
                       minimum two hundred character threshold applied by extract_spa_data so that \
                       the SvelteKit extractor is exercised end-to-end via the main entry point.";
        let payload = serde_json::json!({"status": 200, "body": {"content": article}});
        let html = format!(
            r#"<html><body>
            <script type="application/json" data-sveltekit-fetched data-url="/api/post">
            {payload}
            </script>
            </body></html>"#,
            payload = serde_json::to_string(&payload).unwrap()
        );

        let result = extract_spa_data(&html);
        assert!(result.is_some());
        assert!(result.unwrap().contains("SvelteKit integration test"));
    }

    #[test]
    fn extract_spa_data_detects_gatsby() {
        // GIVEN: HTML that only carries a Gatsby SSR tag
        let article = "Gatsby end-to-end integration test: this content is deliberately over two \
                       hundred characters so that the Gatsby extractor path inside extract_spa_data \
                       is exercised and we confirm the framework is wired into the main try-chain.";
        let payload = serde_json::json!({"result": {"data": {"body": article}}});
        let html = format!(
            r#"<html><body>
            <script type="application/json" data-gatsby-ssr>
            {payload}
            </script>
            </body></html>"#,
            payload = serde_json::to_string(&payload).unwrap()
        );

        let result = extract_spa_data(&html);
        assert!(result.is_some());
        assert!(
            result
                .unwrap()
                .contains("Gatsby end-to-end integration test")
        );
    }

    #[test]
    fn extract_spa_data_detects_angular_universal() {
        // GIVEN: HTML that only carries an Angular Universal transfer state block
        let article = "Angular Universal end-to-end integration test: this content body is \
                       intentionally long enough to exceed the two hundred character minimum so \
                       that the Angular Universal extractor path within extract_spa_data is \
                       exercised and we confirm it is wired into the main try-chain correctly.";
        let state = serde_json::json!({
            "cache.key": {"status": 200, "body": {"content": article}}
        });
        let html = format!(
            r#"<html><body>
            <script id="serverApp-state" type="application/json">
            {state}
            </script>
            </body></html>"#,
            state = serde_json::to_string(&state).unwrap()
        );

        let result = extract_spa_data(&html);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Angular Universal end-to-end"));
    }
}
