//! Simple JSON dot-path extraction from [`serde_json::Value`].
//!
//! # Syntax
//!
//! | Pattern                    | Meaning                                               |
//! |----------------------------|-------------------------------------------------------|
//! | `.field`                   | Object key lookup                                     |
//! | `.field.nested`            | Chained object key lookup                             |
//! | `.field[]`                 | Collect all elements of an array as strings           |
//! | `.field[].nested`          | Navigate into each array element and collect values   |
//! | `.field[N]`                | Index N of an array at `field` (zero-based)           |
//! | `.field[N].nested`         | Navigate into element N and extract nested field      |
//! | `[N].field`                | Index into a root-level array, then key lookup        |
//! | `[N].field[M].nested`      | Chained numeric index + key lookup                    |
//!
//! Object-key paths start with `.`.  Root-array paths start with `[`.  Any
//! missing key, out-of-bounds index, or type mismatch returns `None` / an
//! empty `Vec`.  Values are converted to strings via their JSON representation
//! (strings are unquoted; numbers and booleans as-is).
//!
//! Object paths support inline array indexing via `key[N]` segments, e.g.
//! `.items[0].title` extracts the `title` field from the first element of
//! the `items` array.
//!
//! Root-array paths are useful for APIs (e.g. Reddit) that return a bare JSON
//! array at the top level rather than a JSON object.
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
//!
//! // Indexed array access within an object path (e.g. Stack Exchange API)
//! let v2 = json!({"items": [{"title": "First"}, {"title": "Second"}]});
//! assert_eq!(extract(&v2, ".items[0].title"), Some("First".to_string()));
//!
//! // Root-level array indexing (e.g. Reddit API response)
//! let arr = json!([{"data": {"title": "Post"}}, {"data": {"title": "Comments"}}]);
//! assert_eq!(extract(&arr, "[0].data.title"), Some("Post".to_string()));
//! assert_eq!(extract(&arr, "[1].data.title"), Some("Comments".to_string()));
//! ```

use serde_json::Value;

/// Extract a scalar value at `path` from `value`.
///
/// Returns `None` if the path is missing, the intermediate node is not an
/// object, or the terminal node is `null`.  Arrays are not scalar and also
/// return `None`; use [`extract_array`] for those.
///
/// Supports both object paths (`.field.nested`) and root-array paths
/// (`[N].field.nested`).
pub fn extract(value: &Value, path: &str) -> Option<String> {
    if path.ends_with("[]") || path.contains("[].") {
        // Array-collect paths are handled by extract_array; scalar call returns None.
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

/// Walk a path expression and return the terminal [`Value`] node.
///
/// Two path forms are supported:
///
/// - **Object path** (starts with `.`): each `.`-separated segment is an
///   object key, optionally followed by `[N]` for indexed array access.
///   Example: `.tweet.author.name`, `.items[0].title`.
/// - **Root-array path** (starts with `[`): one or more `[N]` index steps
///   interspersed with `.key` steps.  Example: `[0].data.children[0].data.title`.
///
/// Array-collect brackets (`[]` with no index) are NOT handled here; callers
/// strip those before calling.
fn walk_path<'v>(value: &'v Value, path: &str) -> Option<&'v Value> {
    if path.starts_with('[') {
        return walk_indexed_path(value, path);
    }

    let path = path.strip_prefix('.')?;
    if path.is_empty() {
        return Some(value);
    }

    let mut current = value;
    for segment in path.split('.') {
        current = walk_segment(current, segment)?;
    }
    Some(current)
}

/// Advance one path segment, handling both plain object keys and `key[N]`
/// indexed array access.
///
/// - `"name"` → plain object key lookup.
/// - `"items[0]"` → object key `"items"` followed by array index `0`.
fn walk_segment<'v>(value: &'v Value, segment: &str) -> Option<&'v Value> {
    if let Some(bracket) = segment.find('[') {
        // Segment has the form `key[N]`; extract key and index.
        let key = &segment[..bracket];
        let rest = segment.get(bracket + 1..)?;
        let index_str = rest.strip_suffix(']')?;
        let index: usize = index_str.parse().ok()?;
        let array = value.as_object()?.get(key)?.as_array()?;
        array.get(index)
    } else {
        value.as_object()?.get(segment)
    }
}

/// Walk a path that may contain `[N]` numeric index steps.
///
/// Parses segments of the form `[N]` (array index) and `key` (object key)
/// from the remaining `path` string.  Handles paths like:
/// - `[0].data.children[0].data.title`
/// - `[1].data`
fn walk_indexed_path<'v>(value: &'v Value, path: &str) -> Option<&'v Value> {
    let mut current = value;
    let mut rest = path;

    while !rest.is_empty() {
        if let Some(after_bracket) = rest.strip_prefix('[') {
            // Parse `N]…`
            let close = after_bracket.find(']')?;
            let idx: usize = after_bracket[..close].parse().ok()?;
            current = current.as_array()?.get(idx)?;
            rest = &after_bracket[close + 1..];
        } else if let Some(after_dot) = rest.strip_prefix('.') {
            // Parse `key` up to the next `.` or `[`
            let key_end = after_dot
                .find(['.', '['])
                .unwrap_or(after_dot.len());
            let key = &after_dot[..key_end];
            if key.is_empty() {
                return None;
            }
            current = current.as_object()?.get(key)?;
            rest = &after_dot[key_end..];
        } else {
            // Unexpected character
            return None;
        }
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

    // ── root-array indexing ([N].field) ────────────────────────────────────────

    #[test]
    fn extract_root_array_first_element_scalar() {
        // GIVEN: bare JSON array (like Reddit API response)
        let v = json!([{"title": "Post"}, {"title": "Comments"}]);
        // WHEN: indexing into first element
        // THEN: scalar value extracted
        assert_eq!(extract(&v, "[0].title"), Some("Post".to_string()));
    }

    #[test]
    fn extract_root_array_second_element_scalar() {
        let v = json!([{"kind": "first"}, {"kind": "second"}]);
        assert_eq!(extract(&v, "[1].kind"), Some("second".to_string()));
    }

    #[test]
    fn extract_root_array_nested_path() {
        // GIVEN: Reddit-style array with nested object structure
        let v = json!([
            {"data": {"children": [{"data": {"title": "Rust is great", "score": 42}}]}}
        ]);
        // WHEN: deep path into first listing's first child
        // THEN: title and score extracted correctly
        assert_eq!(
            extract(&v, "[0].data.children[0].data.title"),
            Some("Rust is great".to_string())
        );
        assert_eq!(
            extract(&v, "[0].data.children[0].data.score"),
            Some("42".to_string())
        );
    }

    #[test]
    fn extract_root_array_out_of_bounds_returns_none() {
        let v = json!([{"title": "only one"}]);
        assert_eq!(extract(&v, "[1].title"), None);
    }

    #[test]
    fn extract_root_array_missing_key_returns_none() {
        let v = json!([{"title": "present"}]);
        assert_eq!(extract(&v, "[0].missing"), None);
    }

    #[test]
    fn extract_root_array_number_field() {
        let v = json!([{"count": 100}]);
        assert_eq!(extract(&v, "[0].count"), Some("100".to_string()));
    }

    #[test]
    fn extract_root_array_boolean_field() {
        let v = json!([{"is_self": true}]);
        assert_eq!(extract(&v, "[0].is_self"), Some("true".to_string()));
    }

    #[test]
    fn extract_reddit_api_response_structure() {
        // GIVEN: a realistic Reddit JSON array response (two listings)
        let v = json!([
            {
                "data": {
                    "children": [{
                        "data": {
                            "title": "Why Rust?",
                            "author": "rustacean",
                            "score": 1500,
                            "num_comments": 200,
                            "selftext": "Because it's awesome.",
                            "url": "https://reddit.com/r/rust/comments/abc123",
                            "subreddit": "rust"
                        }
                    }]
                }
            },
            {"data": {"children": []}}
        ]);
        // WHEN: extracting post fields
        // THEN: all fields extracted correctly
        assert_eq!(
            extract(&v, "[0].data.children[0].data.title"),
            Some("Why Rust?".to_string())
        );
        assert_eq!(
            extract(&v, "[0].data.children[0].data.author"),
            Some("rustacean".to_string())
        );
        assert_eq!(
            extract(&v, "[0].data.children[0].data.score"),
            Some("1500".to_string())
        );
        assert_eq!(
            extract(&v, "[0].data.children[0].data.subreddit"),
            Some("rust".to_string())
        );
    }

    // ── indexed array access [N] within object paths ───────────────────────────

    #[test]
    fn extract_indexed_first_element_scalar() {
        let v = json!({"items": [{"title": "First"}, {"title": "Second"}]});
        assert_eq!(extract(&v, ".items[0].title"), Some("First".to_string()));
    }

    #[test]
    fn extract_indexed_second_element_scalar() {
        let v = json!({"items": [{"title": "First"}, {"title": "Second"}]});
        assert_eq!(extract(&v, ".items[1].title"), Some("Second".to_string()));
    }

    #[test]
    fn extract_indexed_out_of_bounds_returns_none() {
        let v = json!({"items": [{"title": "Only"}]});
        assert_eq!(extract(&v, ".items[1].title"), None);
    }

    #[test]
    fn extract_indexed_nested_deep_path() {
        let v = json!({"items": [{"owner": {"display_name": "Alice"}}]});
        assert_eq!(
            extract(&v, ".items[0].owner.display_name"),
            Some("Alice".to_string())
        );
    }

    #[test]
    fn extract_indexed_numeric_field() {
        let v = json!({"items": [{"score": 42}]});
        assert_eq!(extract(&v, ".items[0].score"), Some("42".to_string()));
    }

    #[test]
    fn extract_indexed_missing_key_returns_none() {
        let v = json!({"items": [{"other": "value"}]});
        assert_eq!(extract(&v, ".items[0].title"), None);
    }

    #[test]
    fn extract_indexed_on_non_array_returns_none() {
        let v = json!({"items": "not an array"});
        assert_eq!(extract(&v, ".items[0].title"), None);
    }
}
