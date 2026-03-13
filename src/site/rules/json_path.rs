//! Simple JSON dot-path extraction from [`serde_json::Value`].
//!
//! # Syntax
//!
//! | Pattern                | Meaning                                               |
//! |------------------------|-------------------------------------------------------|
//! | `.field`               | Object key lookup                                     |
//! | `.field.nested`        | Chained object key lookup                             |
//! | `.field[]`             | Collect all elements of an array as strings           |
//! | `.field[].nested`      | Navigate into each array element and collect values   |
//!
//! Paths always start with `.`.  Any missing key or type mismatch returns
//! `None` / an empty `Vec`.  Values are converted to strings via their JSON
//! representation (strings are unquoted; numbers, booleans as-is).
//!
//! # Examples
//!
//! ```rust
//! use serde_json::json;
//! use nab::site::rules::json_path::extract;
//!
//! let v = json!({"tweet": {"text": "hello", "likes": 42}});
//! assert_eq!(extract(&v, ".tweet.text"), Some("hello".to_string()));
//! assert_eq!(extract(&v, ".tweet.likes"), Some("42".to_string()));
//! assert_eq!(extract(&v, ".tweet.missing"), None);
//! ```

use serde_json::Value;

/// Extract a scalar value at `path` from `value`.
///
/// Returns `None` if the path is missing, the intermediate node is not an
/// object, or the terminal node is `null`.  Arrays are not scalar and also
/// return `None`; use [`extract_array`] for those.
pub fn extract(value: &Value, path: &str) -> Option<String> {
    if path.ends_with("[]") || path.contains("[].") {
        // Array paths are handled by extract_array; scalar call returns None.
        return None;
    }
    let node = walk_path(value, path)?;
    value_to_string(node)
}

/// Extract all string values matched by an array path.
///
/// Supports two forms:
/// - `.field[]`          — collect every element of the array at `field`
/// - `.field[].nested`   — collect `.nested` from each array element
///
/// Returns an empty `Vec` if the path is not an array path, the array is
/// missing, or no elements match.
pub fn extract_array(value: &Value, path: &str) -> Vec<String> {
    if let Some(nested_path) = path.strip_suffix("[]") {
        // `.field[]` — collect all array elements
        return walk_path(value, nested_path)
            .map_or_else(Vec::new, |v| collect_array(v, None));
    }

    // `.field[].nested` form
    if let Some(bracket_pos) = path.find("[].") {
        let array_path = &path[..bracket_pos];
        let nested = &path[bracket_pos + 3..]; // skip "[]."
        return walk_path(value, array_path)
            .map_or_else(Vec::new, |v| collect_array(v, Some(nested)));
    }

    Vec::new()
}

/// Walk a dot-path expression and return the terminal [`Value`] node.
///
/// The path must start with `.` and use `.` as the separator.  Array brackets
/// (`[]`) are NOT handled here; callers strip them before calling.
fn walk_path<'v>(value: &'v Value, path: &str) -> Option<&'v Value> {
    let path = path.strip_prefix('.')?;
    if path.is_empty() {
        return Some(value);
    }

    let mut current = value;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// Collect elements of a JSON array as strings.
///
/// If `nested` is `Some("field")`, each element is treated as an object and
/// the `field` key is extracted.  Otherwise each element itself is converted.
fn collect_array(array_val: &Value, nested: Option<&str>) -> Vec<String> {
    let Some(arr) = array_val.as_array() else { return Vec::new() };

    arr.iter()
        .filter_map(|elem| match nested {
            Some(key) => elem.as_object()?.get(key).and_then(value_to_string),
            None => value_to_string(elem),
        })
        .collect()
}

/// Convert a JSON [`Value`] to a plain string.
///
/// - Strings are unquoted.
/// - Numbers and booleans use their natural representation.
/// - `null` and arrays/objects return `None`.
fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── extract (scalar) ─────────────────────────────────────────────────────

    #[test]
    fn extract_top_level_string_field() {
        let v = json!({"title": "Hello World"});
        assert_eq!(extract(&v, ".title"), Some("Hello World".to_string()));
    }

    #[test]
    fn extract_top_level_number_field() {
        let v = json!({"count": 42});
        assert_eq!(extract(&v, ".count"), Some("42".to_string()));
    }

    #[test]
    fn extract_top_level_bool_field() {
        let v = json!({"active": true});
        assert_eq!(extract(&v, ".active"), Some("true".to_string()));
    }

    #[test]
    fn extract_nested_field() {
        let v = json!({"tweet": {"author": {"name": "Alice"}}});
        assert_eq!(extract(&v, ".tweet.author.name"), Some("Alice".to_string()));
    }

    #[test]
    fn extract_missing_field_returns_none() {
        let v = json!({"tweet": {"text": "hi"}});
        assert_eq!(extract(&v, ".tweet.missing"), None);
    }

    #[test]
    fn extract_missing_nested_returns_none() {
        let v = json!({"a": {}});
        assert_eq!(extract(&v, ".a.b.c"), None);
    }

    #[test]
    fn extract_null_field_returns_none() {
        let v = json!({"field": null});
        assert_eq!(extract(&v, ".field"), None);
    }

    #[test]
    fn extract_object_field_returns_none() {
        let v = json!({"obj": {"key": "val"}});
        assert_eq!(extract(&v, ".obj"), None);
    }

    #[test]
    fn extract_array_path_returns_none_for_scalar_call() {
        let v = json!({"items": ["a", "b"]});
        assert_eq!(extract(&v, ".items[]"), None);
        assert_eq!(extract(&v, ".items[].name"), None);
    }

    #[test]
    fn extract_wikipedia_nested_optional_path() {
        let v = json!({
            "content_urls": {
                "desktop": {
                    "page": "https://en.wikipedia.org/wiki/Rust"
                }
            }
        });
        assert_eq!(
            extract(&v, ".content_urls.desktop.page"),
            Some("https://en.wikipedia.org/wiki/Rust".to_string())
        );
    }

    #[test]
    fn extract_wikipedia_nested_missing_intermediate() {
        let v = json!({"content_urls": {}});
        assert_eq!(extract(&v, ".content_urls.desktop.page"), None);
    }

    // ── extract_array ─────────────────────────────────────────────────────────

    #[test]
    fn extract_array_collects_string_elements() {
        let v = json!({"tags": ["rust", "systems", "programming"]});
        assert_eq!(
            extract_array(&v, ".tags[]"),
            vec!["rust", "systems", "programming"]
        );
    }

    #[test]
    fn extract_array_collects_number_elements() {
        let v = json!({"ids": [1, 2, 3]});
        assert_eq!(extract_array(&v, ".ids[]"), vec!["1", "2", "3"]);
    }

    #[test]
    fn extract_array_nested_key_from_objects() {
        let v = json!({
            "media": {
                "all": [
                    {"url": "https://example.com/img1.jpg", "type": "photo"},
                    {"url": "https://example.com/img2.jpg", "type": "photo"}
                ]
            }
        });
        assert_eq!(
            extract_array(&v, ".media.all[].url"),
            vec!["https://example.com/img1.jpg", "https://example.com/img2.jpg"]
        );
    }

    #[test]
    fn extract_array_skips_elements_missing_nested_key() {
        let v = json!({
            "items": [
                {"url": "https://a.com"},
                {"other": "no url here"},
                {"url": "https://b.com"}
            ]
        });
        assert_eq!(
            extract_array(&v, ".items[].url"),
            vec!["https://a.com", "https://b.com"]
        );
    }

    #[test]
    fn extract_array_empty_array_returns_empty() {
        let v = json!({"items": []});
        assert_eq!(extract_array(&v, ".items[]"), Vec::<String>::new());
    }

    #[test]
    fn extract_array_missing_field_returns_empty() {
        let v = json!({});
        assert_eq!(extract_array(&v, ".missing[]"), Vec::<String>::new());
    }

    #[test]
    fn extract_array_non_array_path_returns_empty() {
        let v = json!({"title": "not an array"});
        assert_eq!(extract_array(&v, ".title[]"), Vec::<String>::new());
    }

    #[test]
    fn extract_array_non_array_path_returns_empty_for_plain_path() {
        // A path with no [] at all returns empty from extract_array
        let v = json!({"field": "value"});
        assert_eq!(extract_array(&v, ".field"), Vec::<String>::new());
    }

    #[test]
    fn extract_array_null_elements_are_skipped() {
        let v = json!({"items": [null, "a", null, "b"]});
        assert_eq!(extract_array(&v, ".items[]"), vec!["a", "b"]);
    }
}
