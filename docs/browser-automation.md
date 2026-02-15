# Browser Automation (CDP Integration)

Phase 3 feature: Optional Chrome DevTools Protocol integration for SPA login and CAPTCHA handling.

## Overview

The `browser` feature enables automated login through a running Chrome/Chromium instance using the Chrome DevTools Protocol (CDP). This solves two major challenges:

1. **SPA (Single Page Application) Login Forms** - Forms rendered client-side via React/Vue/Angular that aren't visible to HTTP requests
2. **CAPTCHA-Protected Sites** - Sites that require human interaction to solve visual challenges

## Build and Installation

### Default Build (No Browser Support)

```bash
cargo build --release --features pdf
```

### With Browser Support

```bash
cargo build --release --features pdf,browser
```

## Prerequisites

1. **Chrome/Chromium** must be installed
2. **Remote Debugging** must be enabled on port 9222

### Start Chrome with Remote Debugging

```bash
# macOS
/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome --remote-debugging-port=9222

# Linux
google-chrome --remote-debugging-port=9222

# Or Chromium
chromium --remote-debugging-port=9222
```

**Important**: Keep this Chrome instance running while using `nab login --browser`.

## Usage

### Basic Browser-Based Login

```bash
nab login https://example.com/login --browser
```

This will:
1. Connect to Chrome on port 9222
2. Open the URL in a new browser tab
3. Fill credentials from 1Password (if `--1password` is used)
4. Wait 60 seconds if CAPTCHA is detected (for manual intervention)
5. Extract cookies from the authenticated session
6. Use those cookies to fetch the final page content

### With 1Password Integration

```bash
nab login https://example.com/login --browser --1password
```

Automatically fills username and password from 1Password vault.

### Manual CAPTCHA Solving

When a CAPTCHA is detected, you'll see:

```
⚠️  CAPTCHA detected - please solve it in the browser window
   Waiting 60 seconds for manual intervention...
```

Simply switch to the Chrome window, solve the CAPTCHA, and submit the form manually. The tool will continue after the timeout.

## When to Use Browser-Based Login

### Use `--browser` when:

- ✅ Site has a SPA login form (React/Vue/Angular)
- ✅ CAPTCHA is blocking automated login
- ✅ Complex JavaScript-based authentication flow
- ✅ OAuth/SSO redirect chains

### Use standard login (no `--browser`) when:

- ✅ Traditional server-rendered HTML forms
- ✅ No CAPTCHA protection
- ✅ Simple form submission flow

## Architecture

### Components

1. **`src/browser.rs`** - CDP client wrapper
   - `BrowserLogin::connect()` - Connect to Chrome on port 9222
   - `BrowserLogin::login()` - Automated login flow with CAPTCHA detection
   - `BrowserLogin::extract_cookies()` - Get session cookies

2. **`src/login.rs`** - Updated login orchestrator
   - `LoginFlow::with_browser(true)` - Enable browser-based login
   - `browser_login()` - CDP-based login path (feature-gated)

3. **`src/cmd/login.rs`** - CLI command handler
   - `--browser` flag (only available when feature enabled)

### Feature Gating

All browser-related code is behind `#[cfg(feature = "browser")]`:

```rust
#[cfg(feature = "browser")]
pub mod browser;

#[cfg(feature = "browser")]
pub use browser::{BrowserLogin, Cookie};
```

**Zero Impact** when feature is disabled:
- No code compiled
- No dependencies added
- Binary size unchanged

## Error Messages

### SPA Detected (No Form Found)

**Without browser feature:**
```
No login form found (SPA detected).
💡 Log in via your browser, then use: nab fetch <url> --cookies brave
```

**With browser feature:**
```
No login form found (SPA detected).
💡 Try browser-based login: nab login <url> --browser
💡 Or log in via your browser, then use: nab fetch <url> --cookies brave
```

### CAPTCHA Detected

**Without browser feature:**
```
reCAPTCHA detected on login form
💡 CAPTCHA may block login. Try: nab fetch <url> --cookies brave
```

**With browser feature:**
```
reCAPTCHA detected on login form
💡 CAPTCHA detected. Try browser-based login: nab login <url> --browser
```

### Chrome Not Running

```
Failed to connect to Chrome. Start Chrome with: google-chrome --remote-debugging-port=9222
```

## Implementation Details

### CDP Communication

Uses `chromiumoxide` crate for Chrome DevTools Protocol:
- WebSocket-based communication
- Async/await throughout
- Event-driven handler for CDP events

### Form Field Detection

Tries multiple CSS selectors for username/password fields:

**Username:**
- `input[name='username']`
- `input[name='email']`
- `input[type='email']`
- `input[id='username']`
- `input[id='email']`

**Password:**
- `input[name='password']`
- `input[type='password']`
- `input[id='password']`

**Submit Button:**
- `button[type='submit']`
- `input[type='submit']`
- `button:has-text('Sign in')`
- `button:has-text('Log in')`

### CAPTCHA Detection

Checks for common CAPTCHA providers:
- reCAPTCHA (`.g-recaptcha`, `iframe[src*='recaptcha']`)
- hCaptcha (`.h-captcha`, `iframe[src*='hcaptcha']`)
- Cloudflare Turnstile (`.cf-turnstile`)

### Cookie Extraction

Extracts all cookies for the current domain and converts to HTTP `Cookie` header format:

```
Cookie: session=abc123; token=xyz789
```

## Testing

Run tests with browser feature:

```bash
cargo test --lib --features browser
```

All browser tests are unit tests (no actual Chrome instance required):
- `test_cookies_to_header` - Cookie header formatting
- `test_empty_cookies` - Empty cookie list handling
- `test_single_cookie` - Single cookie conversion
- `test_cookie_equality` - Cookie comparison
- `test_cookie_debug` - Debug trait implementation
- `test_cookie_clone` - Clone trait implementation

## Performance Impact

### Binary Size

| Build | Size | Delta |
|-------|------|-------|
| Default (`--features pdf`) | ~11 MB | baseline |
| With browser (`--features pdf,browser`) | ~13 MB | +2 MB |

### Runtime

- Connection to Chrome: ~50-100ms
- Form filling: ~100-200ms per field
- CAPTCHA wait: 60 seconds (user-configurable in code)
- Cookie extraction: ~10-20ms

## Security Considerations

1. **Chrome must be running locally** - Only connects to `localhost:9222`
2. **Cookies are in memory only** - Not persisted to disk (session saving TODO)
3. **1Password integration** - Uses official `op` CLI (secure credential access)
4. **No credentials in logs** - Passwords/tokens never logged

## Limitations

1. **Chrome must be started manually** - No automatic Chrome launch
2. **Port 9222 hardcoded** - Cannot customize (TODO: make configurable)
3. **60-second CAPTCHA timeout** - Fixed duration (TODO: make configurable)
4. **No session persistence** - Cookies not saved to disk yet

## Troubleshooting

### "Failed to connect to Chrome"

**Cause**: Chrome not running with remote debugging enabled

**Solution**:
```bash
google-chrome --remote-debugging-port=9222
```

### "No login form found"

**Cause**: Site uses SPA (client-side rendering)

**Solution**: Use `--browser` flag to enable CDP-based login

### "CAPTCHA detected"

**Cause**: Site has CAPTCHA protection

**Solution**: Use `--browser` and solve CAPTCHA manually in the Chrome window

### "Browser login successful" but still redirected to login

**Cause**: Session cookies not being sent in subsequent requests

**Solution**: Check cookie domain and path. May need to implement session persistence.

## Future Enhancements

- [ ] Automatic Chrome launch
- [ ] Configurable remote debugging port
- [ ] Configurable CAPTCHA timeout
- [ ] Session cookie persistence to disk
- [ ] Support for Playwright (cross-browser)
- [ ] Headless mode option
- [ ] Screenshot capture on failure
- [ ] Multi-step authentication flows
- [ ] OAuth/SSO handling

## References

- [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/)
- [chromiumoxide crate](https://docs.rs/chromiumoxide/)
- [CDP Commands Reference](https://chromedevtools.github.io/devtools-protocol/tot/)
