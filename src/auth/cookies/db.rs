// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! `SQLite` cookie database helpers for Chromium-based browsers.
//!
//! Handles copying the live database to a temp location, querying cookie rows,
//! parsing the tab-separated `sqlite3` CLI output, and decrypting the results.

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result};
use tracing::{debug, warn};

use super::crypto::{SCHEMA_VERSION_WITH_DOMAIN_TAG, decrypt_cookie_value};

// ─── Types ────────────────────────────────────────────────────────────────────

/// A single row from the Chromium `cookies` table.
pub(super) struct CookieRow {
    pub(super) name: String,
    /// Plaintext value (may be empty when `encrypted_value` is present).
    pub(super) value: String,
    /// Raw encrypted bytes (decoded from hex output by `sqlite3`).
    pub(super) encrypted_bytes: Vec<u8>,
}

/// A cookie row enriched with the metadata a Playwright `storage_state` needs.
///
/// Distinct from [`CookieRow`] because the storage-state export must keep every
/// cookie individually (a `Vec`), never collapse by name like the fetch hot path
/// does: the same cookie name commonly appears on multiple host keys / paths and
/// each is a separate credential.
pub(super) struct RichCookieRow {
    pub(super) name: String,
    pub(super) value: String,
    pub(super) encrypted_bytes: Vec<u8>,
    /// Real Chromium `host_key` (e.g. `.github.com`) — load-bearing for auth.
    pub(super) host_key: String,
    pub(super) path: String,
    /// Chromium `expires_utc`: microseconds since 1601-01-01, `0` for session.
    pub(super) expires_utc: i64,
    pub(super) is_httponly: bool,
    pub(super) is_secure: bool,
    /// Chromium `samesite`: `-1` unspecified, `0` None, `1` Lax, `2` Strict.
    pub(super) samesite: i64,
}

// ─── DB interaction ───────────────────────────────────────────────────────────

/// Copy the browser cookie database to a temp directory (avoids locking issues).
pub(super) fn copy_db_to_temp(cookie_path: &std::path::Path) -> Result<std::path::PathBuf> {
    let temp_dir = std::env::temp_dir().join(format!("nab_cookies_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;
    let temp_db = temp_dir.join("Cookies");
    std::fs::copy(cookie_path, &temp_db)?;

    // Copy WAL/SHM files so SQLite can read a consistent snapshot.
    for suffix in ["-wal", "-shm"] {
        let wal = cookie_path.with_extension(format!("Cookies{suffix}"));
        if wal.exists() {
            let _ = std::fs::copy(&wal, temp_db.with_extension(format!("Cookies{suffix}")));
        }
    }

    Ok(temp_db)
}

/// Query the `meta` table for the DB schema version (0 if unavailable).
///
/// Chromium increments this monotonically; v24 added `SHA-256(host_key)` prepended
/// to every decrypted cookie value.
pub(super) fn query_db_schema_version(temp_db: &std::path::Path) -> u32 {
    let Some(db_str) = temp_db.to_str() else {
        return 0;
    };
    let output = Command::new("sqlite3")
        .args([db_str, "SELECT value FROM meta WHERE key='version';"])
        .output();
    let Ok(out) = output else { return 0 };
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

/// Query the cookie database for rows matching `domain` and its parents.
///
/// Uses `hex(encrypted_value)` to avoid binary corruption when reading blobs
/// through the `sqlite3` CLI and `String::from_utf8_lossy`.
pub(super) fn query_cookie_db(temp_db: &std::path::Path, domain: &str) -> Result<Vec<CookieRow>> {
    let conditions = build_domain_conditions(domain);
    let where_clause = conditions.join(" OR ");
    let query =
        format!("SELECT name, value, hex(encrypted_value) FROM cookies WHERE {where_clause}");
    debug!("Cookie SQL query for '{}': WHERE {}", domain, where_clause);

    let temp_db_str = temp_db
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid temp database path"))?;
    let output = Command::new("sqlite3")
        .args(["-separator", "\t", temp_db_str, &query])
        .output()
        .context("Failed to query cookie database")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("SQLite query failed: {}", stderr);
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_cookie_rows(&stdout))
}

/// Like [`query_cookie_db`] but selects the full metadata each cookie carries,
/// for a faithful Playwright `storage_state` export.
///
/// Returns one [`RichCookieRow`] per database row (never collapsed by name).
pub(super) fn query_cookie_db_rich(
    temp_db: &std::path::Path,
    domain: &str,
) -> Result<Vec<RichCookieRow>> {
    let conditions = build_domain_conditions(domain);
    let where_clause = conditions.join(" OR ");
    // Order matters: parse_rich_cookie_rows reads columns positionally.
    let query = format!(
        "SELECT name, value, hex(encrypted_value), host_key, path, expires_utc, \
         is_httponly, is_secure, samesite FROM cookies WHERE {where_clause}"
    );
    debug!(
        "Rich cookie SQL query for '{}': WHERE {}",
        domain, where_clause
    );

    let temp_db_str = temp_db
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid temp database path"))?;
    let output = Command::new("sqlite3")
        .args(["-separator", "\t", temp_db_str, &query])
        .output()
        .context("Failed to query cookie database")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("SQLite rich query failed: {}", stderr);
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_rich_cookie_rows(&stdout))
}

// ─── Parsing ──────────────────────────────────────────────────────────────────

/// Build SQL `host_key` conditions for `domain` and its parent domains.
pub(super) fn build_domain_conditions(domain: &str) -> Vec<String> {
    domain_candidates(domain)
        .into_iter()
        .map(|host_key| format!("host_key = '{}'", host_key.replace('\'', "''")))
        .collect()
}

pub(super) fn domain_candidates(domain: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    push_host_key_pair(&mut candidates, domain);

    if let Some(base_domain) = domain.strip_prefix("www.") {
        if !base_domain.is_empty() {
            push_host_key_pair(&mut candidates, base_domain);
        }
    } else if !domain.is_empty() {
        push_host_key_pair(&mut candidates, &format!("www.{domain}"));
    }

    let parts: Vec<&str> = domain.split('.').collect();
    for i in 1..parts.len() {
        let parent = parts[i..].join(".");
        push_host_key(&mut candidates, &format!(".{parent}"));
    }
    candidates
}

fn push_host_key_pair(candidates: &mut Vec<String>, domain: &str) {
    push_host_key(candidates, domain);
    push_host_key(candidates, &format!(".{domain}"));
}

fn push_host_key(candidates: &mut Vec<String>, host_key: &str) {
    if !candidates.iter().any(|candidate| candidate == host_key) {
        candidates.push(host_key.to_string());
    }
}

/// Parse tab-separated output from `sqlite3` into [`CookieRow`] structs.
pub(super) fn parse_cookie_rows(stdout: &str) -> Vec<CookieRow> {
    let mut rows = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let encrypted_bytes = if parts.len() >= 3 && !parts[2].is_empty() {
            hex::decode(parts[2]).unwrap_or_default()
        } else {
            Vec::new()
        };
        rows.push(CookieRow {
            name: parts[0].to_string(),
            value: parts[1].to_string(),
            encrypted_bytes,
        });
    }
    rows
}

/// Parse tab-separated rich-query output into [`RichCookieRow`] structs.
///
/// Expects nine tab-separated columns in the order emitted by
/// [`query_cookie_db_rich`]: `name, value, hex(encrypted_value), host_key, path,
/// expires_utc, is_httponly, is_secure, samesite`. The first three use
/// `splitn(3,…)` semantics historically, but here every field after the hex blob
/// is a fixed integer/short string, so a positional `splitn(9, …)` is unambiguous.
pub(super) fn parse_rich_cookie_rows(stdout: &str) -> Vec<RichCookieRow> {
    let mut rows = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(9, '\t').collect();
        if parts.len() < 9 {
            continue;
        }
        let encrypted_bytes = if parts[2].is_empty() {
            Vec::new()
        } else {
            hex::decode(parts[2]).unwrap_or_default()
        };
        rows.push(RichCookieRow {
            name: parts[0].to_string(),
            value: parts[1].to_string(),
            encrypted_bytes,
            host_key: parts[3].to_string(),
            path: parts[4].to_string(),
            expires_utc: parts[5].parse().unwrap_or(0),
            is_httponly: parts[6] == "1",
            is_secure: parts[7] == "1",
            samesite: parts[8].trim().parse().unwrap_or(-1),
        });
    }
    rows
}

// ─── Decryption orchestration ────────────────────────────────────────────────

/// Decrypt cookie rows into a map of `name -> plaintext value`.
///
/// Rows with a plaintext `value` are returned as-is.
/// Encrypted rows require `key`; without a key they are skipped.
///
/// `has_domain_tag` — when `true` (DB schema ≥ [`SCHEMA_VERSION_WITH_DOMAIN_TAG`]),
/// the first 32 bytes of each decrypted value are `SHA-256(host_key)` and are stripped.
pub(super) fn decrypt_rows(
    rows: Vec<CookieRow>,
    key: Option<&[u8]>,
    has_domain_tag: bool,
) -> HashMap<String, String> {
    let mut cookies = HashMap::new();

    for row in rows {
        if !row.value.is_empty() {
            cookies.insert(row.name, row.value);
            continue;
        }

        if row.encrypted_bytes.is_empty() {
            continue;
        }

        let Some(k) = key else {
            debug!(
                "Skipping encrypted cookie '{}' — no key available",
                row.name
            );
            continue;
        };

        match decrypt_cookie_value(&row.encrypted_bytes, k, has_domain_tag) {
            Ok(plain) => {
                cookies.insert(row.name, plain);
            }
            Err(e) => {
                warn!("Cookie decryption failed for '{}': {}", row.name, e);
            }
        }
    }

    cookies
}

/// Decrypt rich cookie rows into faithful Playwright cookies.
///
/// Mirrors [`decrypt_rows`] (plaintext as-is, encrypted requires `key`, domain
/// tag stripped when present) but preserves per-row metadata and never collapses
/// by name — so two same-named cookies on different hosts both survive.
pub(super) fn decrypt_rich_rows(
    rows: Vec<RichCookieRow>,
    key: Option<&[u8]>,
    has_domain_tag: bool,
) -> Vec<crate::auth::cookies::storage_state::PlaywrightCookie> {
    use crate::auth::cookies::storage_state::{
        PlaywrightCookie, chromium_expiry_to_unix, chromium_samesite_to_playwright,
    };

    let mut cookies = Vec::with_capacity(rows.len());
    for row in rows {
        let value = if !row.value.is_empty() {
            row.value
        } else if row.encrypted_bytes.is_empty() {
            continue;
        } else if let Some(k) = key {
            match decrypt_cookie_value(&row.encrypted_bytes, k, has_domain_tag) {
                Ok(plain) => plain,
                Err(e) => {
                    warn!("Cookie decryption failed for '{}': {}", row.name, e);
                    continue;
                }
            }
        } else {
            debug!(
                "Skipping encrypted cookie '{}' — no key available",
                row.name
            );
            continue;
        };

        cookies.push(PlaywrightCookie {
            name: row.name,
            value,
            domain: row.host_key,
            path: row.path,
            expires: chromium_expiry_to_unix(row.expires_utc),
            http_only: row.is_httponly,
            secure: row.is_secure,
            same_site: chromium_samesite_to_playwright(row.samesite),
        });
    }
    cookies
}
/// Query the schema version of `temp_db` and return whether domain tags are present.
pub(super) fn has_domain_tag(temp_db: &std::path::Path) -> bool {
    let version = query_db_schema_version(temp_db);
    debug!("Cookie DB schema v{version}");
    version >= SCHEMA_VERSION_WITH_DOMAIN_TAG
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rich_rows_reads_all_metadata_columns_positionally() {
        // GIVEN tab-separated rich-query output with two rows (one plaintext,
        // one with an encrypted hex blob) covering distinct samesite values.
        // WHEN parsed
        // THEN every metadata column is read into the right field.
        let stdout = "sess\tplain\t\t.github.com\t/\t13437022686718487\t1\t1\t2\n\
                      tok\t\t763130\tapi.github.com\t/v3\t0\t0\t0\t-1\n";
        let rows = parse_rich_cookie_rows(stdout);
        assert_eq!(rows.len(), 2);

        let a = &rows[0];
        assert_eq!(a.name, "sess");
        assert_eq!(a.value, "plain");
        assert!(a.encrypted_bytes.is_empty());
        assert_eq!(a.host_key, ".github.com");
        assert_eq!(a.path, "/");
        assert_eq!(a.expires_utc, 13_437_022_686_718_487);
        assert!(a.is_httponly);
        assert!(a.is_secure);
        assert_eq!(a.samesite, 2);

        let b = &rows[1];
        assert_eq!(b.name, "tok");
        assert!(b.value.is_empty());
        assert_eq!(b.encrypted_bytes, hex::decode("763130").unwrap());
        assert_eq!(b.host_key, "api.github.com");
        assert_eq!(b.path, "/v3");
        assert_eq!(b.expires_utc, 0);
        assert!(!b.is_httponly);
        assert!(!b.is_secure);
        assert_eq!(b.samesite, -1);
    }

    #[test]
    fn parse_rich_rows_skips_short_lines() {
        // Fewer than nine columns → not a valid cookie row, skipped.
        let rows = parse_rich_cookie_rows("only\ttwo\tcols\n");
        assert!(rows.is_empty());
    }

    #[test]
    fn decrypt_rich_rows_preserves_same_name_on_different_hosts() {
        // GIVEN two plaintext cookies sharing a name on different host keys
        // WHEN decrypted
        // THEN both survive as distinct entries (no name-collapse), proving the
        // storage_state path keeps every credential.
        let rows = vec![
            RichCookieRow {
                name: "s".into(),
                value: "1".into(),
                encrypted_bytes: Vec::new(),
                host_key: ".github.com".into(),
                path: "/".into(),
                expires_utc: 0,
                is_httponly: true,
                is_secure: true,
                samesite: 1,
            },
            RichCookieRow {
                name: "s".into(),
                value: "2".into(),
                encrypted_bytes: Vec::new(),
                host_key: "api.github.com".into(),
                path: "/".into(),
                expires_utc: 0,
                is_httponly: false,
                is_secure: false,
                samesite: 0,
            },
        ];
        let out = decrypt_rich_rows(rows, None, false);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].domain, ".github.com");
        assert_eq!(out[1].domain, "api.github.com");
        assert_eq!(out[1].value, "2");
    }

    #[test]
    fn decrypt_rich_rows_skips_encrypted_without_key() {
        // An encrypted row with no key available is dropped, not surfaced empty.
        let rows = vec![RichCookieRow {
            name: "enc".into(),
            value: String::new(),
            encrypted_bytes: vec![1, 2, 3],
            host_key: ".x.com".into(),
            path: "/".into(),
            expires_utc: 0,
            is_httponly: false,
            is_secure: true,
            samesite: 1,
        }];
        assert!(decrypt_rich_rows(rows, None, false).is_empty());
    }
}
