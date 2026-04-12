//! Cloudflare AI Labyrinth and similar bot-trap detection.
//!
//! [Cloudflare AI Labyrinth](https://blog.cloudflare.com/ai-labyrinth/)
//! is a defence-in-depth system that serves *invisible*, hidden-link AI
//! generated content to suspected scrapers. The pages contain real
//! scientific facts, but have nothing to do with the host site, are
//! marked `noindex`, and are riddled with `display:none` / `aria-hidden`
//! anchors. Any client that follows two such links is added to
//! Cloudflare's bot fingerprint database.
//!
//! `nab` is *not* a bot — but its fetcher behaves enough like one to
//! get caught. This module detects the trap from the HTML body alone
//! using five cheap signals:
//!
//! 1. **Hidden-link density** — ratio of `<a>` elements that are
//!    invisible (CSS `display:none`, `visibility:hidden`, `opacity:0`,
//!    or `aria-hidden="true"`) versus total links.
//! 2. **Topic drift** — keyword overlap between `<title>` and the
//!    first 500 chars of `<body>` text. AI Labyrinth pages tend to
//!    drift wildly because the body is generated, the title is the
//!    host site's chrome.
//! 3. **`noindex` + heavy body** — a `<meta name="robots"
//!    content="noindex">` directive combined with >2000 characters of
//!    rendered text. Real noindex pages (login walls, search results)
//!    are usually thin.
//! 4. **Text-structure fingerprint** — sentence-length variance.
//!    Human writing is bursty (short sentences interleaved with long
//!    ones); generated text is much more uniform.
//! 5. **Link-graph fanout** — number of distinct `<a>` targets that
//!    look auto-generated (random slugs, repeated path prefixes).
//!
//! Each signal contributes a weighted score. The total maps to a
//! [`Verdict`]: `Clean` (<30), `Suspicious` (30..60), or `Trap` (>=60).
//!
//! The scorer is deliberately conservative on the high end so that
//! tripping `Trap` requires multiple signals to agree. This module
//! never makes a network call.
//!
//! # Example
//!
//! ```no_run
//! use nab::detect::{detect_labyrinth, Verdict};
//! use url::Url;
//!
//! let html = "<html><body><h1>Hi</h1></body></html>";
//! let url = Url::parse("https://example.com/").unwrap();
//! let score = detect_labyrinth(html, &url);
//! assert!(matches!(score.verdict, Verdict::Clean));
//! ```

use std::collections::HashSet;

use scraper::{Html, Selector};
use std::sync::LazyLock;
use url::Url;

// ── Thresholds ───────────────────────────────────────────────────────
//
// Verdict thresholds. Tuned so that legitimate pages with the
// occasional `display:none` cookie banner stay below 30, and a fully
// constructed labyrinth page comfortably exceeds 60. See the unit
// tests in this file for the calibration cases.
const SUSPICIOUS_THRESHOLD: f32 = 30.0;
const TRAP_THRESHOLD: f32 = 60.0;

// ── Selectors (compiled once) ────────────────────────────────────────

static A_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("a").expect("static <a> selector"));
static TITLE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("title").expect("static <title> selector"));
static BODY_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("body").expect("static <body> selector"));
static META_ROBOTS_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse(r#"meta[name="robots"], meta[name="ROBOTS"]"#)
        .expect("static meta robots selector")
});

// ── Public types ─────────────────────────────────────────────────────

/// Final classification of a fetched page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Page looks like normal human content. Score < 30.
    Clean,
    /// Page exhibits one or two trap-like properties. Score 30..60.
    /// Worth logging, not worth aborting.
    Suspicious,
    /// Page is almost certainly a bot trap. Score >= 60.
    /// Caller should refuse to return content.
    Trap,
}

/// One contribution to the total score.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// Short stable identifier for the signal (e.g. `"hidden_link_density"`).
    pub name: &'static str,
    /// Raw measurement (ratio, character count, variance, etc.).
    pub value: f32,
    /// Weighted score added to the total. Range 0..=40.
    pub score: f32,
    /// Human-readable explanation for logs / `--verbose`.
    pub detail: String,
}

/// Result of running [`detect_labyrinth`] on a page.
#[derive(Debug, Clone)]
pub struct LabyrinthScore {
    /// Sum of all signal scores.
    pub total: f32,
    /// Per-signal breakdown for debugging.
    pub signals: Vec<Signal>,
    /// Final classification.
    pub verdict: Verdict,
}

impl LabyrinthScore {
    /// `true` if the verdict is `Trap`.
    #[must_use]
    pub fn is_trap(&self) -> bool {
        matches!(self.verdict, Verdict::Trap)
    }

    /// `true` if the verdict is `Suspicious` or worse.
    #[must_use]
    pub fn is_suspicious(&self) -> bool {
        matches!(self.verdict, Verdict::Suspicious | Verdict::Trap)
    }
}

/// Inspect `html` and assign a labyrinth score.
///
/// `url` is used for link-graph fanout (resolving relative anchors and
/// classifying internal vs external links). The function never
/// performs network IO.
///
/// # Performance
///
/// One full HTML parse plus a handful of CSS selector walks. On a
/// 200 KB document this completes in single-digit milliseconds on a
/// modern laptop.
#[must_use]
pub fn detect_labyrinth(html: &str, url: &Url) -> LabyrinthScore {
    let doc = Html::parse_document(html);

    let signals = vec![
        score_hidden_link_density(&doc),
        score_topic_drift(&doc),
        score_meta_directive(&doc),
        score_text_structure(&doc),
        score_link_graph_fanout(&doc, url),
    ];

    let total: f32 = signals.iter().map(|s| s.score).sum();
    let verdict = classify(total);

    LabyrinthScore {
        total,
        signals,
        verdict,
    }
}

fn classify(total: f32) -> Verdict {
    if total >= TRAP_THRESHOLD {
        Verdict::Trap
    } else if total >= SUSPICIOUS_THRESHOLD {
        Verdict::Suspicious
    } else {
        Verdict::Clean
    }
}

// ── Signal 1: hidden-link density ────────────────────────────────────

/// Returns `true` if the inline `style` attribute or `aria-hidden`
/// attribute marks the element as visually hidden. We deliberately do
/// not parse stylesheets — labyrinth pages embed the hiding inline so
/// the trap renders identically with or without external CSS.
fn looks_hidden(style: Option<&str>, aria_hidden: Option<&str>, hidden_attr: bool) -> bool {
    if hidden_attr {
        return true;
    }
    if matches!(aria_hidden, Some(v) if v.eq_ignore_ascii_case("true")) {
        return true;
    }
    let Some(style) = style else { return false };
    let s: String = style.chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.to_ascii_lowercase();
    s.contains("display:none")
        || s.contains("visibility:hidden")
        || s.contains("opacity:0")
        // CSS shorthand: "opacity:0;" matches above; also catch "opacity:.0".
        || s.contains("opacity:.0")
}

fn score_hidden_link_density(doc: &Html) -> Signal {
    let mut total = 0u32;
    let mut hidden = 0u32;

    for a in doc.select(&A_SELECTOR) {
        total += 1;
        let el = a.value();
        if looks_hidden(
            el.attr("style"),
            el.attr("aria-hidden"),
            el.attr("hidden").is_some(),
        ) {
            hidden += 1;
        }
    }

    let ratio = if total == 0 {
        0.0
    } else {
        f32::from(u16::try_from(hidden).unwrap_or(u16::MAX))
            / f32::from(u16::try_from(total).unwrap_or(u16::MAX))
    };

    // Cliff-edge weighting: <2% normal, 2-10% odd, 10-30% suspicious,
    // >30% almost certainly a trap. The Cloudflare reference pages run
    // ~80-100% hidden, so we cap the score at 40 once we cross 30%.
    //
    // We *also* require a minimum number of hidden links to avoid
    // false-positives on legitimate pages with one or two hidden cookie
    // banner / privacy / accessibility anchors. Real labyrinth pages
    // have dozens of hidden anchors.
    let score = if total < 10 || hidden < 5 {
        // Too few links — or too few hidden — to be statistically meaningful.
        0.0
    } else if ratio >= 0.30 {
        40.0
    } else if ratio >= 0.10 {
        25.0
    } else if ratio >= 0.02 {
        8.0
    } else {
        0.0
    };

    Signal {
        name: "hidden_link_density",
        value: ratio,
        score,
        detail: format!("{hidden}/{total} links hidden ({:.1}%)", ratio * 100.0),
    }
}

// ── Signal 2: topic drift ────────────────────────────────────────────

/// Lowercase, strip punctuation, split into >=4 character tokens.
/// Stop-words and short tokens contribute nothing to topical
/// similarity, so we drop them aggressively to keep the heuristic
/// sharp on small samples.
fn tokenize(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(str::to_ascii_lowercase)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

const STOPWORDS: &[&str] = &[
    "this", "that", "with", "from", "have", "been", "were", "they", "their", "there", "which",
    "about", "would", "could", "should", "into", "more", "than", "then", "what", "when", "your",
    "will", "also", "such", "some", "other", "these", "those", "page", "site",
];

fn score_topic_drift(doc: &Html) -> Signal {
    let title_text: String = doc
        .select(&TITLE_SELECTOR)
        .next()
        .map(|t| t.text().collect::<String>())
        .unwrap_or_default();

    let body_text: String = doc
        .select(&BODY_SELECTOR)
        .next()
        .map(|b| b.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();

    let body_sample: String = body_text.chars().take(500).collect();

    let title_tokens = tokenize(&title_text);
    let body_tokens = tokenize(&body_sample);

    if title_tokens.is_empty() || body_tokens.is_empty() {
        // Cannot judge — abstain.
        return Signal {
            name: "topic_drift",
            value: 0.0,
            score: 0.0,
            detail: "title or body too short to compare".to_string(),
        };
    }

    let intersection = title_tokens.intersection(&body_tokens).count();
    let union = title_tokens.union(&body_tokens).count();
    // Jaccard similarity. Range 0..=1.
    let similarity = if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    };

    // Drift is the inverse of similarity. We only penalise *combined*
    // with rich body text — short pages legitimately have unrelated
    // chrome titles ("Login | Acme") and short forms.
    let score = if body_text.len() < 800 {
        0.0
    } else if similarity < 0.05 {
        20.0
    } else if similarity < 0.15 {
        8.0
    } else {
        0.0
    };

    Signal {
        name: "topic_drift",
        value: similarity,
        score,
        detail: format!(
            "title∩body Jaccard={:.2} (title={}t, body={}t)",
            similarity,
            title_tokens.len(),
            body_tokens.len()
        ),
    }
}

// ── Signal 3: noindex + rich body ────────────────────────────────────

fn score_meta_directive(doc: &Html) -> Signal {
    let mut noindex = false;
    for meta in doc.select(&META_ROBOTS_SELECTOR) {
        if let Some(content) = meta.value().attr("content")
            && content.to_ascii_lowercase().contains("noindex")
        {
            noindex = true;
            break;
        }
    }

    let body_chars = doc
        .select(&BODY_SELECTOR)
        .next()
        .map_or(0, |b| b.text().map(str::len).sum::<usize>());

    // Real noindex pages (search results, login pages) are usually
    // thin. A rich, articulate noindex page is the labyrinth's
    // signature.
    let score = if noindex && body_chars > 2000 {
        20.0
    } else if noindex && body_chars > 800 {
        6.0
    } else {
        0.0
    };

    Signal {
        name: "meta_directive",
        value: if noindex { 1.0 } else { 0.0 },
        score,
        detail: format!("noindex={noindex}, body_chars={body_chars}"),
    }
}

// ── Signal 4: text structure fingerprint ─────────────────────────────

fn sentence_lengths(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut current: usize = 0;
    for ch in text.chars() {
        if ch == '.' || ch == '!' || ch == '?' {
            if current >= 3 {
                out.push(current);
            }
            current = 0;
        } else if !ch.is_whitespace() {
            current += 1;
        } else if current > 0 {
            // count whitespace as part of the in-progress sentence
            current += 1;
        }
    }
    if current >= 3 {
        out.push(current);
    }
    out
}

fn variance(samples: &[usize]) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }
    let mean: f32 = samples.iter().map(|&n| n as f32).sum::<f32>() / samples.len() as f32;
    let var: f32 = samples
        .iter()
        .map(|&n| {
            let d = n as f32 - mean;
            d * d
        })
        .sum::<f32>()
        / samples.len() as f32;
    var
}

fn score_text_structure(doc: &Html) -> Signal {
    let body_text: String = doc
        .select(&BODY_SELECTOR)
        .next()
        .map(|b| b.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();

    let lengths = sentence_lengths(&body_text);
    if lengths.len() < 8 {
        return Signal {
            name: "text_structure",
            value: 0.0,
            score: 0.0,
            detail: format!("only {} sentences, abstaining", lengths.len()),
        };
    }
    let var = variance(&lengths);
    let mean: f32 = lengths.iter().map(|&n| n as f32).sum::<f32>() / lengths.len() as f32;
    // Coefficient of variation: dimensionless, comparable across pages.
    let cv = if mean > 0.0 { var.sqrt() / mean } else { 0.0 };

    // Human prose typically has CV >= 0.5. AI-generated trap text in
    // the wild measures around 0.15-0.30. Below 0.20 is a strong tell.
    let score = if cv < 0.20 {
        15.0
    } else if cv < 0.30 {
        6.0
    } else {
        0.0
    };

    Signal {
        name: "text_structure",
        value: cv,
        score,
        detail: format!("{} sentences, mean={mean:.1}, cv={cv:.2}", lengths.len()),
    }
}

// ── Signal 5: link-graph fanout ──────────────────────────────────────

fn score_link_graph_fanout(doc: &Html, base: &Url) -> Signal {
    let base_host = base.host_str().unwrap_or("");
    let mut internal_targets: HashSet<String> = HashSet::new();
    let mut slug_like: u32 = 0;

    for a in doc.select(&A_SELECTOR) {
        let Some(href) = a.value().attr("href") else {
            continue;
        };
        let Ok(resolved) = base.join(href) else {
            continue;
        };
        if resolved.host_str() != Some(base_host) || base_host.is_empty() {
            continue;
        }
        let path = resolved.path().to_string();
        // "Slug-like" = last path segment is long (>=10 chars) and
        // contains both letters and digits, the typical look of an
        // auto-generated trap URL like /article/abc123def-kepler.
        if let Some(last) = path.rsplit('/').find(|s| !s.is_empty())
            && last.len() >= 10
            && last.chars().any(|c| c.is_ascii_digit())
            && last.chars().any(|c| c.is_ascii_alphabetic())
        {
            slug_like += 1;
        }
        internal_targets.insert(path);
    }

    let distinct = internal_targets.len();
    // Pages that fan out to dozens of slug-like internal targets are
    // suspicious; legitimate articles rarely cross 10.
    let score = if distinct >= 40 && slug_like >= 20 {
        15.0
    } else if distinct >= 20 && slug_like >= 10 {
        8.0
    } else {
        0.0
    };

    Signal {
        name: "link_graph_fanout",
        value: distinct as f32,
        score,
        detail: format!("{distinct} distinct internal targets, {slug_like} slug-like"),
    }
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
#[allow(clippy::format_push_string)] // tests build small fixtures, readability > micro-perf
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://example.com/article").unwrap()
    }

    /// A small, real-world looking page rendered from a README-style
    /// markdown source. Should score Clean.
    fn clean_readme_page() -> String {
        let mut html = String::from(
            r#"<!doctype html>
<html lang="en">
<head>
  <title>nab — README</title>
  <meta charset="utf-8">
</head>
<body>
  <header><a href="/">Home</a> <a href="/docs">Docs</a> <a href="/changelog">Changelog</a></header>
  <article>
    <h1>nab — README</h1>
    <p>nab is a small command-line HTTP client that turns any URL into clean
    markdown for LLM context windows. It supports cookies, 1Password, HTTP/3,
    and a labyrinth detector that protects scrapers from Cloudflare's bot trap.</p>
    <p>Install with cargo install nab. The README explains every flag in detail.
    Most users only need <code>nab fetch URL</code>; everything else is opt-in.</p>
    <p>nab speaks markdown so an LLM can read its output without wading through
    HTML chrome. The readability extractor keeps the article body and discards
    navigation, ads, and footers.</p>
    <p>Building from source requires Rust 1.93. Run cargo test to execute the
    suite, which is fast: most tests finish in milliseconds.</p>
    <p>Bug reports are welcome on GitHub. Please attach a minimal reproduction.</p>
  </article>
  <footer><a href="/about">About</a></footer>
</body>
</html>"#,
        );
        // pad with realistic prose so length checks fire correctly
        for _ in 0..3 {
            html.push_str(
                r"<p>nab is honest about its limitations. It will not log in for you,
will not solve CAPTCHAs, and will not pretend to be a browser it isn't.
The goal is reliable text, not stealth. Yes, that means some sites refuse
to serve us. We accept that trade-off.</p>",
            );
        }
        html
    }

    /// Synthetic Cloudflare-style labyrinth: many hidden links, noindex,
    /// uniform AI-flavored sentences, slug-like fanout.
    fn synthetic_trap_page() -> String {
        let mut html = String::from(
            r#"<!doctype html>
<html><head>
<title>Curated Hub</title>
<meta name="robots" content="noindex, nofollow">
</head><body>
<h1>Knowledge Index</h1>
"#,
        );
        // 50 hidden anchors with slug-like targets.
        for i in 0..50 {
            html.push_str(&format!(
                r#"<a href="/labyrinth/article-{i}-kepler-9b" style="display:none">Article {i} on Kepler 9b</a>
"#,
            ));
        }
        // 6 visible (still slug-like) anchors so total > hidden_count is ~88%.
        for i in 0..6 {
            html.push_str(&format!(
                r#"<a href="/labyrinth/topic-{i}-quantum-3a">Topic {i}</a>
"#,
            ));
        }
        // Uniform AI-flavored prose, low burstiness, > 2000 chars,
        // and ZERO overlap with the title "Curated Hub".
        for _ in 0..30 {
            html.push_str(
                "The orbit measured exactly forty seven minutes around the dwarf companion. \
                 Astronomers logged precisely twelve transits during the seasonal window. \
                 Spectral lines indicated roughly six different volatile compounds present. \
                 Photometric variation peaked around eighteen percent across the cycle. \
                 Researchers documented carefully nine candidate planetary signatures here. ",
            );
        }
        html.push_str("</body></html>");
        html
    }

    /// Two of five signals firing — should land in Suspicious.
    fn partial_match_page() -> String {
        let mut html = String::from(
            r#"<!doctype html>
<html><head><title>Acme Login</title>
<meta name="robots" content="noindex">
</head><body>"#,
        );
        // Trigger meta_directive (noindex + rich body) and topic_drift
        // (title says "Acme Login", body talks about astronomy).
        for _ in 0..40 {
            html.push_str(
                "<p>The orbit measured forty seven minutes around the dwarf companion star. \
                 Spectral lines indicated multiple different volatile compounds present today. \
                 Photometric variation peaked at eighteen percent across the observation cycle.</p>",
            );
        }
        // A normal, mostly visible link set.
        for i in 0..5 {
            html.push_str(&format!(r#"<a href="/page{i}">Page {i}</a>"#));
        }
        html.push_str("</body></html>");
        html
    }

    /// Legitimate page with a couple of `display:none` cookie banner
    /// anchors. Must NOT trip the detector.
    fn legitimate_cookie_banner_page() -> String {
        let mut html = String::from(
            r#"<!doctype html>
<html><head><title>Acme Blog — How we built nab</title></head>
<body>
<nav>
  <a href="/">Home</a>
  <a href="/blog">Blog</a>
  <a href="/about">About</a>
</nav>
<div class="cookie-banner">
  <a href="/cookies" style="display:none">Cookie policy</a>
  <a href="/privacy" style="display:none">Privacy policy</a>
</div>
<article>
<h1>How we built nab</h1>
"#,
        );
        for _ in 0..5 {
            html.push_str(
                "<p>We built nab because the existing tools were either too heavy or too \
                 fragile. We wanted a single binary that could fetch any URL, follow \
                 redirects sanely, and give us back clean markdown. The first prototype \
                 took a weekend. Most of the work after that was removing things.</p>
                 <p>Honesty is the design principle. nab does not pretend to be a browser \
                 it isn't. nab does not log in for you. nab does not solve CAPTCHAs.</p>",
            );
        }
        html.push_str("</article></body></html>");
        html
    }

    #[test]
    fn clean_real_world_page_scores_below_30() {
        let html = clean_readme_page();
        let score = detect_labyrinth(&html, &url());
        eprintln!("clean: total={} signals={:?}", score.total, score.signals);
        assert!(
            score.total < SUSPICIOUS_THRESHOLD,
            "clean page scored {} (>= {SUSPICIOUS_THRESHOLD})",
            score.total
        );
        assert_eq!(score.verdict, Verdict::Clean);
    }

    #[test]
    fn synthetic_labyrinth_scores_above_60() {
        let html = synthetic_trap_page();
        let score = detect_labyrinth(&html, &url());
        eprintln!("trap: total={} signals={:?}", score.total, score.signals);
        assert!(
            score.total >= TRAP_THRESHOLD,
            "trap page only scored {} (< {TRAP_THRESHOLD})",
            score.total
        );
        assert_eq!(score.verdict, Verdict::Trap);
    }

    #[test]
    fn partial_match_is_suspicious() {
        let html = partial_match_page();
        let score = detect_labyrinth(&html, &url());
        eprintln!("partial: total={} signals={:?}", score.total, score.signals);
        assert_eq!(
            score.verdict,
            Verdict::Suspicious,
            "expected Suspicious, got {:?} (total={})",
            score.verdict,
            score.total
        );
    }

    #[test]
    fn legitimate_cookie_banner_is_clean() {
        let html = legitimate_cookie_banner_page();
        let score = detect_labyrinth(&html, &url());
        eprintln!(
            "cookie banner: total={} signals={:?}",
            score.total, score.signals
        );
        assert_eq!(score.verdict, Verdict::Clean);
    }

    #[test]
    fn empty_page_does_not_panic() {
        let score = detect_labyrinth("", &url());
        assert_eq!(score.verdict, Verdict::Clean);
        assert_eq!(score.signals.len(), 5);
    }

    #[test]
    fn looks_hidden_recognises_common_patterns() {
        assert!(looks_hidden(Some("display: none"), None, false));
        assert!(looks_hidden(Some("DISPLAY:NONE;"), None, false));
        assert!(looks_hidden(Some("visibility: hidden"), None, false));
        assert!(looks_hidden(Some("opacity: 0"), None, false));
        assert!(looks_hidden(None, Some("true"), false));
        assert!(looks_hidden(None, None, true));
        assert!(!looks_hidden(Some("color: red"), None, false));
        assert!(!looks_hidden(None, Some("false"), false));
        assert!(!looks_hidden(None, None, false));
    }

    #[test]
    fn fixture_file_scores_as_trap() {
        let html = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/labyrinth_sample.html"
        ))
        .expect("fixture file present");
        let score = detect_labyrinth(&html, &url());
        eprintln!("fixture: total={} signals={:?}", score.total, score.signals);
        assert!(score.is_trap(), "fixture should classify as Trap");
    }
}
