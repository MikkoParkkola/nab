// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Fetch-time YARA-X guard integration.

pub use crate::security::yara_engine::{
    FetchGuardAction, FetchGuardConfig, GuardedBody, SignatureMatch, YaraEngineError,
};
use anyhow::Result;

/// Apply the default fetch-time YARA-X policy from environment.
///
/// `NAB_YARA_BYPASS=1` bypasses scanning and emits an audit warning.
/// `NAB_YARA_ACTION=refuse` refuses matched bodies instead of redacting them.
///
/// # Errors
///
/// Returns an error when scanning fails or the configured policy refuses
/// a matched body.
pub fn guard_fetch_output(body: &str, surface: &str, url: &str) -> Result<String> {
    guard_fetch_output_with_config(body, surface, url, &FetchGuardConfig::from_env())
}

/// Apply fetch-time YARA-X policy with an explicit config.
///
/// # Errors
///
/// Returns an error when scanning fails or the configured policy refuses
/// a matched body.
pub fn guard_fetch_output_with_config(
    body: &str,
    surface: &str,
    url: &str,
    config: &FetchGuardConfig,
) -> Result<String> {
    let guarded = crate::security::yara_engine::guard_fetch_body(body, config)?;
    audit_guard_result(&guarded, surface, url);
    Ok(guarded.body)
}

fn audit_guard_result(guarded: &GuardedBody, surface: &str, url: &str) {
    if guarded.bypassed {
        tracing::warn!(
            surface,
            url,
            "NAB_YARA_BYPASS=1 active; fetch-time YARA-X guard bypassed"
        );
        return;
    }

    if guarded.report.matches.is_empty() {
        tracing::debug!(
            surface,
            url,
            elapsed_us = guarded.report.elapsed.as_micros(),
            "fetch-time YARA-X scan clean"
        );
        return;
    }

    let rules = guarded
        .report
        .matches
        .iter()
        .map(|m| m.rule_id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    tracing::warn!(
        surface,
        url,
        rules,
        match_count = guarded.report.matches.len(),
        elapsed_us = guarded.report.elapsed.as_micros(),
        "fetch-time YARA-X guard sanitized body"
    );
}
