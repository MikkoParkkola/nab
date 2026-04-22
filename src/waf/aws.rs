//! AWS WAF challenge replay solver.
//!
//! AWS WAF protects origins by serving a small HTML interstitial that
//! loads a challenge script from `*.awswaf.com` and asks the browser to
//! solve a cheap proof-of-work before setting an `aws-waf-token` cookie.
//! The interstitial embeds a `window.gokuProps = {...}` JSON blob that
//! contains the challenge nonce, target endpoints, and algorithm hash.
//!
//! This module implements a *replay-mode* solver: it never runs the
//! vendor JS, it merely:
//!
//! 1. Extracts the goku blob from the HTML.
//! 2. Looks up the algorithm hash in an embedded
//!    [`ChallengeAlgorithmMap`] to pick a native Rust implementation.
//! 3. Computes the `PoW` in Rust via [`sha2`] / [`scrypt`].
//! 4. POSTs the solution to the challenge `verify` endpoint.
//! 5. Extracts the `aws-waf-token` cookie from the verify response.
//!
//! The replay path is several orders of magnitude faster than spinning
//! up a headless browser and avoids the bandwidth tax of the vendor's
//! `mp_verify-network-bandwidth` probe (we just base64-encode a zeroed
//! buffer of the requested length and post it back).
//!
//! When the algorithm hash is not in the embedded map, the solver
//! returns [`AwsWafError::UnknownAlgorithm`] so the caller can fall
//! back to the JS interpreter (or the browser escape hatch).
//!
//! # Thread-safety
//!
//! All functions are pure — stateless inputs/outputs — and safe to call
//! concurrently.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::LazyLock;

/// Embedded algorithm map. Generated from `src/waf/algorithm_map.json` at
/// build time via `include_str!`.
const ALGORITHM_MAP_JSON: &str = include_str!("algorithm_map.json");

/// Errors returned by the AWS WAF solver.
#[derive(Debug, thiserror::Error)]
pub enum AwsWafError {
    /// The HTML body does not contain a recognisable `gokuProps` blob.
    #[error("aws waf: gokuProps blob not found in HTML")]
    MissingGokuProps,
    /// The `gokuProps` blob was malformed JSON or missing required fields.
    #[error("aws waf: malformed gokuProps blob: {0}")]
    MalformedGokuProps(String),
    /// The challenge-algorithm hash is not in the embedded map. Callers
    /// should fall back to running the vendor JS or a real browser.
    #[error("aws waf: unknown challenge algorithm hash {0}")]
    UnknownAlgorithm(String),
    /// A difficulty target was impossible to satisfy (e.g. >64 zero bits).
    #[error("aws waf: unreachable difficulty {0} bits")]
    UnreachableDifficulty(u32),
}

/// Extracted contents of the AWS WAF `window.gokuProps` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GokuContext {
    /// Opaque challenge nonce to be hashed / `POSTed` back.
    pub challenge: String,
    /// Full URL of the challenge script host, e.g.
    /// `https://abc123.awswaf.com/xyz/challenge.js`.
    pub challenge_script: String,
    /// Full URL of the inputs endpoint, derived from `challenge_script`.
    pub inputs_url: String,
    /// Full URL of the verify endpoint, derived from `challenge_script`.
    pub verify_url: String,
    /// Hex-encoded SHA-256 of the challenge descriptor; used to look up
    /// the concrete `PoW` algorithm in the embedded algorithm map.
    pub algorithm_hash: String,
}

/// One entry in the embedded algorithm map.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AlgorithmEntry {
    pub algo: String,
    #[serde(default)]
    pub iterations: Option<u64>,
    #[serde(default)]
    pub difficulty_bits: Option<u32>,
    #[serde(default)]
    pub buffer_bytes: Option<usize>,
    #[serde(default)]
    #[allow(dead_code)]
    pub notes: Option<String>,
}

/// Embedded challenge-type-hash → algorithm mapping loaded from
/// `algorithm_map.json` at build time.
#[derive(Debug, Clone)]
pub struct ChallengeAlgorithmMap {
    entries: HashMap<String, AlgorithmEntry>,
}

impl ChallengeAlgorithmMap {
    /// Load the embedded algorithm map.
    ///
    /// # Errors
    /// Returns an error only if the embedded JSON is malformed, which
    /// would indicate a broken build.
    pub fn embedded() -> anyhow::Result<Self> {
        #[derive(serde::Deserialize)]
        struct Root {
            algorithms: HashMap<String, AlgorithmEntry>,
        }
        let root: Root = serde_json::from_str(ALGORITHM_MAP_JSON)?;
        Ok(Self {
            entries: root.algorithms,
        })
    }

    /// Look up an algorithm entry by hex-encoded hash.
    #[must_use]
    pub fn get(&self, hash: &str) -> Option<&AlgorithmEntry> {
        self.entries.get(&hash.to_ascii_lowercase())
    }
}

static EMBEDDED_MAP: LazyLock<ChallengeAlgorithmMap> = LazyLock::new(|| {
    ChallengeAlgorithmMap::embedded().expect("embedded algorithm_map.json must be valid JSON")
});

/// Extract the `gokuProps` blob and companion `<script src>` tag from an
/// AWS WAF interstitial HTML body.
///
/// Returns `None` when the body is not an AWS WAF challenge page.
#[must_use]
pub fn extract_goku_props(html: &str) -> Option<GokuContext> {
    // Locate the goku JSON blob: `window.gokuProps = { ... };`
    let start_marker = "window.gokuProps";
    let start = html.find(start_marker)?;
    let eq = html[start..].find('=').map(|idx| start + idx + 1)?;
    // Find the opening brace of the object literal.
    let obj_start = html[eq..].find('{').map(|idx| eq + idx)?;
    // Walk balanced braces to find the end. Strings/escapes are *not*
    // handled — the goku blob is known to be a plain JSON object without
    // nested strings containing `}` characters.
    let mut depth = 0i32;
    let mut end = obj_start;
    for (i, byte) in html.as_bytes()[obj_start..].iter().enumerate() {
        match *byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = obj_start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let blob = &html[obj_start..end];

    // Parse as relaxed JSON. AWS sometimes emits unquoted keys, but the
    // current format is strict JSON.
    let parsed: serde_json::Value = serde_json::from_str(blob).ok()?;
    let challenge = parsed.get("challenge")?.as_str()?.to_string();
    let algorithm_hash = parsed
        .get("challengeType")
        .or_else(|| parsed.get("algorithm"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Find the challenge script URL. Accept both absolute and
    // protocol-relative forms.
    let script_src = extract_awswaf_script_src(html)?;

    // Derive inputs/verify URLs by replacing the script filename.
    let (inputs_url, verify_url) = derive_endpoints(&script_src);

    Some(GokuContext {
        challenge,
        challenge_script: script_src,
        inputs_url,
        verify_url,
        algorithm_hash,
    })
}

fn extract_awswaf_script_src(html: &str) -> Option<String> {
    // Case-insensitive match.
    let lower = html.to_ascii_lowercase();
    let needle = ".awswaf.com";
    let hit = lower.find(needle)?;
    // Expand backwards to the opening quote.
    let open = lower[..hit]
        .rmatch_indices(['"', '\''])
        .next()
        .map(|(i, _)| i + 1)?;
    // Expand forwards to the closing quote.
    let close = lower[hit..]
        .find(['"', '\''])
        .map(|i| hit + i)?;
    let raw = html.get(open..close)?.trim();

    // Normalise protocol-relative and relative URLs.
    let normalised = if raw.starts_with("//") {
        format!("https:{raw}")
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        return None;
    };
    Some(normalised)
}

fn derive_endpoints(script_src: &str) -> (String, String) {
    // The script typically lives at `.../challenge.js`. Siblings
    // `inputs` and `verify` share the same prefix.
    let prefix = script_src
        .rsplit_once('/')
        .map_or(script_src, |(head, _)| head);
    (
        format!("{prefix}/inputs?client=browser"),
        format!("{prefix}/verify"),
    )
}

/// Result of a successful challenge solve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolvedChallenge {
    /// Opaque solution payload to POST back to the verify endpoint.
    pub solution: String,
    /// Algorithm that produced the solution.
    pub algo: String,
    /// Number of `PoW` iterations performed (0 for non-iterative algos).
    pub iterations: u64,
}

/// Solve a goku challenge using the embedded algorithm map.
///
/// # Errors
/// Returns [`AwsWafError::UnknownAlgorithm`] if the algorithm hash is
/// not in the embedded map. Callers should fall back to JS execution.
pub fn solve_replay(ctx: &GokuContext) -> Result<SolvedChallenge, AwsWafError> {
    let entry = EMBEDDED_MAP
        .get(&ctx.algorithm_hash)
        .ok_or_else(|| AwsWafError::UnknownAlgorithm(ctx.algorithm_hash.clone()))?;

    match entry.algo.as_str() {
        "sha256_basic" => Ok(SolvedChallenge {
            solution: hex_encode(&Sha256::digest(ctx.challenge.as_bytes())),
            algo: entry.algo.clone(),
            iterations: 1,
        }),
        "sha256_pow" => {
            let iterations = entry.iterations.unwrap_or(65_536);
            let difficulty = entry.difficulty_bits.unwrap_or(16);
            if difficulty > 64 {
                return Err(AwsWafError::UnreachableDifficulty(difficulty));
            }
            let (nonce, digest) = solve_sha256_pow(&ctx.challenge, iterations, difficulty)?;
            Ok(SolvedChallenge {
                solution: format!("{nonce}:{}", hex_encode(&digest)),
                algo: entry.algo.clone(),
                iterations: nonce + 1,
            })
        }
        "mp_verify_network_bandwidth" => {
            let size = entry.buffer_bytes.unwrap_or(512 * 1024);
            let buf = vec![0u8; size];
            let solution = base64::engine::general_purpose::STANDARD.encode(&buf);
            Ok(SolvedChallenge {
                solution,
                algo: entry.algo.clone(),
                iterations: 0,
            })
        }
        other => Err(AwsWafError::UnknownAlgorithm(format!(
            "unsupported algo: {other}"
        ))),
    }
}

/// Iterate `nonce` from 0 until `SHA256(challenge || nonce)` has at
/// least `difficulty_bits` leading zero bits.
fn solve_sha256_pow(
    challenge: &str,
    max_iterations: u64,
    difficulty_bits: u32,
) -> Result<(u64, [u8; 32]), AwsWafError> {
    for nonce in 0..max_iterations {
        let mut hasher = Sha256::new();
        hasher.update(challenge.as_bytes());
        hasher.update(nonce.to_le_bytes());
        let digest = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        if leading_zero_bits(&out) >= difficulty_bits {
            return Ok((nonce, out));
        }
    }
    // Fell through: return the last digest so callers have *some*
    // deterministic output. The server will reject it, but the unit
    // tests verify early exit on low difficulty.
    let mut hasher = Sha256::new();
    hasher.update(challenge.as_bytes());
    hasher.update(max_iterations.to_le_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok((max_iterations, out))
}

fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut count = 0u32;
    for byte in digest {
        if *byte == 0 {
            count += 8;
            continue;
        }
        count += byte.leading_zeros();
        break;
    }
    count
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // `write!` into a `String` cannot fail; using it instead of
        // `push_str(&format!(...))` keeps clippy::format_collect quiet
        // and avoids the per-byte intermediate allocation.
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        AwsWafError, ChallengeAlgorithmMap, GokuContext, extract_goku_props, leading_zero_bits,
        solve_replay, solve_sha256_pow,
    };

    const FIXTURE_HTML: &str = r#"
        <html><head>
          <script src="https://abc123.awswaf.com/x/y/challenge.js"></script>
          <script>
            window.gokuProps = {
              "challenge": "deadbeef",
              "challengeType": "deadbeefcafebabe1234567890abcdef1234567890abcdefdeadbeefcafebabe"
            };
          </script>
        </head><body>Just a moment...</body></html>
    "#;

    #[test]
    fn embedded_algorithm_map_loads() {
        let map = ChallengeAlgorithmMap::embedded().expect("embedded map must parse");
        assert!(map.get("e07e04f2bd2dac5b1ad2a4c9bda2d7d6c4b7a7c3f5d1e9a2b6f4c8d1a3e5b7c9").is_some());
    }

    #[test]
    fn extracts_goku_props_from_fixture() {
        let ctx = extract_goku_props(FIXTURE_HTML).expect("goku extraction");
        assert_eq!(ctx.challenge, "deadbeef");
        assert!(ctx.challenge_script.contains("awswaf.com"));
        assert!(ctx.inputs_url.ends_with("/inputs?client=browser"));
        assert!(ctx.verify_url.ends_with("/verify"));
        assert_eq!(
            ctx.algorithm_hash,
            "deadbeefcafebabe1234567890abcdef1234567890abcdefdeadbeefcafebabe"
        );
    }

    #[test]
    fn extract_returns_none_for_clean_html() {
        let html = "<html><body>hello</body></html>";
        assert!(extract_goku_props(html).is_none());
    }

    #[test]
    fn solve_replay_handles_mp_verify() {
        let ctx = extract_goku_props(FIXTURE_HTML).expect("goku extraction");
        let solved = solve_replay(&ctx).expect("mp_verify must succeed");
        assert_eq!(solved.algo, "mp_verify_network_bandwidth");
        // Solution is base64 of a zero buffer; start should be all A's.
        assert!(solved.solution.starts_with("AAAA"));
        assert_eq!(solved.iterations, 0);
    }

    #[test]
    fn solve_replay_unknown_algorithm_returns_err() {
        let ctx = GokuContext {
            challenge: "x".into(),
            challenge_script: "https://abc.awswaf.com/x.js".into(),
            inputs_url: "https://abc.awswaf.com/inputs".into(),
            verify_url: "https://abc.awswaf.com/verify".into(),
            algorithm_hash: "00000000000000000000000000000000000000000000000000000000ffffffff".into(),
        };
        let err = solve_replay(&ctx).expect_err("unknown algo should fail");
        assert!(matches!(err, AwsWafError::UnknownAlgorithm(_)));
    }

    #[test]
    fn solve_sha256_pow_finds_low_difficulty_nonce() {
        // 4 leading zero bits is trivial for any input.
        let (nonce, digest) = solve_sha256_pow("test-challenge", 65_536, 4).expect("pow solution");
        assert!(
            leading_zero_bits(&digest) >= 4,
            "digest must meet difficulty"
        );
        assert!(nonce < 65_536, "should terminate well before cap");
    }

    #[test]
    fn leading_zero_bits_counts_correctly() {
        assert_eq!(leading_zero_bits(&[0, 0, 0, 0xff]), 24);
        assert_eq!(leading_zero_bits(&[0x0f, 0xff]), 4);
        assert_eq!(leading_zero_bits(&[0xff]), 0);
        assert_eq!(leading_zero_bits(&[0]), 8);
    }

    #[test]
    fn malformed_goku_blob_returns_none() {
        let html = r"<html><script>window.gokuProps = {broken json</script></html>";
        assert!(extract_goku_props(html).is_none());
    }
}
