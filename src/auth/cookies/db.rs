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

// ─── Parsing ──────────────────────────────────────────────────────────────────

/// Build SQL `host_key` conditions for `domain` and its parent domains.
pub(super) fn build_domain_conditions(domain: &str) -> Vec<String> {
    let parts: Vec<&str> = domain.split('.').collect();
    let mut conditions = vec![
        format!("host_key = '{domain}'"),
        format!("host_key = '.{domain}'"),
    ];
    for i in 1..parts.len() {
        let parent = parts[i..].join(".");
        conditions.push(format!("host_key = '.{parent}'"));
    }
    conditions
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

/// Query the schema version of `temp_db` and return whether domain tags are present.
pub(super) fn has_domain_tag(temp_db: &std::path::Path) -> bool {
    let version = query_db_schema_version(temp_db);
    debug!("Cookie DB schema v{version}");
    version >= SCHEMA_VERSION_WITH_DOMAIN_TAG
}
