//! Markdown template rendering for site rules.
//!
//! Supports three placeholder forms:
//! - `{field}` — replaced with the extracted field value.
//! - `{field|number}` — field value formatted with K/M suffixes.
//! - `{field|strip_html}` — field value with HTML tags stripped and entities decoded.
//!
//! Lines that still contain an unresolved `{…}` placeholder after substitution
//! are **omitted entirely** from the output.  This cleanly handles optional
//! fields (e.g., a thumbnail that may not exist).
//!
//! Leading/trailing blank lines in the final output are trimmed.
//!
//! # Example
//!
//! ```rust
//! use std::collections::HashMap;
//! use nab::site::rules::template::render;
//!
//! let mut fields = HashMap::new();
//! fields.insert("title".to_string(), "My Video".to_string());
//! fields.insert("author".to_string(), "Alice".to_string());
//!
//! let tmpl = "## {title}\n\nby {author}\n\n[missing field: {thumbnail}]\n";
//! let output = render(tmpl, &fields, "https://example.com");
//!
//! assert!(output.contains("## My Video"));
//! assert!(output.contains("by Alice"));
//! assert!(!output.contains("thumbnail")); // line omitted
//! ```

use std::collections::HashMap;
use std::hash::BuildHasher;

/// Render `template` by substituting extracted `fields`.
///
/// `original_url` is always available as the `{original_url}` placeholder.
pub fn render<S: BuildHasher>(
    template: &str,
    fields: &HashMap<String, String, S>,
    original_url: &str,
) -> String {
    let lines: Vec<String> = template
        .lines()
        .filter_map(|line| render_line(line, fields, original_url))
        .collect();

    trim_blank_edges(&lines)
}

/// Render a single template line, returning `None` if any placeholder remains
/// unresolved after substitution.
fn render_line<S: BuildHasher>(
    line: &str,
    fields: &HashMap<String, String, S>,
    original_url: &str,
) -> Option<String> {
    let mut result = line.to_string();

    // Substitute filtered placeholders first (more specific before generic).
    result = substitute_filtered_placeholders(&result, fields);

    // Substitute {original_url}.
    result = result.replace("{original_url}", original_url);

    // Substitute {field} for each known field.
    for (name, value) in fields {
        result = result.replace(&format!("{{{name}}}"), value);
    }

    // If any {…} placeholder remains, omit this line.
    if has_unresolved_placeholder(&result) {
        None
    } else {
        Some(result)
    }
}

/// Replace `{field|number}` and `{field|strip_html}` placeholders.
fn substitute_filtered_placeholders<S: BuildHasher>(
    line: &str,
    fields: &HashMap<String, String, S>,
) -> String {
    let mut result = line.to_string();
    let mut search_from = 0;

    loop {
        let Some(open) = result[search_from..].find('{') else {
            break;
        };
        let open = open + search_from;

        let Some(close) = result[open..].find('}') else {
            break;
        };
        let close = close + open;

        let placeholder_inner = &result[open + 1..close];

        if let Some(field_name) = placeholder_inner.strip_suffix("|number")
            && let Some(value) = fields.get(field_name)
        {
            let formatted = format_number(value);
            let placeholder = format!("{{{placeholder_inner}}}");
            result = result.replacen(&placeholder, &formatted, 1);
            continue;
        }

        if let Some(field_name) = placeholder_inner.strip_suffix("|strip_html")
            && let Some(value) = fields.get(field_name)
        {
            let stripped = strip_html(value);
            let placeholder = format!("{{{placeholder_inner}}}");
            result = result.replacen(&placeholder, &stripped, 1);
            continue;
        }

        // {field|truncate:N} — truncate to N characters at a word boundary.
        if let Some((rest, len_str)) = placeholder_inner
            .rsplit_once("|truncate:")
            .and_then(|(r, l)| l.parse::<usize>().ok().map(|n| (r, n)))
            && let Some(value) = fields.get(rest)
        {
            let truncated = truncate_at_word(value, len_str);
            let placeholder = format!("{{{placeholder_inner}}}");
            result = result.replacen(&placeholder, &truncated, 1);
            continue;
        }

        search_from = close + 1;
    }

    result
}

/// Strip HTML tags and decode common entities.
fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Truncate a string to at most `max_chars` characters, cutting at a word
/// boundary.  Appends `…` if truncated.
fn truncate_at_word(value: &str, max_chars: usize) -> String {
    if value.len() <= max_chars {
        return value.to_string();
    }

    // Find the last space before the limit for a clean word-boundary cut.
    let cut = value[..max_chars]
        .rfind(' ')
        .unwrap_or(max_chars);

    let mut out = value[..cut].to_string();
    out.push('…');
    out
}

/// Format a numeric string with K/M suffixes.
///
/// Parses the input as `f64` for flexibility (JSON numbers may be floats).
/// Falls back to the raw string on parse failure.
pub fn format_number(value: &str) -> String {
    let Ok(n) = value.parse::<f64>() else {
        return value.to_string();
    };

    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1}K", n / 1_000.0)
    } else if n.fract() == 0.0 && n >= 0.0 {
        // Safe: n is in [0, 1000), whole number — fits in u64 without truncation.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = n as u64;
        format!("{i}")
    } else {
        format!("{n:.1}")
    }
}

/// Return `true` if the string contains an unresolved `{…}` placeholder.
///
/// A placeholder is `{` followed by one or more non-`}` characters followed
/// by `}`.  This ignores literal `{}` (empty braces) and braces inside code
/// blocks because those don't appear in our templates.
fn has_unresolved_placeholder(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(rel) = bytes[i + 1..].iter().position(|&b| b == b'}')
            && rel > 0
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Remove leading and trailing blank lines from a rendered line list.
fn trim_blank_edges(lines: &[String]) -> String {
    let start = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |p| p + 1);

    if start >= end {
        return String::new();
    }

    lines[start..end].join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ── format_number ─────────────────────────────────────────────────────────

    #[test]
    fn format_number_below_thousand() {
        assert_eq!(format_number("999"), "999");
        assert_eq!(format_number("0"), "0");
        assert_eq!(format_number("1"), "1");
    }

    #[test]
    fn format_number_thousands_suffix() {
        assert_eq!(format_number("1000"), "1.0K");
        assert_eq!(format_number("1500"), "1.5K");
        assert_eq!(format_number("8800"), "8.8K");
        assert_eq!(format_number("999999"), "1000.0K");
    }

    #[test]
    fn format_number_millions_suffix() {
        assert_eq!(format_number("1000000"), "1.0M");
        assert_eq!(format_number("3800000"), "3.8M");
        assert_eq!(format_number("10000000"), "10.0M");
    }

    #[test]
    fn format_number_non_numeric_passthrough() {
        assert_eq!(format_number("n/a"), "n/a");
        assert_eq!(format_number(""), "");
    }

    #[test]
    fn format_number_float_input() {
        // JSON numbers that come back as floats
        assert_eq!(format_number("8800.0"), "8.8K");
        assert_eq!(format_number("1000000.0"), "1.0M");
    }

    // ── has_unresolved_placeholder ─────────────────────────────────────────────

    #[test]
    fn unresolved_placeholder_detected() {
        assert!(has_unresolved_placeholder("{missing}"));
        assert!(has_unresolved_placeholder("text {field} text"));
        assert!(has_unresolved_placeholder("📊 {likes|number} likes"));
    }

    #[test]
    fn resolved_string_not_detected() {
        assert!(!has_unresolved_placeholder("all good here"));
        assert!(!has_unresolved_placeholder(""));
        assert!(!has_unresolved_placeholder("100 likes · 50 reposts"));
    }

    #[test]
    fn empty_braces_not_detected() {
        assert!(!has_unresolved_placeholder("{}"));
    }

    // ── render_line ────────────────────────────────────────────────────────────

    #[test]
    fn render_line_substitutes_field() {
        let f = fields(&[("author", "Alice")]);
        let result = render_line("by {author}", &f, "https://example.com").unwrap();
        assert_eq!(result, "by Alice");
    }

    #[test]
    fn render_line_omits_when_field_missing() {
        let f = fields(&[("title", "Test")]);
        assert!(render_line("![img]({thumbnail})", &f, "https://example.com").is_none());
    }

    #[test]
    fn render_line_substitutes_original_url() {
        let f = fields(&[]);
        let result = render_line(
            "[Watch]({original_url})",
            &f,
            "https://youtube.com/watch?v=abc",
        )
        .unwrap();
        assert_eq!(result, "[Watch](https://youtube.com/watch?v=abc)");
    }

    #[test]
    fn render_line_substitutes_number_format() {
        let f = fields(&[("likes", "8800")]);
        let result = render_line("📊 {likes|number} likes", &f, "https://x.com").unwrap();
        assert_eq!(result, "📊 8.8K likes");
    }

    #[test]
    fn render_line_omits_when_number_field_missing() {
        let f = fields(&[]);
        assert!(render_line("{views|number} views", &f, "https://x.com").is_none());
    }

    // ── render (full template) ─────────────────────────────────────────────────

    #[test]
    fn render_full_template_all_fields_present() {
        let f = fields(&[
            ("author_handle", "testuser"),
            ("author_name", "Test User"),
            ("text", "Hello World"),
            ("likes", "100"),
            ("retweets", "10"),
            ("replies", "5"),
            ("views", "1000"),
            ("date", "2025-01-01"),
            ("url", "https://x.com/testuser/status/123"),
        ]);
        let tmpl = "## @{author_handle} ({author_name})\n\n{text}\n\n📊 {likes|number} likes\n🕐 {date}\n\n[View on X]({url})\n";
        let output = render(tmpl, &f, "https://x.com/testuser/status/123");

        assert!(output.contains("## @testuser (Test User)"));
        assert!(output.contains("Hello World"));
        assert!(output.contains("📊 100 likes"));
        assert!(output.contains("🕐 2025-01-01"));
        assert!(output.contains("[View on X](https://x.com/testuser/status/123)"));
    }

    #[test]
    fn render_omits_lines_with_missing_optional_fields() {
        let f = fields(&[("title", "A Video"), ("author", "Bob")]);
        let tmpl =
            "## {title}\n\nby {author}\n\n![thumb]({thumbnail})\n\n[Watch]({original_url})\n";
        let output = render(tmpl, &f, "https://youtube.com/watch?v=xyz");

        assert!(output.contains("## A Video"));
        assert!(output.contains("by Bob"));
        assert!(!output.contains("thumbnail")); // line omitted
        assert!(output.contains("[Watch](https://youtube.com/watch?v=xyz)"));
    }

    #[test]
    fn render_trims_leading_and_trailing_blank_lines() {
        let f = fields(&[("title", "T")]);
        let tmpl = "\n\n## {title}\n\n";
        let output = render(tmpl, &f, "https://example.com");
        assert!(!output.starts_with('\n'));
        assert!(!output.ends_with('\n'));
        assert_eq!(output, "## T");
    }

    #[test]
    fn render_multiple_placeholders_on_one_line() {
        let f = fields(&[("first", "Foo"), ("second", "Bar")]);
        let output = render("{first} and {second}", &f, "https://example.com");
        assert_eq!(output, "Foo and Bar");
    }

    #[test]
    fn render_omits_line_when_one_of_two_placeholders_missing() {
        let f = fields(&[("first", "Foo")]);
        let output = render("{first} and {second}", &f, "https://example.com");
        assert!(output.is_empty());
    }

    #[test]
    fn render_number_format_million() {
        let f = fields(&[("views", "3800000")]);
        let output = render("{views|number} views", &f, "https://x.com");
        assert_eq!(output, "3.8M views");
    }

    #[test]
    fn render_empty_template_returns_empty() {
        let f = fields(&[]);
        let output = render("", &f, "https://example.com");
        assert!(output.is_empty());
    }

    // ── truncate filter ───────────────────────────────────────────────────────

    #[test]
    fn render_truncate_filter_shortens_long_text() {
        let f = fields(&[("body", "The quick brown fox jumps over the lazy dog")]);
        let output = render("{body|truncate:20}", &f, "https://example.com");
        assert_eq!(output, "The quick brown fox…");
    }

    #[test]
    fn render_truncate_filter_preserves_short_text() {
        let f = fields(&[("body", "Short")]);
        let output = render("{body|truncate:100}", &f, "https://example.com");
        assert_eq!(output, "Short");
    }

    #[test]
    fn render_truncate_filter_omits_line_when_field_missing() {
        let f = fields(&[]);
        let output = render("{body|truncate:50}", &f, "https://example.com");
        assert!(output.is_empty());
    }

    #[test]
    fn truncate_at_word_cuts_at_space_boundary() {
        assert_eq!(truncate_at_word("hello world foo", 12), "hello world…");
    }

    #[test]
    fn truncate_at_word_no_truncation_when_short() {
        assert_eq!(truncate_at_word("short", 100), "short");
    }
}
