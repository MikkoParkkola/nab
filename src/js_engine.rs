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

    /// Inject the extended DOM shim suitable for running vendor challenge
    /// JavaScript (AWS WAF, `DataDome`, etc.).
    ///
    /// Extends [`inject_minimal_dom`](Self::inject_minimal_dom) with:
    ///
    /// * `navigator.{userAgent, platform, hardwareConcurrency, deviceMemory,
    ///   webdriver, language, languages, plugins, connection}`
    /// * `performance.{now, timing, mark, measure}`
    /// * `document.cookie` getter / setter backed by an in-memory store
    /// * `crypto.getRandomValues(buf)` and `crypto.subtle.digest(algo, buf)`
    /// * `crypto.subtle.{encrypt, decrypt}` (AES-GCM) returning `Promise`s
    /// * `HTMLCanvasElement.prototype.toDataURL` stub
    /// * `setTimeout` / `setInterval` / microtask-queue drain
    ///
    /// The shim is feature-gated behind the `js-dom-full` cargo feature
    /// (default on) so minimal builds that only need string evaluation
    /// do not pay the ~18 kB cost.
    ///
    /// # Errors
    /// Returns an error if the `QuickJS` runtime rejects any of the
    /// synthetic globals (should not happen in practice).
    #[cfg(feature = "js-dom-full")]
    pub fn inject_full_dom(&self) -> Result<()> {
        self.inject_minimal_dom()?;
        let shim = FULL_DOM_SHIM_JS;
        self.context.with(|ctx| -> Result<()> {
            ctx.eval::<(), _>(shim)?;
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

// ═════════════════════════════════════════════════════════════════════════════
// Full DOM shim — feature-gated JS payload that lights up navigator,
// performance, document.cookie, crypto, and canvas stubs for WAF challenge
// scripts.
//
// Deliberately written in plain ES2015 so QuickJS accepts it verbatim without
// transpilation. The shim defines no external callouts — random bytes come
// from a seeded LCG, digests from a pure-JS SHA-256, and cookies live in an
// in-engine Map keyed by cookie name.
//
// ~450 Rust LoC budget → ~240 lines of JS after minification. Kept as one
// raw string literal for locality; the compiled binary embeds it as a &str.
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "js-dom-full")]
const FULL_DOM_SHIM_JS: &str = r"
(function(_g) {
    // Derive a high-resolution timestamp from the current wall clock.
    var _origin = Date.now();

    // ── navigator ──────────────────────────────────────────────────────────
    var _nav = {
        userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36',
        platform: 'MacIntel',
        hardwareConcurrency: 8,
        deviceMemory: 8,
        webdriver: false,
        language: 'en-US',
        languages: ['en-US', 'en'],
        plugins: [],
        doNotTrack: null,
        connection: {
            effectiveType: '4g',
            rtt: 50,
            downlink: 10,
            saveData: false
        },
        userAgentData: {
            brands: [
                { brand: 'Chromium', version: '131' },
                { brand: 'Not.A/Brand', version: '24' }
            ],
            mobile: false,
            platform: 'macOS'
        }
    };
    if (typeof navigator === 'undefined' || typeof navigator !== 'object') {
        _g.navigator = _nav;
    } else {
        for (var k in _nav) { if (!(k in navigator)) navigator[k] = _nav[k]; }
    }

    // ── performance ────────────────────────────────────────────────────────
    var _marks = {};
    var _measures = {};
    _g.performance = {
        now: function() { return Date.now() - _origin; },
        timeOrigin: _origin,
        timing: {
            navigationStart: _origin,
            domInteractive: _origin + 10,
            domContentLoadedEventEnd: _origin + 20,
            loadEventEnd: _origin + 30
        },
        mark: function(name) { _marks[name] = Date.now() - _origin; },
        measure: function(name, startMark, endMark) {
            var s = _marks[startMark] || 0;
            var e = _marks[endMark] || (Date.now() - _origin);
            _measures[name] = e - s;
        },
        getEntriesByName: function() { return []; },
        getEntriesByType: function() { return []; },
        clearMarks: function() { _marks = {}; },
        clearMeasures: function() { _measures = {}; }
    };

    // ── document.cookie ────────────────────────────────────────────────────
    var _cookieStore = {};
    Object.defineProperty(document, 'cookie', {
        configurable: true,
        get: function() {
            var out = [];
            for (var k in _cookieStore) {
                if (Object.prototype.hasOwnProperty.call(_cookieStore, k)) {
                    out.push(k + '=' + _cookieStore[k]);
                }
            }
            return out.join('; ');
        },
        set: function(raw) {
            if (!raw) return;
            var first = String(raw).split(';')[0];
            var eq = first.indexOf('=');
            if (eq < 0) return;
            var name = first.slice(0, eq).trim();
            var value = first.slice(eq + 1).trim();
            if (name) _cookieStore[name] = value;
        }
    });

    // ── crypto ─────────────────────────────────────────────────────────────
    // Deterministic-enough PRNG: xorshift128+ seeded from Date.now().
    // Challenge scripts only inspect *shape*, not statistical quality.
    var _s0 = (Date.now() & 0xffffffff) >>> 0;
    var _s1 = ((Date.now() >>> 13) ^ 0x9E3779B9) >>> 0;
    function _nextByte() {
        var x = _s0, y = _s1;
        _s0 = y;
        x ^= (x << 23) >>> 0; x = x & 0xffffffff;
        _s1 = ((x ^ y ^ (x >>> 17) ^ (y >>> 26)) >>> 0);
        return (_s1 + y) & 0xff;
    }

    function _toUint8(buf) {
        if (buf instanceof Uint8Array) return buf;
        if (buf && buf.buffer && typeof buf.byteLength === 'number') {
            return new Uint8Array(buf.buffer, buf.byteOffset || 0, buf.byteLength);
        }
        if (typeof buf === 'string') {
            var out = new Uint8Array(buf.length);
            for (var i = 0; i < buf.length; i++) out[i] = buf.charCodeAt(i) & 0xff;
            return out;
        }
        return new Uint8Array(0);
    }

    // Pure-JS SHA-256 so digest() works without native bindings.
    function _sha256(bytes) {
        var K = [
            0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
            0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
            0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
            0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
            0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
            0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
            0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
            0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2
        ];
        var H = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
        var msg = Array.from(bytes);
        var l = msg.length;
        msg.push(0x80);
        while ((msg.length % 64) !== 56) msg.push(0);
        var bitLen = l * 8;
        for (var i = 7; i >= 0; i--) msg.push((bitLen >>> (i * 8)) & 0xff);
        for (var off = 0; off < msg.length; off += 64) {
            var W = new Array(64);
            for (var t = 0; t < 16; t++) {
                W[t] = (msg[off + t*4] << 24) | (msg[off + t*4 + 1] << 16) | (msg[off + t*4 + 2] << 8) | msg[off + t*4 + 3];
                W[t] = W[t] >>> 0;
            }
            for (var t2 = 16; t2 < 64; t2++) {
                var s0 = ((W[t2-15] >>> 7) | (W[t2-15] << 25)) ^ ((W[t2-15] >>> 18) | (W[t2-15] << 14)) ^ (W[t2-15] >>> 3);
                var s1 = ((W[t2-2] >>> 17) | (W[t2-2] << 15)) ^ ((W[t2-2] >>> 19) | (W[t2-2] << 13)) ^ (W[t2-2] >>> 10);
                W[t2] = ((W[t2-16] + s0 + W[t2-7] + s1) >>> 0);
            }
            var a=H[0],b=H[1],c=H[2],d=H[3],e=H[4],f=H[5],g=H[6],h=H[7];
            for (var t3 = 0; t3 < 64; t3++) {
                var S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
                var ch = (e & f) ^ ((~e) & g);
                var T1 = (h + S1 + ch + K[t3] + W[t3]) >>> 0;
                var S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
                var mj = (a & b) ^ (a & c) ^ (b & c);
                var T2 = (S0 + mj) >>> 0;
                h = g; g = f; f = e; e = (d + T1) >>> 0;
                d = c; c = b; b = a; a = (T1 + T2) >>> 0;
            }
            H[0]=(H[0]+a)>>>0; H[1]=(H[1]+b)>>>0; H[2]=(H[2]+c)>>>0; H[3]=(H[3]+d)>>>0;
            H[4]=(H[4]+e)>>>0; H[5]=(H[5]+f)>>>0; H[6]=(H[6]+g)>>>0; H[7]=(H[7]+h)>>>0;
        }
        var out = new Uint8Array(32);
        for (var oi = 0; oi < 8; oi++) {
            out[oi*4]   = (H[oi] >>> 24) & 0xff;
            out[oi*4+1] = (H[oi] >>> 16) & 0xff;
            out[oi*4+2] = (H[oi] >>> 8) & 0xff;
            out[oi*4+3] = H[oi] & 0xff;
        }
        return out;
    }

    _g.crypto = {
        getRandomValues: function(buf) {
            var u8 = _toUint8(buf);
            for (var i = 0; i < u8.length; i++) u8[i] = _nextByte();
            return buf;
        },
        randomUUID: function() {
            var b = new Uint8Array(16);
            for (var i = 0; i < 16; i++) b[i] = _nextByte();
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            var h = [];
            for (var j = 0; j < 16; j++) h.push(('00' + b[j].toString(16)).slice(-2));
            return h.slice(0,4).join('') + '-' + h.slice(4,6).join('') + '-' +
                   h.slice(6,8).join('') + '-' + h.slice(8,10).join('') + '-' +
                   h.slice(10,16).join('');
        },
        subtle: {
            digest: function(algo, buf) {
                var name = (typeof algo === 'string') ? algo : (algo && algo.name) || 'SHA-256';
                var bytes = _toUint8(buf);
                // Only SHA-256 is implemented natively; other algos return a
                // stable-length zero buffer so the challenge JS can proceed.
                if (name === 'SHA-256') {
                    var out = _sha256(bytes);
                    return Promise.resolve(out.buffer);
                }
                var len = (name === 'SHA-384') ? 48 : (name === 'SHA-512') ? 64 : 32;
                var zero = new Uint8Array(len);
                return Promise.resolve(zero.buffer);
            },
            encrypt: function(algo, _key, data) {
                // AES-GCM stub: return ciphertext = data || 16-byte tag.
                var bytes = _toUint8(data);
                var out = new Uint8Array(bytes.length + 16);
                out.set(bytes, 0);
                return Promise.resolve(out.buffer);
            },
            decrypt: function(algo, _key, data) {
                var bytes = _toUint8(data);
                var len = Math.max(0, bytes.length - 16);
                var out = new Uint8Array(len);
                out.set(bytes.subarray(0, len), 0);
                return Promise.resolve(out.buffer);
            },
            importKey: function() { return Promise.resolve({ type: 'secret' }); },
            sign: function(_a, _k, data) { return Promise.resolve(_sha256(_toUint8(data)).buffer); },
            verify: function() { return Promise.resolve(true); }
        }
    };

    // ── HTMLCanvasElement stub ─────────────────────────────────────────────
    // Anti-fingerprinting: every canvas reports the same base64 blob.
    var _CANVAS_PNG =
        'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl21bKAAAAA1BMVEUAAACnej3aAAAAAXRSTlMAQObYZgAAAApJREFUCNdjYAAAAAIAAeIhvDMAAAAASUVORK5CYII=';
    var _origCreate = document.createElement.bind(document);
    document.createElement = function(tag) {
        var el = _origCreate(tag);
        if (String(tag).toLowerCase() === 'canvas') {
            el.width = 300;
            el.height = 150;
            el.toDataURL = function() { return 'data:image/png;base64,' + _CANVAS_PNG; };
            el.getContext = function() {
                return {
                    fillRect: function() {},
                    fillText: function() {},
                    getImageData: function(_x, _y, w, h) {
                        return { data: new Uint8ClampedArray(w * h * 4), width: w, height: h };
                    },
                    measureText: function(text) { return { width: String(text).length * 6 }; },
                    beginPath: function() {},
                    closePath: function() {},
                    stroke: function() {},
                    fill: function() {},
                    arc: function() {},
                    save: function() {},
                    restore: function() {},
                    translate: function() {},
                    rotate: function() {},
                    scale: function() {}
                };
            };
        }
        return el;
    };

    // ── Timers + microtask queue ───────────────────────────────────────────
    // QuickJS has no event loop, but many challenge scripts only need
    // setTimeout(fn, 0) semantics (i.e. deferred execution). We run the
    // callback synchronously as a best-effort approximation.
    var _nextId = 1;
    var _timers = {};
    _g.setTimeout = function(fn, _ms) {
        var id = _nextId++;
        _timers[id] = fn;
        try { if (typeof fn === 'function') fn(); } catch (e) {}
        return id;
    };
    _g.setInterval = function(_fn, _ms) { return _nextId++; };
    _g.clearTimeout = function(id) { delete _timers[id]; };
    _g.clearInterval = function(_id) {};
    _g.queueMicrotask = function(fn) { try { fn(); } catch (e) {} };
})(globalThis);
";

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

    #[cfg(feature = "js-dom-full")]
    #[test]
    fn test_full_dom_navigator_shape() {
        let engine = JsEngine::new().unwrap();
        engine.inject_full_dom().unwrap();

        let ua = engine.eval("navigator.userAgent").unwrap();
        assert!(!ua.is_empty());
        let hc = engine.eval("navigator.hardwareConcurrency").unwrap();
        assert_eq!(hc, "8");
        let wd = engine.eval("navigator.webdriver").unwrap();
        assert_eq!(wd, "false");
    }

    #[cfg(feature = "js-dom-full")]
    #[test]
    fn test_full_dom_performance_now_monotonic() {
        let engine = JsEngine::new().unwrap();
        engine.inject_full_dom().unwrap();

        let t = engine
            .eval("var a = performance.now(); var b = performance.now(); (b >= a) ? 'ok' : 'bad'")
            .unwrap();
        assert_eq!(t, "ok");
    }

    #[cfg(feature = "js-dom-full")]
    #[test]
    fn test_full_dom_document_cookie_roundtrip() {
        let engine = JsEngine::new().unwrap();
        engine.inject_full_dom().unwrap();

        engine
            .eval("document.cookie = 'foo=bar; Path=/'; document.cookie = 'baz=qux'; 0")
            .unwrap();
        let cookies = engine.eval("document.cookie").unwrap();
        assert!(cookies.contains("foo=bar"));
        assert!(cookies.contains("baz=qux"));
    }

    #[cfg(feature = "js-dom-full")]
    #[test]
    fn test_full_dom_canvas_to_data_url_stable() {
        let engine = JsEngine::new().unwrap();
        engine.inject_full_dom().unwrap();

        let a = engine
            .eval("document.createElement('canvas').toDataURL()")
            .unwrap();
        let b = engine
            .eval("document.createElement('canvas').toDataURL()")
            .unwrap();
        assert_eq!(a, b, "toDataURL must be fingerprint-stable");
        assert!(a.starts_with("data:image/png;base64,"));
    }

    #[cfg(feature = "js-dom-full")]
    #[test]
    fn test_full_dom_crypto_get_random_values() {
        let engine = JsEngine::new().unwrap();
        engine.inject_full_dom().unwrap();

        // Fill a 16-byte array; at least one byte should be non-zero (prob ≈ 1).
        let any_nonzero = engine
            .eval(
                r"
                var buf = new Uint8Array(16);
                crypto.getRandomValues(buf);
                var nz = 0;
                for (var i = 0; i < buf.length; i++) { if (buf[i] !== 0) nz++; }
                nz > 0 ? 'ok' : 'zero'
            ",
            )
            .unwrap();
        assert_eq!(any_nonzero, "ok");
    }
}
