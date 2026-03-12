//! Schema-guided structured data extraction from HTML.
//!
//! Extracts structured JSON from web pages by matching a user-provided schema
//! against multiple data sources in the page:
//!
//! 1. **JSON-LD** (`<script type="application/ld+json">`) -- highest fidelity
//! 2. **Meta tags** (Open Graph `og:*`, Twitter `twitter:*`, standard `<meta>`)
//! 3. **Microdata** (`itemscope`/`itemprop` attributes)
//! 4. **CSS selectors** (fallback: heuristic matching by field name)
//!
//! # Example
//!
//! ```rust
//! use nab::content::structured::{extract_structured, ExtractionSchema};
//!
//! let schema = ExtractionSchema::from_json(r#"{"title": "string", "price": "number"}"#).unwrap();
//! let html = r#"
//!     <html><head>
//!         <meta property="og:title" content="Widget Pro">
//!         <script type="application/ld+json">
//!         {"@type": "Product", "name": "Widget Pro", "offers": {"price": 29.99}}
//!         </script>
//!     </head><body></body></html>
//! "#;
//! let result = extract_structured(html, &schema);
//! assert_eq!(result.fields["title"].as_str(), Some("Widget Pro"));
//! assert_eq!(result.fields["price"].as_f64(), Some(29.99));
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Type hint for a schema field, guiding coercion of extracted values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Number,
    Boolean,
    Array,
    Object,
}

impl FieldType {
    /// Parse a type hint from a JSON string value.
    fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "number" | "float" | "int" | "integer" | "decimal" => Self::Number,
            "bool" | "boolean" => Self::Boolean,
            "array" | "list" => Self::Array,
            "object" | "map" | "dict" => Self::Object,
            _ => Self::String,
        }
    }
}

/// A single field in the extraction schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaField {
    /// The expected type of this field.
    pub field_type: FieldType,
    /// Optional CSS selector override for extraction.
    pub selector: Option<String>,
    /// Optional attribute to extract from the selected element (default: text content).
    pub attribute: Option<String>,
}

/// User-provided schema describing what to extract from a page.
///
/// Simple form: `{"title": "string", "price": "number"}`.
/// Advanced form: `{"title": {"type": "string", "selector": "h1.product-title"}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionSchema {
    pub fields: HashMap<String, SchemaField>,
}

impl ExtractionSchema {
    /// Parse a schema from a JSON string (simple or advanced form).
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON is invalid or not an object.
    pub fn from_json(json: &str) -> Result<Self, ExtractionError> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| ExtractionError::InvalidSchema(e.to_string()))?;

        let obj = value
            .as_object()
            .ok_or_else(|| ExtractionError::InvalidSchema("schema must be a JSON object".into()))?;

        let mut fields = HashMap::with_capacity(obj.len());

        for (key, val) in obj {
            let field = match val {
                // Simple form: "field_name": "type"
                Value::String(type_str) => SchemaField {
                    field_type: FieldType::from_str_loose(type_str),
                    selector: None,
                    attribute: None,
                },
                // Advanced form: "field_name": {"type": "string", "selector": "..."}
                Value::Object(field_obj) => {
                    let field_type = field_obj
                        .get("type")
                        .and_then(Value::as_str)
                        .map_or(FieldType::String, FieldType::from_str_loose);
                    let selector = field_obj
                        .get("selector")
                        .and_then(Value::as_str)
                        .map(String::from);
                    let attribute = field_obj
                        .get("attribute")
                        .and_then(Value::as_str)
                        .map(String::from);
                    SchemaField {
                        field_type,
                        selector,
                        attribute,
                    }
                }
                _ => {
                    return Err(ExtractionError::InvalidSchema(format!(
                        "field '{key}' must be a type string or object"
                    )));
                }
            };
            fields.insert(key.clone(), field);
        }

        Ok(Self { fields })
    }
}

/// Result of structured data extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    /// Extracted fields matching the schema.
    pub fields: HashMap<String, Value>,
    /// Which data source each field was extracted from.
    pub sources: HashMap<String, DataSource>,
    /// Fields that could not be extracted.
    pub missing: Vec<String>,
}

impl ExtractionResult {
    /// Serialize extracted fields as JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.fields)
    }

    /// Serialize full result (fields + sources + missing) as JSON.
    pub fn to_json_full(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Where a field value was extracted from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataSource {
    JsonLd,
    MetaTag,
    OpenGraph,
    TwitterCard,
    CssSelector,
    Microdata,
}

/// Errors during structured extraction.
#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("invalid schema: {0}")]
    InvalidSchema(String),
}

/// Extract structured data from HTML according to the provided schema.
///
/// Priority: JSON-LD > Open Graph > Twitter Card > meta tags > microdata > CSS heuristics.
/// Values are coerced to the type specified in the schema.
#[must_use]
pub fn extract_structured(html: &str, schema: &ExtractionSchema) -> ExtractionResult {
    let document = scraper::Html::parse_document(html);

    // Pre-extract all data sources
    let jsonld_data = extract_all_jsonld(&document);
    let meta_tags = extract_meta_tags(&document);
    let og_tags = extract_og_tags(&document);
    let twitter_tags = extract_twitter_tags(&document);
    let microdata = extract_microdata(&document);

    let mut fields = HashMap::with_capacity(schema.fields.len());
    let mut sources = HashMap::with_capacity(schema.fields.len());
    let mut missing = Vec::new();

    for (field_name, field_spec) in &schema.fields {
        // If the field has a CSS selector override, try that first
        if let Some(selector) = &field_spec.selector
            && let Some(value) =
                extract_by_css_selector(&document, selector, field_spec.attribute.as_deref())
        {
            let coerced = coerce_value(&value, &field_spec.field_type);
            fields.insert(field_name.clone(), coerced);
            sources.insert(field_name.clone(), DataSource::CssSelector);
            continue;
        }

        // Try data sources in priority order
        if let Some((value, source)) = try_extract_field(
            field_name,
            field_spec,
            &jsonld_data,
            &og_tags,
            &twitter_tags,
            &meta_tags,
            &microdata,
            &document,
        ) {
            fields.insert(field_name.clone(), value);
            sources.insert(field_name.clone(), source);
        } else {
            missing.push(field_name.clone());
        }
    }

    ExtractionResult {
        fields,
        sources,
        missing,
    }
}

/// Extract all JSON-LD blocks from the page, flattened into key-value maps.
fn extract_all_jsonld(document: &scraper::Html) -> Vec<HashMap<String, Value>> {
    let mut results = Vec::new();

    let Some(sel) = scraper::Selector::parse(r#"script[type="application/ld+json"]"#).ok() else {
        return results;
    };

    for script in document.select(&sel) {
        let json_text = script.text().collect::<String>();
        let json_text = json_text.trim();
        if json_text.is_empty() {
            continue;
        }

        // Parse single object or array
        let values: Vec<Value> = if json_text.starts_with('[') {
            serde_json::from_str(json_text).unwrap_or_default()
        } else if json_text.starts_with('{') {
            serde_json::from_str(json_text)
                .ok()
                .map(|v| vec![v])
                .unwrap_or_default()
        } else {
            continue;
        };

        for value in values {
            if let Value::Object(map) = &value {
                // Flatten the top-level object into string keys
                let mut flat = HashMap::new();
                flatten_json_object(map, "", &mut flat);
                results.push(flat);
            }
        }
    }

    results
}

/// Recursively flatten a JSON object into dot-separated key paths.
fn flatten_json_object(
    obj: &serde_json::Map<String, Value>,
    prefix: &str,
    out: &mut HashMap<String, Value>,
) {
    for (key, value) in obj {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };

        if let Value::Object(nested) = value {
            // Store the object itself AND flatten its children
            out.insert(full_key.clone(), value.clone());
            flatten_json_object(nested, &full_key, out);
        } else {
            out.insert(full_key, value.clone());
        }
    }
}

/// Extract Open Graph tags (`<meta property="og:*">`).
fn extract_og_tags(document: &scraper::Html) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    let Some(sel) = scraper::Selector::parse("meta[property]").ok() else {
        return tags;
    };

    for meta in document.select(&sel) {
        if let (Some(property), Some(content)) = (
            meta.value().attr("property"),
            meta.value().attr("content"),
        ) {
            let prop_lower = property.to_lowercase();
            if let Some(key) = prop_lower.strip_prefix("og:") {
                tags.insert(key.to_string(), content.to_string());
            }
        }
    }

    tags
}

/// Extract Twitter Card tags (`<meta name="twitter:*">`).
fn extract_twitter_tags(document: &scraper::Html) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    let Some(sel) = scraper::Selector::parse("meta[name]").ok() else {
        return tags;
    };

    for meta in document.select(&sel) {
        if let (Some(name), Some(content)) = (meta.value().attr("name"), meta.value().attr("content"))
        {
            let name_lower = name.to_lowercase();
            if let Some(key) = name_lower.strip_prefix("twitter:") {
                tags.insert(key.to_string(), content.to_string());
            }
        }
    }

    tags
}

/// Extract standard meta tags (`<meta name="..." content="...">`).
fn extract_meta_tags(document: &scraper::Html) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    let Some(sel) = scraper::Selector::parse("meta[name][content]").ok() else {
        return tags;
    };

    for meta in document.select(&sel) {
        if let (Some(name), Some(content)) = (meta.value().attr("name"), meta.value().attr("content"))
        {
            let name_lower = name.to_lowercase();
            // Skip twitter: prefixed (handled separately)
            if !name_lower.starts_with("twitter:") {
                tags.insert(name_lower, content.to_string());
            }
        }
    }

    tags
}

/// Extract microdata (`itemprop` attributes).
fn extract_microdata(document: &scraper::Html) -> HashMap<String, String> {
    let mut data = HashMap::new();
    let Some(sel) = scraper::Selector::parse("[itemprop]").ok() else {
        return data;
    };

    for elem in document.select(&sel) {
        if let Some(prop) = elem.value().attr("itemprop") {
            let value = elem.value().attr("content").map_or_else(
                || elem.text().collect::<String>().trim().to_string(),
                String::from,
            );

            if !value.is_empty() {
                data.insert(prop.to_lowercase(), value);
            }
        }
    }

    data
}

/// Extract a value by CSS selector.
fn extract_by_css_selector(
    document: &scraper::Html,
    selector: &str,
    attribute: Option<&str>,
) -> Option<Value> {
    let sel = scraper::Selector::parse(selector).ok()?;
    let elem = document.select(&sel).next()?;

    let text = if let Some(attr) = attribute {
        elem.value().attr(attr)?.to_string()
    } else {
        elem.text().collect::<String>().trim().to_string()
    };

    if text.is_empty() {
        return None;
    }

    Some(Value::String(text))
}

/// Known aliases mapping schema field names to data source counterparts.
const FIELD_ALIASES: &[(&str, &[&str])] = &[
    ("title", &["name", "headline", "og:title"]),
    ("name", &["title", "headline"]),
    ("description", &["summary", "abstract", "excerpt"]),
    ("price", &["offers.price", "offers.lowprice", "lowprice"]),
    (
        "image",
        &["image", "thumbnailurl", "thumbnail", "og:image"],
    ),
    ("author", &["author.name", "creator", "author"]),
    (
        "date",
        &[
            "datepublished",
            "datecreated",
            "datemodified",
            "published_time",
        ],
    ),
    (
        "published",
        &[
            "datepublished",
            "datecreated",
            "published_time",
            "article:published_time",
        ],
    ),
    ("rating", &["aggregaterating.ratingvalue", "ratingvalue"]),
    ("url", &["url", "mainentityofpage"]),
    ("brand", &["brand.name", "brand"]),
    ("category", &["category", "articleSection"]),
    ("sku", &["sku", "mpn", "gtin13", "isbn"]),
    ("currency", &["offers.pricecurrency", "pricecurrency"]),
    (
        "availability",
        &["offers.availability", "availability"],
    ),
];

/// Try extracting a field from all data sources in priority order.
#[allow(clippy::too_many_arguments)]
fn try_extract_field(
    field_name: &str,
    field_spec: &SchemaField,
    jsonld_data: &[HashMap<String, Value>],
    og_tags: &HashMap<String, String>,
    twitter_tags: &HashMap<String, String>,
    meta_tags: &HashMap<String, String>,
    microdata: &HashMap<String, String>,
    document: &scraper::Html,
) -> Option<(Value, DataSource)> {
    let field_lower = field_name.to_lowercase();

    // Build list of candidate keys to search for
    let mut candidates: Vec<String> = vec![field_lower.clone()];

    // Add known aliases
    for &(alias_name, alias_keys) in FIELD_ALIASES {
        if alias_name == field_lower {
            candidates.extend(alias_keys.iter().map(|s| s.to_lowercase()));
        }
    }

    // 1. Try JSON-LD (highest priority)
    for jsonld in jsonld_data {
        for candidate in &candidates {
            // Try exact match
            if let Some(val) = jsonld.get(candidate.as_str()) {
                let coerced = coerce_value(&Value::String(value_to_string(val)), &field_spec.field_type);
                return Some((coerced, DataSource::JsonLd));
            }
            // Try case-insensitive match
            for (key, val) in jsonld {
                if key.to_lowercase() == *candidate {
                    let coerced =
                        coerce_value(&Value::String(value_to_string(val)), &field_spec.field_type);
                    return Some((coerced, DataSource::JsonLd));
                }
            }
        }
    }

    // 2. Try Open Graph tags
    for candidate in &candidates {
        if let Some(val) = og_tags.get(candidate.as_str()) {
            let coerced = coerce_value(&Value::String(val.clone()), &field_spec.field_type);
            return Some((coerced, DataSource::OpenGraph));
        }
    }

    // 3. Try Twitter Card tags
    for candidate in &candidates {
        if let Some(val) = twitter_tags.get(candidate.as_str()) {
            let coerced = coerce_value(&Value::String(val.clone()), &field_spec.field_type);
            return Some((coerced, DataSource::TwitterCard));
        }
    }

    // 4. Try standard meta tags
    for candidate in &candidates {
        if let Some(val) = meta_tags.get(candidate.as_str()) {
            let coerced = coerce_value(&Value::String(val.clone()), &field_spec.field_type);
            return Some((coerced, DataSource::MetaTag));
        }
    }

    // 5. Try microdata
    for candidate in &candidates {
        if let Some(val) = microdata.get(candidate.as_str()) {
            let coerced = coerce_value(&Value::String(val.clone()), &field_spec.field_type);
            return Some((coerced, DataSource::Microdata));
        }
    }

    // 6. CSS selector heuristic fallback
    if let Some(value) = heuristic_css_extract(document, &field_lower) {
        let coerced = coerce_value(&value, &field_spec.field_type);
        return Some((coerced, DataSource::CssSelector));
    }

    None
}

/// Heuristic CSS selector extraction based on field name.
fn heuristic_css_extract(document: &scraper::Html, field_name: &str) -> Option<Value> {
    // Map field names to likely CSS selectors
    let selectors: &[&str] = match field_name {
        "title" | "name" | "headline" => &["h1", "[class*='title']", "[class*='headline']"],
        "price" => &[
            "[class*='price']",
            "[data-price]",
            "[itemprop='price']",
        ],
        "description" | "summary" => &[
            "meta[name='description']",
            "[class*='description']",
            "[class*='summary']",
        ],
        "rating" => &["[class*='rating']", "[data-rating]", "[itemprop='ratingValue']"],
        "author" => &["[class*='author']", "[rel='author']", "[itemprop='author']"],
        "date" | "published" => &["time[datetime]", "[class*='date']", "[class*='published']"],
        "image" => &["[class*='product'] img", "article img", "main img"],
        _ => return None,
    };

    for sel_str in selectors {
        let Some(sel) = scraper::Selector::parse(sel_str).ok() else {
            continue;
        };
        if let Some(elem) = document.select(&sel).next() {
            // For meta tags, extract content attribute
            if sel_str.starts_with("meta[") {
                if let Some(content) = elem.value().attr("content")
                    && !content.is_empty()
                {
                    return Some(Value::String(content.to_string()));
                }
                continue;
            }

            // For time elements, prefer datetime attribute
            if elem.value().name() == "time"
                && let Some(dt) = elem.value().attr("datetime")
            {
                return Some(Value::String(dt.to_string()));
            }

            // For images, extract src
            if elem.value().name() == "img" {
                if let Some(src) = elem.value().attr("src") {
                    return Some(Value::String(src.to_string()));
                }
                continue;
            }

            // Check data-* attributes for the field
            let data_attr = format!("data-{field_name}");
            if let Some(val) = elem.value().attr(&data_attr)
                && !val.is_empty()
            {
                return Some(Value::String(val.to_string()));
            }

            // Fall back to text content
            let text = elem.text().collect::<String>().trim().to_string();
            if !text.is_empty() {
                return Some(Value::String(text));
            }
        }
    }

    None
}

/// Convert a JSON value to the specified field type.
fn coerce_value(value: &Value, target_type: &FieldType) -> Value {
    match target_type {
        FieldType::String => match value {
            Value::String(_) => value.clone(),
            _ => Value::String(value_to_string(value)),
        },
        FieldType::Number => {
            let s = value_to_string(value);
            parse_number(&s).unwrap_or(Value::Null)
        }
        FieldType::Boolean => {
            let s = value_to_string(value).to_lowercase();
            match s.as_str() {
                "true" | "1" | "yes" | "on" => Value::Bool(true),
                "false" | "0" | "no" | "off" => Value::Bool(false),
                _ => Value::Null,
            }
        }
        FieldType::Array => {
            match value {
                Value::Array(_) => value.clone(),
                Value::String(s) => {
                    // Try parsing as JSON array
                    if let Ok(arr) = serde_json::from_str::<Vec<Value>>(s) {
                        Value::Array(arr)
                    } else {
                        // Split by comma as fallback
                        let items: Vec<Value> = s
                            .split(',')
                            .map(|item| Value::String(item.trim().to_string()))
                            .collect();
                        Value::Array(items)
                    }
                }
                _ => Value::Array(vec![value.clone()]),
            }
        }
        FieldType::Object => {
            match value {
                Value::Object(_) => value.clone(),
                Value::String(s) => {
                    // Try parsing as JSON object
                    serde_json::from_str(s).unwrap_or(Value::Null)
                }
                _ => Value::Null,
            }
        }
    }
}

/// Extract a string representation from any JSON value.
/// For Schema.org typed objects, extracts `name` instead of serializing the object.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::Array(arr) => {
            // For arrays of strings, join them
            arr.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
        Value::Object(map) => {
            // Schema.org typed objects: extract "name" if present
            if map.contains_key("@type")
                && let Some(Value::String(name)) = map.get("name")
            {
                return name.clone();
            }
            serde_json::to_string(value).unwrap_or_default()
        }
    }
}

/// Parse a number from a string, stripping currency symbols.
fn parse_number(s: &str) -> Option<Value> {
    // Strip currency symbols and whitespace
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();

    if cleaned.is_empty() {
        return None;
    }

    // Try integer first, then float
    if let Ok(n) = cleaned.parse::<i64>() {
        Some(Value::Number(serde_json::Number::from(n)))
    } else if let Ok(n) = cleaned.parse::<f64>() {
        serde_json::Number::from_f64(n).map(Value::Number)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_from_simple_json() {
        let schema = ExtractionSchema::from_json(
            r#"{"title": "string", "price": "number", "active": "boolean"}"#,
        )
        .unwrap();
        assert_eq!(schema.fields.len(), 3);
        assert_eq!(schema.fields["title"].field_type, FieldType::String);
        assert_eq!(schema.fields["price"].field_type, FieldType::Number);
        assert_eq!(schema.fields["active"].field_type, FieldType::Boolean);
    }

    #[test]
    fn schema_from_advanced_json() {
        let schema = ExtractionSchema::from_json(
            r#"{"title": {"type": "string", "selector": "h1.main"}, "price": {"type": "number", "selector": ".price", "attribute": "data-price"}}"#,
        )
        .unwrap();
        assert_eq!(schema.fields["title"].selector.as_deref(), Some("h1.main"));
        assert_eq!(
            schema.fields["price"].attribute.as_deref(),
            Some("data-price")
        );
    }

    #[test]
    fn schema_rejects_invalid_json() {
        assert!(ExtractionSchema::from_json("not json").is_err());
        assert!(ExtractionSchema::from_json("[1, 2]").is_err());
    }

    #[test]
    fn coerce_string_to_number() {
        let val = coerce_value(&Value::String("29.99".into()), &FieldType::Number);
        assert_eq!(val.as_f64(), Some(29.99));
    }

    #[test]
    fn coerce_currency_string_to_number() {
        let val = coerce_value(&Value::String("$29.99".into()), &FieldType::Number);
        assert_eq!(val.as_f64(), Some(29.99));
    }

    #[test]
    fn coerce_string_to_boolean() {
        assert_eq!(
            coerce_value(&Value::String("true".into()), &FieldType::Boolean),
            Value::Bool(true)
        );
        assert_eq!(
            coerce_value(&Value::String("false".into()), &FieldType::Boolean),
            Value::Bool(false)
        );
        assert_eq!(
            coerce_value(&Value::String("1".into()), &FieldType::Boolean),
            Value::Bool(true)
        );
    }

    #[test]
    fn coerce_string_to_array() {
        let val = coerce_value(&Value::String("a, b, c".into()), &FieldType::Array);
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_str(), Some("a"));
        assert_eq!(arr[1].as_str(), Some("b"));
        assert_eq!(arr[2].as_str(), Some("c"));
    }

    #[test]
    fn parse_number_handles_integers() {
        assert_eq!(parse_number("42"), Some(Value::Number(42.into())));
    }

    #[test]
    fn parse_number_handles_floats() {
        assert_eq!(parse_number("3.14").and_then(|v| v.as_f64()), Some(3.14));
    }

    #[test]
    fn parse_number_strips_currency() {
        assert_eq!(
            parse_number("$19.99").and_then(|v| v.as_f64()),
            Some(19.99)
        );
    }
}
