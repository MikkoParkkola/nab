// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! YARA-X fetch-time guard for prompt-injection and exfiltration content.

use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use thiserror::Error;
use yara_x::{MetaValue, Rules, Scanner};

const BUILTIN_RULES: &str = include_str!("rules/prompt_firewall.yar");

const BUILTIN_RULE_IDS: &[&str] = &[
    "prompt_ignore_previous_instructions",
    "prompt_disregard_prior_messages",
    "prompt_system_prompt_exfil",
    "prompt_developer_mode_override",
    "prompt_new_goal_hijack",
    "prompt_tool_call_instruction",
    "prompt_hidden_html_comment",
    "prompt_hidden_style_directive",
    "prompt_data_attr_directive",
    "prompt_boundary_breakout",
    "prompt_do_not_summarize",
    "prompt_disable_safety_filters",
    "exfil_curl_secret_to_remote",
    "exfil_wget_sensitive_payload",
    "exfil_netcat_sensitive",
    "exfil_dns_sensitive",
    "exfil_webhook_env_dump",
    "exfil_cloud_metadata",
    "secret_aws_access_key",
    "secret_github_token",
    "secret_openai_key",
    "secret_slack_token",
    "secret_bearer_token",
    "secret_private_key_block",
    "obf_base64_bash_reverse_shell",
    "obf_base64_curl_pipe_shell",
    "obf_base64_python_exec",
    "obf_javascript_eval_atob",
    "obf_powershell_encoded_command",
    "obf_hex_encoded_curl",
];

/// Marker used when redacting a matched fetch body section.
pub const REDACTION_MARKER_PREFIX: &str = "[REDACTED BY NAB YARA";

/// Operator-selected behavior when signatures match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchGuardAction {
    /// Return a clearly marked body with matched sections replaced by redaction markers.
    Redact,
    /// Refuse to return body content and report the rules that fired.
    Refuse,
}

impl FetchGuardAction {
    fn from_value(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("refuse" | "block" | "deny") => Self::Refuse,
            _ => Self::Redact,
        }
    }
}

/// Fetch guard configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchGuardConfig {
    /// Match behavior.
    pub action: FetchGuardAction,
    /// Emergency bypass. `NAB_YARA_BYPASS=1` sets this to true.
    pub bypass: bool,
}

impl FetchGuardConfig {
    /// Build config from process environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_getter(|key| std::env::var(key).ok())
    }

    /// Build config from an injected environment getter, useful for tests.
    #[must_use]
    pub fn from_env_getter<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let bypass = get("NAB_YARA_BYPASS").is_some_and(|value| value.trim() == "1");
        let action = FetchGuardAction::from_value(get("NAB_YARA_ACTION").as_deref());
        Self { action, bypass }
    }
}

impl Default for FetchGuardConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// One YARA signature match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureMatch {
    /// YARA rule identifier.
    pub rule_id: String,
    /// Rule category metadata.
    pub category: String,
    /// Rule severity metadata.
    pub severity: String,
    /// Rule description metadata.
    pub description: String,
    /// Matched pattern identifiers inside the rule.
    pub pattern_ids: Vec<String>,
    /// Byte ranges for matched patterns in the scanned body.
    pub ranges: Vec<Range<usize>>,
}

/// Scan report for one fetch body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    /// Signatures that matched.
    pub matches: Vec<SignatureMatch>,
    /// Time spent in the scanner.
    pub elapsed: Duration,
}

impl ScanReport {
    /// True when no signatures matched.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.matches.is_empty()
    }

    fn empty() -> Self {
        Self {
            matches: Vec::new(),
            elapsed: Duration::ZERO,
        }
    }
}

/// Body returned by the fetch guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedBody {
    /// Body to return to the caller.
    pub body: String,
    /// Scan report. Empty when bypassed.
    pub report: ScanReport,
    /// True when `NAB_YARA_BYPASS=1` or equivalent config bypassed scanning.
    pub bypassed: bool,
}

/// Error returned by the YARA engine or policy application.
#[derive(Debug, Error)]
pub enum YaraEngineError {
    /// Built-in or custom rules did not compile.
    #[error("failed to compile YARA-X rules: {0}")]
    Compile(String),
    /// Scan execution failed.
    #[error("YARA-X scan failed: {0}")]
    Scan(String),
    /// Refuse policy blocked a body because one or more signatures matched.
    #[error("fetch refused by YARA-X guard; matches: {matches:?}")]
    Refused {
        /// Matching signatures.
        matches: Vec<SignatureMatch>,
    },
}

/// Compiled YARA-X engine.
#[derive(Clone)]
pub struct YaraEngine {
    rules: Arc<Rules>,
}

impl YaraEngine {
    /// Compile a custom rule source.
    ///
    /// # Errors
    ///
    /// Returns an error when YARA-X rejects the rule source.
    pub fn new(rule_source: &str) -> Result<Self, YaraEngineError> {
        Ok(Self {
            rules: Arc::new(compile_rules(rule_source)?),
        })
    }

    /// Scan a body using this engine.
    ///
    /// # Errors
    ///
    /// Returns an error when YARA-X scanning fails.
    pub fn scan(&self, bytes: &[u8]) -> Result<ScanReport, YaraEngineError> {
        let start = Instant::now();
        let mut scanner = Scanner::new(&self.rules);
        scanner.max_matches_per_pattern(8);
        let results = scanner
            .scan(bytes)
            .map_err(|err| YaraEngineError::Scan(err.to_string()))?;

        let mut matches = Vec::new();
        for rule in results.matching_rules() {
            let mut ranges = Vec::new();
            let mut pattern_ids = Vec::new();
            for pattern in rule.patterns() {
                let mut pattern_matched = false;
                for matched in pattern.matches() {
                    ranges.push(matched.range());
                    pattern_matched = true;
                }
                if pattern_matched {
                    pattern_ids.push(pattern.identifier().to_owned());
                }
            }

            matches.push(SignatureMatch {
                rule_id: rule.identifier().to_owned(),
                category: metadata_string(&rule, "category"),
                severity: metadata_string(&rule, "severity"),
                description: metadata_string(&rule, "description"),
                pattern_ids,
                ranges,
            });
        }

        Ok(ScanReport {
            matches,
            elapsed: start.elapsed(),
        })
    }
}

impl Default for YaraEngine {
    fn default() -> Self {
        let rules = builtin_rules()
            .expect("built-in nab YARA-X rules must compile; run cargo test --test yara_guard");
        Self { rules }
    }
}

/// Number of built-in rules.
#[must_use]
pub fn builtin_rule_count() -> usize {
    BUILTIN_RULE_IDS.len()
}

/// Built-in rule identifiers.
#[must_use]
pub fn builtin_rule_ids() -> &'static [&'static str] {
    BUILTIN_RULE_IDS
}

/// Built-in YARA-X source shared by nab and downstream hook consumers.
#[must_use]
pub fn builtin_rule_source() -> &'static str {
    BUILTIN_RULES
}

/// Guard a fetched body with the built-in engine and supplied policy.
///
/// # Errors
///
/// Returns [`YaraEngineError::Refused`] when the policy is refuse and a rule
/// matches, or another error if rule compilation/scanning fails.
pub fn guard_fetch_body(
    body: &str,
    config: &FetchGuardConfig,
) -> Result<GuardedBody, YaraEngineError> {
    if config.bypass {
        tracing::warn!("NAB_YARA_BYPASS=1 active; fetch-time YARA-X guard bypassed");
        return Ok(GuardedBody {
            body: body.to_owned(),
            report: ScanReport::empty(),
            bypassed: true,
        });
    }

    let engine = YaraEngine::default();
    let report = engine.scan(body.as_bytes())?;
    if report.is_clean() {
        return Ok(GuardedBody {
            body: body.to_owned(),
            report,
            bypassed: false,
        });
    }

    tracing::warn!(
        rules = %rule_list(&report.matches),
        match_count = report.matches.len(),
        elapsed_us = report.elapsed.as_micros(),
        "fetch-time YARA-X signatures matched"
    );

    match config.action {
        FetchGuardAction::Redact => Ok(GuardedBody {
            body: redact_body(body, &report.matches),
            report,
            bypassed: false,
        }),
        FetchGuardAction::Refuse => Err(YaraEngineError::Refused {
            matches: report.matches,
        }),
    }
}

fn builtin_rules() -> Result<Arc<Rules>, YaraEngineError> {
    static RULES: OnceLock<Result<Arc<Rules>, String>> = OnceLock::new();
    match RULES.get_or_init(|| {
        compile_rules(BUILTIN_RULES)
            .map(Arc::new)
            .map_err(|e| e.to_string())
    }) {
        Ok(rules) => Ok(Arc::clone(rules)),
        Err(err) => Err(YaraEngineError::Compile(err.clone())),
    }
}

fn compile_rules(source: &str) -> Result<Rules, YaraEngineError> {
    yara_x::compile(source).map_err(|err| YaraEngineError::Compile(err.to_string()))
}

fn metadata_string(rule: &yara_x::Rule<'_, '_>, key: &str) -> String {
    rule.metadata()
        .find_map(|(name, value)| {
            if name != key {
                return None;
            }
            match value {
                MetaValue::String(value) => Some(value.to_owned()),
                MetaValue::Bytes(value) => Some(String::from_utf8_lossy(value).into_owned()),
                MetaValue::Integer(value) => Some(value.to_string()),
                MetaValue::Float(value) => Some(value.to_string()),
                MetaValue::Bool(value) => Some(value.to_string()),
            }
        })
        .unwrap_or_default()
}

fn redact_body(body: &str, matches: &[SignatureMatch]) -> String {
    let ranges = redaction_ranges(body.as_bytes(), matches);
    if ranges.is_empty() {
        return body.to_owned();
    }

    let mut out = String::new();
    out.push_str("[NAB YARA SANITIZED: ");
    out.push_str(&matches.len().to_string());
    out.push_str(" signature match(es); rules: ");
    out.push_str(&rule_list(matches));
    out.push_str("]\n\n");

    let mut cursor = 0;
    for range in ranges {
        if range.start > cursor {
            out.push_str(&body[cursor..range.start]);
        }
        out.push_str(&redaction_marker(matches));
        cursor = range.end;
    }
    if cursor < body.len() {
        out.push_str(&body[cursor..]);
    }

    out
}

fn redaction_marker(matches: &[SignatureMatch]) -> String {
    format!("{REDACTION_MARKER_PREFIX}: rules={}]", rule_list(matches))
}

fn redaction_ranges(bytes: &[u8], matches: &[SignatureMatch]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for signature in matches {
        for range in &signature.ranges {
            if range.start >= bytes.len() {
                continue;
            }
            let start = line_start(bytes, range.start);
            let end = line_end(bytes, range.end.min(bytes.len()));
            if start < end {
                ranges.push(start..end);
            }
        }
    }

    ranges.sort_by_key(|range| (range.start, range.end));
    merge_ranges(ranges)
}

fn line_start(bytes: &[u8], mut at: usize) -> usize {
    while at > 0 && bytes[at - 1] != b'\n' {
        at -= 1;
    }
    at
}

fn line_end(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && bytes[at] != b'\n' {
        at += 1;
    }
    at
}

fn merge_ranges(ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}

fn rule_list(matches: &[SignatureMatch]) -> String {
    let mut rules: Vec<&str> = matches
        .iter()
        .map(|signature| signature.rule_id.as_str())
        .collect();
    rules.sort_unstable();
    rules.dedup();
    rules.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_rules_compile() {
        let engine = YaraEngine::default();
        let report = engine.scan(b"ordinary page").unwrap();
        assert!(report.matches.is_empty());
    }
}
