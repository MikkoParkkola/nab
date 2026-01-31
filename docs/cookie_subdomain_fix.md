# Cookie Subdomain Matching Fix

## Problem

When fetching `areena.yle.fi`, microfetch reported "0 cookies for areena.yle.fi", but when fetching `yle.fi` it found "8 cookies from brave".

The issue was that cookies set on `.yle.fi` (parent domain with leading dot) were not being sent to subdomains like `areena.yle.fi`.

## Root Causes

### 1. Native Rust Cookie Extraction (SQL Query)

**Original buggy code:**
```rust
let query = format!(
    "SELECT name, value, encrypted_value FROM cookies WHERE host_key LIKE '%{domain}' OR host_key LIKE '%.{domain}'"
);
```

**Problem:** This LIKE pattern doesn't match parent domain cookies. For `areena.yle.fi`, it would look for:
- `host_key LIKE '%areena.yle.fi'` - matches `areena.yle.fi` but not `.yle.fi`
- `host_key LIKE '%.areena.yle.fi'` - matches `.areena.yle.fi` but not `.yle.fi`

**Fix:** Generate exact match conditions for the domain and all parent domains:
```rust
// For areena.yle.fi, generate:
// host_key = 'areena.yle.fi' OR host_key = '.areena.yle.fi' OR host_key = '.yle.fi' OR host_key = '.fi'
let domain_parts: Vec<&str> = domain.split('.').collect();
let mut conditions = vec![
    format!("host_key = '{domain}'"),           // Exact match
    format!("host_key = '.{domain}'"),          // Parent domain with dot
];

// Add parent domain matches
for i in 1..domain_parts.len() {
    let parent = domain_parts[i..].join(".");
    conditions.push(format!("host_key = '.{parent}'"));
}

let where_clause = conditions.join(" OR ");
```

### 2. Python Fallback (browser_cookie3)

**Original buggy code:**
```python
cj = bc.brave(domain_name='{domain}')
cookies = {c.name: c.value for c in cj if '{domain}' in c.domain}
```

**Problems:**
1. `browser_cookie3`'s `domain_name` parameter doesn't support subdomain matching - it only returns cookies for exact domain matches
2. The filter `if '{domain}' in c.domain` would fail even if cookies were returned, because `"areena.yle.fi"` is not contained in `".yle.fi"`

**Fix:** Fetch all cookies and implement proper RFC 6265 cookie domain matching:
```python
# Don't use domain_name parameter
cj = bc.brave()

def matches_cookie_domain(cookie_domain, request_domain):
    if cookie_domain.startswith('.'):
        # Parent domain with leading dot - matches request domain and all subdomains
        parent = cookie_domain[1:]  # Remove leading dot
        return request_domain == parent or request_domain.endswith('.' + parent)
    else:
        # No leading dot - exact match only
        return cookie_domain == request_domain

cookies = {c.name: c.value for c in cj if matches_cookie_domain(c.domain, '{domain}')}
```

## Cookie Domain Matching Rules (RFC 6265)

- Cookie on `.example.com` matches `example.com`, `sub.example.com`, `sub.sub.example.com`, etc.
- Cookie on `example.com` (no leading dot) matches only `example.com` exactly
- Cookie on `.sub.example.com` matches `sub.example.com` and `deeper.sub.example.com`

## Testing

Created test suite in `tests/cookie_subdomain_test.sh` that verifies:
1. Subdomain (`areena.yle.fi`) finds parent domain cookies (`.yle.fi`)
2. Parent domain (`yle.fi`) finds its own cookies (`.yle.fi`)

Run with:
```bash
./tests/cookie_subdomain_test.sh
```

## Files Changed

- `src/auth.rs`:
  - Fixed `get_cookies_native()` SQL query generation (lines 558-574)
  - Fixed `get_cookies_via_python()` cookie filtering (lines 650-676)
  - Added debug logging for cookie queries

## Verification

Before fix:
```bash
$ microfetch fetch https://areena.yle.fi --cookies brave
# No cookie message (0 cookies loaded)
```

After fix:
```bash
$ microfetch fetch https://areena.yle.fi --cookies brave
🍪 Loaded 8 cookies from brave
```

Both subdomain and parent domain now correctly match cookies:
- `areena.yle.fi` → finds 8 cookies from `.yle.fi`
- `yle.fi` → finds 8 cookies from `.yle.fi`
