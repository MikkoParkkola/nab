//! JavaScript Engine Module (rquickjs)
//!
//! Provides minimal JavaScript execution for SPA support.
//! Uses `QuickJS` via rquickjs bindings (ES2020, ~1MB).

use anyhow::Result;
use rquickjs::{Context, Function, Runtime, Type};
use tracing::debug;

/// Minimal JavaScript engine for executing scripts
pub struct JsEngine {
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
                r#"
            var div = document.createElement('div');
            div.tagName;
        "#,
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
                r#"
            window.localStorage.setItem('key', 'value');
            window.localStorage.getItem('key');
        "#,
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
                r#"
            var [a, b] = [1, 2];
            a + b;
        "#,
            )
            .unwrap();
        assert_eq!(result, "3");

        // Spread operator
        let result = engine
            .eval(
                r#"
            var arr1 = [1, 2];
            var arr2 = [...arr1, 3, 4];
            arr2.length;
        "#,
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
                r#"
            async function test() {
                return 42;
            }
            typeof test;
        "#,
            )
            .unwrap();
        assert_eq!(result, "function");
    }
}
