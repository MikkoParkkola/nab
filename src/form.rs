//! Form parsing and submission
//!
//! Smart form POST handling:
//! - Extracts hidden fields (CSRF tokens, session tokens)
//! - Merges user-provided fields with hidden fields
//! - Detects and uses correct encoding (urlencoded or multipart)
//! - Supports form discovery with heuristics

use anyhow::{Context, Result};
use scraper::{Html, Selector};
use std::collections::HashMap;

/// A parsed HTML form
#[derive(Debug, Clone)]
pub struct Form {
    /// Form action URL (may be relative)
    pub action: String,
    /// HTTP method (GET or POST)
    pub method: String,
    /// Encoding type (application/x-www-form-urlencoded or multipart/form-data)
    pub enctype: String,
    /// All form fields (visible and hidden)
    pub fields: HashMap<String, String>,
    /// Hidden fields only
    pub hidden_fields: HashMap<String, String>,
    /// Is this likely a login form?
    pub is_login_form: bool,
}

impl Form {
    /// Parse all forms from HTML
    pub fn parse_all(html: &str) -> Result<Vec<Self>> {
        let document = Html::parse_document(html);
        let form_selector = Selector::parse("form").map_err(|e| anyhow::anyhow!("{:?}", e))?;
        let input_selector = Selector::parse("input").map_err(|e| anyhow::anyhow!("{:?}", e))?;
        let select_selector = Selector::parse("select").map_err(|e| anyhow::anyhow!("{:?}", e))?;
        let textarea_selector =
            Selector::parse("textarea").map_err(|e| anyhow::anyhow!("{:?}", e))?;
        let option_selector = Selector::parse("option").map_err(|e| anyhow::anyhow!("{:?}", e))?;

        let mut forms = Vec::new();

        for form_elem in document.select(&form_selector) {
            let action = form_elem.value().attr("action").unwrap_or("").to_string();
            let method = form_elem
                .value()
                .attr("method")
                .unwrap_or("get")
                .to_uppercase();
            let enctype = form_elem
                .value()
                .attr("enctype")
                .unwrap_or("application/x-www-form-urlencoded")
                .to_string();

            let mut fields = HashMap::new();
            let mut hidden_fields = HashMap::new();
            let mut has_password_field = false;

            // Extract input fields
            for input in form_elem.select(&input_selector) {
                let input_type = input.value().attr("type").unwrap_or("text");
                let name = input.value().attr("name").unwrap_or("");
                let value = input.value().attr("value").unwrap_or("");

                if !name.is_empty() {
                    fields.insert(name.to_string(), value.to_string());

                    if input_type == "hidden" {
                        hidden_fields.insert(name.to_string(), value.to_string());
                    }

                    if input_type == "password" {
                        has_password_field = true;
                    }
                }
            }

            // Extract select fields (use first selected option or first option)
            for select in form_elem.select(&select_selector) {
                let name = select.value().attr("name").unwrap_or("");
                if !name.is_empty() {
                    let mut selected_value = String::new();
                    for option in select.select(&option_selector) {
                        if option.value().attr("selected").is_some() {
                            selected_value = option.value().attr("value").unwrap_or("").to_string();
                            break;
                        }
                        // Fallback to first option
                        if selected_value.is_empty() {
                            selected_value = option.value().attr("value").unwrap_or("").to_string();
                        }
                    }
                    fields.insert(name.to_string(), selected_value);
                }
            }

            // Extract textarea fields
            for textarea in form_elem.select(&textarea_selector) {
                let name = textarea.value().attr("name").unwrap_or("");
                if !name.is_empty() {
                    let value = textarea.text().collect::<String>();
                    fields.insert(name.to_string(), value);
                }
            }

            forms.push(Form {
                action,
                method,
                enctype,
                fields,
                hidden_fields,
                is_login_form: has_password_field,
            });
        }

        Ok(forms)
    }

    /// Find the first login form in HTML
    pub fn find_login_form(html: &str) -> Result<Option<Self>> {
        let forms = Self::parse_all(html)?;
        Ok(forms.into_iter().find(|f| f.is_login_form))
    }

    /// Find a form by action URL pattern
    pub fn find_by_action(html: &str, pattern: &str) -> Result<Option<Self>> {
        let forms = Self::parse_all(html)?;
        Ok(forms.into_iter().find(|f| f.action.contains(pattern)))
    }

    /// Merge user-provided fields into the form
    pub fn merge_fields(&mut self, user_fields: &HashMap<String, String>) {
        for (key, value) in user_fields {
            self.fields.insert(key.clone(), value.clone());
        }
    }

    /// Extract CSRF token from HTML using a CSS selector
    pub fn extract_csrf_token(html: &str, selector: &str) -> Result<Option<String>> {
        let document = Html::parse_document(html);
        let css_selector = Selector::parse(selector).map_err(|e| anyhow::anyhow!("{:?}", e))?;

        if let Some(element) = document.select(&css_selector).next() {
            // Try to get value attribute (for input elements)
            if let Some(value) = element.value().attr("value") {
                return Ok(Some(value.to_string()));
            }
            // Try to get text content (for other elements)
            let text = element.text().collect::<String>().trim().to_string();
            if !text.is_empty() {
                return Ok(Some(text));
            }
        }

        Ok(None)
    }

    /// Encode form data as application/x-www-form-urlencoded
    pub fn encode_urlencoded(&self) -> String {
        let mut pairs: Vec<String> = self
            .fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect();
        pairs.sort(); // Stable encoding for tests
        pairs.join("&")
    }

    /// Get the Content-Type header for this form
    pub fn content_type(&self) -> &str {
        &self.enctype
    }

    /// Resolve action URL against a base URL
    pub fn resolve_action(&self, base_url: &str) -> Result<String> {
        if self.action.is_empty() {
            return Ok(base_url.to_string());
        }

        let base = url::Url::parse(base_url).context("Invalid base URL")?;
        let resolved = base
            .join(&self.action)
            .context("Failed to resolve action URL")?;
        Ok(resolved.to_string())
    }
}

/// Parse field arguments from CLI (e.g., "username=admin")
pub fn parse_field_args(field_args: &[String]) -> Result<HashMap<String, String>> {
    let mut fields = HashMap::new();

    for arg in field_args {
        let parts: Vec<&str> = arg.splitn(2, '=').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid field format: '{}'. Expected 'name=value'", arg);
        }
        fields.insert(parts[0].to_string(), parts[1].to_string());
    }

    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_form() {
        let html = r#"
            <form action="/submit" method="post">
                <input type="text" name="username" value="">
                <input type="password" name="password" value="">
                <input type="hidden" name="csrf" value="abc123">
                <button type="submit">Login</button>
            </form>
        "#;

        let forms = Form::parse_all(html).unwrap();
        assert_eq!(forms.len(), 1);

        let form = &forms[0];
        assert_eq!(form.action, "/submit");
        assert_eq!(form.method, "POST");
        assert!(form.is_login_form);
        assert_eq!(form.fields.get("csrf"), Some(&"abc123".to_string()));
        assert_eq!(form.hidden_fields.get("csrf"), Some(&"abc123".to_string()));
    }

    #[test]
    fn test_find_login_form() {
        let html = r#"
            <form action="/search">
                <input type="text" name="q">
            </form>
            <form action="/login" method="post">
                <input type="text" name="username">
                <input type="password" name="password">
            </form>
        "#;

        let form = Form::find_login_form(html).unwrap();
        assert!(form.is_some());
        assert_eq!(form.unwrap().action, "/login");
    }

    #[test]
    fn test_extract_csrf_token() {
        let html = r#"
            <input type="hidden" name="csrf_token" value="secret123">
        "#;

        let token = Form::extract_csrf_token(html, "input[name=csrf_token]").unwrap();
        assert_eq!(token, Some("secret123".to_string()));
    }

    #[test]
    fn test_encode_urlencoded() {
        let mut form = Form {
            action: "/submit".to_string(),
            method: "POST".to_string(),
            enctype: "application/x-www-form-urlencoded".to_string(),
            fields: HashMap::new(),
            hidden_fields: HashMap::new(),
            is_login_form: false,
        };

        form.fields
            .insert("name".to_string(), "John Doe".to_string());
        form.fields
            .insert("email".to_string(), "john@example.com".to_string());

        let encoded = form.encode_urlencoded();
        // Should be sorted
        assert!(encoded.contains("email=john%40example.com"));
        assert!(encoded.contains("name=John%20Doe"));
    }

    #[test]
    fn test_parse_field_args() {
        let args = vec!["username=admin".to_string(), "password=secret".to_string()];

        let fields = parse_field_args(&args).unwrap();
        assert_eq!(fields.get("username"), Some(&"admin".to_string()));
        assert_eq!(fields.get("password"), Some(&"secret".to_string()));
    }

    #[test]
    fn test_merge_fields() {
        let mut form = Form {
            action: "/submit".to_string(),
            method: "POST".to_string(),
            enctype: "application/x-www-form-urlencoded".to_string(),
            fields: HashMap::from([
                ("csrf".to_string(), "token123".to_string()),
                ("username".to_string(), "".to_string()),
            ]),
            hidden_fields: HashMap::from([("csrf".to_string(), "token123".to_string())]),
            is_login_form: false,
        };

        let user_fields = HashMap::from([
            ("username".to_string(), "admin".to_string()),
            ("password".to_string(), "secret".to_string()),
        ]);

        form.merge_fields(&user_fields);

        assert_eq!(form.fields.get("username"), Some(&"admin".to_string()));
        assert_eq!(form.fields.get("password"), Some(&"secret".to_string()));
        assert_eq!(form.fields.get("csrf"), Some(&"token123".to_string()));
    }
}
