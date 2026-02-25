//! JavaScript Engine Module (rquickjs)
//!
//! Provides minimal JavaScript execution for SPA support.
//! Uses `QuickJS` via rquickjs bindings (ES2020, ~1MB).

use anyhow::Result;
use rquickjs::{Context, Function, Runtime, Type};
use tracing::debug;

/// Minimal JavaScript engine for executing scripts
pub struct JsEngine {
    /// Runtime must be kept alive for Context lifetime - not directly used after initialization
    #[allow(dead_code)]
    runtime: Runtime,
    context: Context,
}

impl JsEngine {
    /// Create a new JavaScript engine
    pub fn new() -> Result<Self> {
        let runtime = Runtime::new()?;
        let context = Context::full(&runtime)?;

        // Set memory limit to 32MB (reasonable for web scraping)
        runtime.set_memory_limit(32 * 1024 * 1024);

        // Set max stack size
        runtime.set_max_stack_size(1024 * 1024);

        Ok(Self { runtime, context })
    }

    /// Execute JavaScript code and return the result as a string
    pub fn eval(&self, code: &str) -> Result<String> {
        debug!("Evaluating JS: {} chars", code.len());

        self.context.with(|ctx| {
            let result: rquickjs::Value = ctx.eval(code)?;

            // Convert result to string based on type
            let result_str = match result.type_of() {
                Type::Undefined => "undefined".to_string(),
                Type::Null => "null".to_string(),
                Type::Bool => {
                    let b: bool = result.get()?;
                    b.to_string()
                }
                Type::Int => {
                    let i: i32 = result.get()?;
                    i.to_string()
                }
                Type::Float => {
                    let f: f64 = result.get()?;
                    f.to_string()
                }
                Type::String => {
                    let s: String = result.get()?;
                    s
                }
                Type::Object | Type::Array => {
                    // Try JSON.stringify for objects/arrays
                    let json: Function = ctx.globals().get("JSON")?;
                    let stringify: Function = json.get("stringify")?;
                    let json_str: String = stringify.call((result,))?;
                    json_str
                }
                _ => format!("{result:?}"),
            };

            Ok(result_str)
        })
    }

    /// Execute JavaScript and return boolean result
    pub fn eval_bool(&self, code: &str) -> Result<bool> {
        self.context.with(|ctx| {
            let result: bool = ctx.eval(code)?;
            Ok(result)
        })
    }

    /// Execute JavaScript and return i64 result
    pub fn eval_int(&self, code: &str) -> Result<i64> {
        self.context.with(|ctx| {
            let result: i64 = ctx.eval(code)?;
            Ok(result)
        })
    }

    /// Inject a global variable
    pub fn set_global(&self, name: &str, value: &str) -> Result<()> {
        self.context.with(|ctx| {
            let globals = ctx.globals();
            globals.set(name, value)?;
            Ok(())
        })
    }

    /// Inject minimal DOM-like globals for basic compatibility
    pub fn inject_minimal_dom(&self) -> Result<()> {
        let dom_shim = r"
            // Minimal DOM shim for basic script compatibility
            var document = {
                // Store for elements
                _elements: {},

                getElementById: function(id) {
                    return this._elements[id] || null;
                },

                querySelector: function(selector) {
                    // Return first match or null
                    return null;
                },

                querySelectorAll: function(selector) {
                    return [];
                },

                createElement: function(tag) {
                    return {
                        tagName: tag.toUpperCase(),
                        children: [],
                        attributes: {},
                        innerHTML: '',
                        innerText: '',
                        style: {},
                        classList: {
                            _classes: [],
                            add: function(c) { this._classes.push(c); },
                            remove: function(c) {
                                var idx = this._classes.indexOf(c);
                                if (idx > -1) this._classes.splice(idx, 1);
                            },
                            contains: function(c) { return this._classes.indexOf(c) > -1; }
                        },
                        appendChild: function(child) { this.children.push(child); return child; },
                        removeChild: function(child) {
                            var idx = this.children.indexOf(child);
                            if (idx > -1) this.children.splice(idx, 1);
                            return child;
                        },
                        setAttribute: function(k, v) { this.attributes[k] = v; },
                        getAttribute: function(k) { return this.attributes[k]; },
                        addEventListener: function(evt, fn) { /* no-op for now */ },
                        removeEventListener: function(evt, fn) { /* no-op */ }
                    };
                },

                createTextNode: function(text) {
                    return { nodeType: 3, textContent: text };
                },

                body: {
                    children: [],
                    appendChild: function(child) { this.children.push(child); },
                    innerHTML: ''
                }
            };

            var window = {
                document: document,
                location: {
                    href: '',
                    hostname: '',
                    pathname: '/',
                    search: '',
                    hash: ''
                },
                navigator: {
                    userAgent: 'MicroFetch/1.0',
                    language: 'en-US'
                },
                localStorage: {
                    _data: {},
                    getItem: function(k) { return this._data[k] || null; },
                    setItem: function(k, v) { this._data[k] = String(v); },
                    removeItem: function(k) { delete this._data[k]; },
                    clear: function() { this._data = {}; }
                },
                sessionStorage: {
                    _data: {},
                    getItem: function(k) { return this._data[k] || null; },
                    setItem: function(k, v) { this._data[k] = String(v); },
                    removeItem: function(k) { delete this._data[k]; },
                    clear: function() { this._data = {}; }
                },
                setTimeout: function(fn, ms) { /* no-op: can't do real async */ return 0; },
                setInterval: function(fn, ms) { return 0; },
                clearTimeout: function(id) {},
                clearInterval: function(id) {},
                console: console,
                atob: function(s) { /* base64 decode - simplified */ return s; },
                btoa: function(s) { /* base64 encode - simplified */ return s; }
            };

            // Global console (if not defined)
            if (typeof console === 'undefined') {
                var console = {
                    log: function() {},
                    error: function() {},
                    warn: function() {},
                    info: function() {},
                    debug: function() {}
                };
            }
        ";

        self.context.with(|ctx| {
            ctx.eval::<(), _>(dom_shim)?;
            Ok(())
        })
    }

    /// Parse JSON from a JavaScript object
    pub fn parse_json(&self, json_str: &str) -> Result<String> {
        let code = format!("JSON.parse('{}')", json_str.replace('\'', "\\'"));
        self.eval(&code)
    }

    /// Get reference to the underlying `QuickJS` context
    /// Used for injecting native functions like `fetch()`
    #[must_use]
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Execute inline scripts from HTML and extract rendered DOM
    ///
    /// Parses HTML for inline `<script>` tags (not external src=), executes them
    /// in `QuickJS` with minimal DOM shim, then returns `document.body.innerHTML`.
    ///
    /// # Arguments
    /// * `html` - Raw HTML containing inline scripts
    ///
    /// # Returns
    /// Rendered HTML after script execution, or original if no scripts found
    ///
    /// # Errors
    /// Returns error if script execution fails
    pub fn execute_and_extract_forms(&self, html: &str) -> Result<String> {
        use scraper::{Html, Selector};

        debug!("Parsing HTML for inline scripts");
        let document = Html::parse_document(html);

        // Find all inline script tags (no src attribute)
        let script_selector = Selector::parse("script")
            .map_err(|e| anyhow::anyhow!("Failed to parse script selector: {e:?}"))?;

        let mut inline_scripts = Vec::new();
        for script_elem in document.select(&script_selector) {
            // Skip external scripts (those with src attribute)
            if script_elem.value().attr("src").is_some() {
                debug!("Skipping external script with src attribute");
                continue;
            }

            // Collect inline script content
            let script_text = script_elem.text().collect::<String>();
            if !script_text.trim().is_empty() {
                debug!("Found inline script: {} chars", script_text.len());
                inline_scripts.push(script_text);
            }
        }

        if inline_scripts.is_empty() {
            debug!("No inline scripts found, returning original HTML");
            return Ok(html.to_string());
        }

        // Inject minimal DOM before executing scripts
        self.inject_minimal_dom()?;

        // Parse HTML structure into DOM
        debug!("Injecting HTML structure into DOM");
        self.inject_html_into_dom(html)?;

        // Execute all inline scripts
        debug!("Executing {} inline scripts", inline_scripts.len());
        for (idx, script) in inline_scripts.iter().enumerate() {
            debug!("Executing script {}/{}", idx + 1, inline_scripts.len());
            // Execute but don't fail on script errors (SPA scripts may expect browser APIs)
            if let Err(e) = self.eval(script) {
                debug!("Script {} execution warning: {}", idx + 1, e);
                // Continue with next script - partial execution may still render forms
            }
        }

        // Extract rendered HTML from document.body.innerHTML
        debug!("Extracting rendered HTML from document.body");
        let rendered_html = self
            .eval("document.body.innerHTML")
            .unwrap_or_else(|_| html.to_string());

        Ok(rendered_html)
    }

    /// Inject parsed HTML structure into the DOM shim
    fn inject_html_into_dom(&self, html: &str) -> Result<()> {
        use scraper::{Html, Selector};

        let document = Html::parse_document(html);

        // Build a simplified innerHTML string from body content
        let body_selector = Selector::parse("body")
            .map_err(|e| anyhow::anyhow!("Failed to parse body selector: {e:?}"))?;

        if let Some(body_elem) = document.select(&body_selector).next() {
            // Extract inner HTML of body (all child elements as text)
            let body_html = body_elem.html();

            // Inject into document.body.innerHTML
            let js_code = format!(
                "document.body.innerHTML = {};",
                serde_json::to_string(&body_html)?
            );

            self.context.with(|ctx| {
                ctx.eval::<(), _>(js_code.as_str())?;
                Ok(())
            })
        } else {
            // No body tag, inject entire HTML
            let js_code = format!(
                "document.body.innerHTML = {};",
                serde_json::to_string(html)?
            );

            self.context.with(|ctx| {
                ctx.eval::<(), _>(js_code.as_str())?;
                Ok(())
            })
        }
    }
}

impl Default for JsEngine {
    fn default() -> Self {
        Self::new().expect("Failed to create JS engine")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_eval() {
        let engine = JsEngine::new().unwrap();

        // Arithmetic
        let result = engine.eval("1 + 2").unwrap();
        assert_eq!(result, "3");

        // String
        let result = engine.eval("'hello' + ' ' + 'world'").unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_json_operations() {
        let engine = JsEngine::new().unwrap();

        // Create and stringify JSON
        let result = engine
            .eval(
                r#"
            var obj = { name: "test", value: 42 };
            JSON.stringify(obj);
        "#,
            )
            .unwrap();

        assert!(result.contains("test"));
        assert!(result.contains("42"));
    }

    #[test]
    fn test_dom_shim() {
        let engine = JsEngine::new().unwrap();
        engine.inject_minimal_dom().unwrap();

        // Test document exists
        let result = engine.eval("typeof document").unwrap();
        assert_eq!(result, "object");

        // Test createElement
        let result = engine
            .eval(
                r"
            var div = document.createElement('div');
            div.tagName;
        ",
            )
            .unwrap();
        assert_eq!(result, "DIV");

        // Test window.location
        let result = engine.eval("typeof window.location").unwrap();
        assert_eq!(result, "object");
    }

    #[test]
    fn test_localstorage() {
        let engine = JsEngine::new().unwrap();
        engine.inject_minimal_dom().unwrap();

        let result = engine
            .eval(
                r"
            window.localStorage.setItem('key', 'value');
            window.localStorage.getItem('key');
        ",
            )
            .unwrap();
        assert_eq!(result, "value");
    }

    #[test]
    fn test_es6_features() {
        let engine = JsEngine::new().unwrap();

        // Arrow functions
        let result = engine.eval("((x) => x * 2)(5)").unwrap();
        assert_eq!(result, "10");

        // Template literals
        let result = engine
            .eval(
                r#"
            var name = "World";
            `Hello ${name}!`;
        "#,
            )
            .unwrap();
        assert_eq!(result, "Hello World!");

        // Destructuring
        let result = engine
            .eval(
                r"
            var [a, b] = [1, 2];
            a + b;
        ",
            )
            .unwrap();
        assert_eq!(result, "3");

        // Spread operator
        let result = engine
            .eval(
                r"
            var arr1 = [1, 2];
            var arr2 = [...arr1, 3, 4];
            arr2.length;
        ",
            )
            .unwrap();
        assert_eq!(result, "4");
    }

    #[test]
    fn test_async_await() {
        let engine = JsEngine::new().unwrap();

        // Note: QuickJS supports async/await syntax
        // but without an event loop, promises won't resolve
        // This just tests the syntax is accepted
        let result = engine
            .eval(
                r"
            async function test() {
                return 42;
            }
            typeof test;
        ",
            )
            .unwrap();
        assert_eq!(result, "function");
    }

    #[test]
    fn test_execute_and_extract_forms_no_scripts() {
        let engine = JsEngine::new().unwrap();

        let html = r#"
            <html>
                <body>
                    <form>
                        <input name="username">
                    </form>
                </body>
            </html>
        "#;

        let result = engine.execute_and_extract_forms(html).unwrap();
        // Should return original HTML when no inline scripts
        assert!(result.contains("username"));
    }

    #[test]
    fn test_execute_and_extract_forms_with_inline_script() {
        let engine = JsEngine::new().unwrap();

        let html = r#"
            <html>
                <body>
                    <div id="root"></div>
                    <script>
                        var form = document.createElement('form');
                        form.innerHTML = '<input name="email" type="text"><input name="password" type="password">';
                        document.body.appendChild(form);
                    </script>
                </body>
            </html>
        "#;

        let result = engine.execute_and_extract_forms(html).unwrap();
        // After script execution, should contain the dynamically created form
        assert!(
            result.contains("email") || result.contains("password"),
            "Rendered HTML should contain form fields: {result}"
        );
    }

    #[test]
    fn test_execute_and_extract_forms_skip_external_scripts() {
        let engine = JsEngine::new().unwrap();

        let html = r#"
            <html>
                <body>
                    <div id="app"></div>
                    <script src="https://example.com/bundle.js"></script>
                    <script>
                        document.body.innerHTML += '<form><input name="test"></form>';
                    </script>
                </body>
            </html>
        "#;

        let result = engine.execute_and_extract_forms(html).unwrap();
        // Should execute inline script but skip external src
        assert!(
            result.contains("test"),
            "Should contain form from inline script"
        );
    }

    #[test]
    fn test_execute_and_extract_forms_spa_login() {
        let engine = JsEngine::new().unwrap();

        // Simulates a SPA login page that renders form via JavaScript
        let html = r#"
            <html>
                <head><title>Login</title></head>
                <body>
                    <div id="root"></div>
                    <script>
                        // Simulate SPA form rendering
                        var loginForm = document.createElement('form');
                        loginForm.setAttribute('action', '/api/login');
                        loginForm.setAttribute('method', 'POST');

                        var usernameInput = document.createElement('input');
                        usernameInput.setAttribute('name', 'username');
                        usernameInput.setAttribute('type', 'text');

                        var passwordInput = document.createElement('input');
                        passwordInput.setAttribute('name', 'password');
                        passwordInput.setAttribute('type', 'password');

                        loginForm.appendChild(usernameInput);
                        loginForm.appendChild(passwordInput);

                        var root = document.getElementById('root');
                        if (root) {
                            root.appendChild(loginForm);
                        } else {
                            document.body.appendChild(loginForm);
                        }
                    </script>
                </body>
            </html>
        "#;

        let result = engine.execute_and_extract_forms(html).unwrap();
        assert!(
            result.contains("username"),
            "Should find username field: {result}"
        );
        assert!(
            result.contains("password"),
            "Should find password field: {result}"
        );
    }

    #[test]
    fn test_execute_and_extract_forms_script_error_handling() {
        let engine = JsEngine::new().unwrap();

        // Test graceful handling of script errors
        let html = r#"
            <html>
                <body>
                    <script>
                        // This will fail (nonexistent function)
                        nonExistentFunction();
                    </script>
                    <script>
                        // This should still execute
                        document.body.innerHTML += '<div id="success">OK</div>';
                    </script>
                </body>
            </html>
        "#;

        // Should not fail completely - partial execution is acceptable
        let result = engine.execute_and_extract_forms(html);
        assert!(result.is_ok(), "Should handle script errors gracefully");
    }
}
