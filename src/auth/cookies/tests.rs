// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Tests for browser cookie extraction, crypto, and DB parsing.

#[cfg(target_os = "macos")]
use super::lock_ignoring_poison;
use super::{
    CookieSource, KeychainInteraction, cookie_rows_need_key, crypto::*, db::*,
    keychain_interaction_from_env_os_value, keychain_interaction_from_env_value,
    load_cookie_domain_tag_if_needed, load_cookie_key_if_needed, resolve_cookie_lookup,
    rich_cookie_rows_need_key,
};

// ─── Test helpers ─────────────────────────────────────────────────────────────

/// Build a valid v10-encrypted blob from `inner_plaintext` using the same
/// cipher parameters that Chromium uses, for round-trip testing.
///
/// `inner_plaintext` is what is placed directly inside the AES envelope.
/// For schema-v24 blobs the caller should prepend 32 SHA-256 bytes itself.
fn encrypt_v10(inner_plaintext: &[u8], key: &[u8]) -> Vec<u8> {
    use aes::Aes128;
    use cbc::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let out_len = inner_plaintext.len() + 16;
    let mut out = vec![0u8; out_len];
    let enc = Aes128CbcEnc::new_from_slices(key, &AES_CBC_IV).unwrap();
    let ciphertext = enc
        .encrypt_padded_b2b::<Pkcs7>(inner_plaintext, &mut out)
        .expect("output buffer is always large enough");

    let mut blob = V10_PREFIX.to_vec();
    blob.extend_from_slice(ciphertext);
    blob
}

/// Build a v24+ blob: `v10` + AES-CBC(`SHA-256(host_key)` + `value`).
fn encrypt_v10_v24(value: &[u8], key: &[u8], host_key: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut inner = Sha256::digest(host_key.as_bytes()).to_vec();
    inner.extend_from_slice(value);
    encrypt_v10(&inner, key)
}

// ─── CookieSource metadata ────────────────────────────────────────────────────

#[test]
fn cookie_source_variants_are_distinct() {
    let sources = [
        CookieSource::Chrome,
        CookieSource::Firefox,
        CookieSource::Brave,
        CookieSource::Safari,
    ];
    for (i, a) in sources.iter().enumerate() {
        for (j, b) in sources.iter().enumerate() {
            if i != j {
                assert_ne!(
                    format!("{a:?}"),
                    format!("{b:?}"),
                    "variants {i} and {j} should differ"
                );
            }
        }
    }
}

#[test]
fn from_browser_name_maps_known_browsers_consistently() {
    assert!(matches!(
        CookieSource::from_browser_name("brave"),
        CookieSource::Brave
    ));
    assert!(matches!(
        CookieSource::from_browser_name("chrome"),
        CookieSource::Chrome
    ));
    assert!(matches!(
        CookieSource::from_browser_name("firefox"),
        CookieSource::Firefox
    ));
    assert!(matches!(
        CookieSource::from_browser_name("safari"),
        CookieSource::Safari
    ));
}

#[test]
fn from_browser_name_uses_chrome_family_fallback_for_edge_and_unknown() {
    assert!(matches!(
        CookieSource::from_browser_name("edge"),
        CookieSource::Chrome
    ));
    assert!(matches!(
        CookieSource::from_browser_name("dia"),
        CookieSource::Chrome
    ));
    assert!(matches!(
        CookieSource::from_browser_name("unknown"),
        CookieSource::Chrome
    ));
}

#[test]
fn keychain_service_brave_and_chrome_are_nonempty() {
    assert!(!CookieSource::Brave.keychain_service().is_empty());
    assert!(!CookieSource::Chrome.keychain_service().is_empty());
}

#[test]
fn keychain_service_firefox_safari_are_empty() {
    assert!(CookieSource::Firefox.keychain_service().is_empty());
    assert!(CookieSource::Safari.keychain_service().is_empty());
}

#[test]
fn keychain_interaction_policy_defaults_to_allow_and_fails_closed_when_configured() {
    for value in [None, Some(""), Some("allow"), Some("true"), Some("1")] {
        assert_eq!(
            keychain_interaction_from_env_value(value),
            KeychainInteraction::Allow,
            "value {value:?} should preserve explicit CLI behavior"
        );
    }
    for value in [
        Some("never"),
        Some("false"),
        Some("0"),
        Some("off"),
        Some("typo"),
    ] {
        assert_eq!(
            keychain_interaction_from_env_value(value),
            KeychainInteraction::Never,
            "configured value {value:?} must not unexpectedly allow UI"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_keychain_interaction_value_fails_closed() {
    use std::os::unix::ffi::OsStringExt;

    let value = std::ffi::OsString::from_vec(vec![0xff]);
    assert_eq!(
        keychain_interaction_from_env_os_value(Some(value.as_os_str())),
        KeychainInteraction::Never
    );
}

#[cfg(target_os = "macos")]
#[test]
fn keychain_serialization_recovers_after_mutex_poisoning() {
    let mutex = std::sync::Arc::new(std::sync::Mutex::new(()));
    let poisoner = std::sync::Arc::clone(&mutex);
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.lock().expect("initial lock");
        panic!("controlled mutex poisoning");
    })
    .join();

    let _guard = lock_ignoring_poison(&mutex);
}

#[test]
fn mixed_plaintext_and_encrypted_rows_require_a_successful_key_read() {
    let rows = vec![
        CookieRow {
            name: "plain".into(),
            value: "available".into(),
            encrypted_bytes: Vec::new(),
        },
        CookieRow {
            name: "session".into(),
            value: String::new(),
            encrypted_bytes: vec![1, 2, 3],
        },
    ];
    assert!(cookie_rows_need_key(&rows));

    let err = load_cookie_key_if_needed(true, KeychainInteraction::Never, || {
        Err(anyhow::anyhow!("Keychain interaction is not allowed"))
    })
    .expect_err("encrypted rows must preserve the key-read failure");
    assert!(err.to_string().contains("interaction is not allowed"));
}

#[test]
fn plaintext_rows_skip_keychain_loading() {
    let rows = vec![CookieRow {
        name: "plain".into(),
        value: "available".into(),
        encrypted_bytes: Vec::new(),
    }];
    assert!(!cookie_rows_need_key(&rows));
    let key = load_cookie_key_if_needed(false, KeychainInteraction::Never, || {
        panic!("plaintext rows must not read Keychain")
    })
    .expect("plaintext rows should not need a key");
    assert!(key.is_none());
}

#[test]
fn interactive_mixed_rows_retain_plaintext_when_key_read_fails() {
    let rows = vec![
        CookieRow {
            name: "plain".into(),
            value: "available".into(),
            encrypted_bytes: Vec::new(),
        },
        CookieRow {
            name: "session".into(),
            value: String::new(),
            encrypted_bytes: vec![1, 2, 3],
        },
    ];
    let key = load_cookie_key_if_needed(true, KeychainInteraction::Allow, || {
        Err(anyhow::anyhow!("interactive Keychain read failed"))
    })
    .expect("interactive mode should preserve recoverable plaintext rows");
    let native = decrypt_rows(rows, key.as_deref(), false);
    let fallback_called = std::cell::Cell::new(false);
    let cookies = resolve_cookie_lookup(Ok(native), KeychainInteraction::Allow, || {
        fallback_called.set(true);
        Err(anyhow::anyhow!("Python fallback failed"))
    })
    .expect("plaintext native cookie should survive failed key and fallback paths");

    assert_eq!(cookies.get("plain").map(String::as_str), Some("available"));
    assert!(!cookies.contains_key("session"));
    assert!(
        !fallback_called.get(),
        "usable native plaintext avoids fallback"
    );
}

#[test]
fn interactive_schema_failure_skips_encrypted_rows_without_losing_plaintext() {
    let domain_tag = load_cookie_domain_tag_if_needed(true, KeychainInteraction::Allow, || {
        Err(anyhow::anyhow!("schema query failed"))
    })
    .expect("interactive mode may preserve plaintext rows after schema failure");
    assert!(domain_tag.is_none());

    let rows = vec![
        CookieRow {
            name: "plain".into(),
            value: "available".into(),
            encrypted_bytes: Vec::new(),
        },
        CookieRow {
            name: "session".into(),
            value: String::new(),
            encrypted_bytes: vec![1, 2, 3],
        },
    ];
    let cookies = decrypt_rows(rows, None, domain_tag.unwrap_or(false));
    assert_eq!(cookies.get("plain").map(String::as_str), Some("available"));
    assert!(!cookies.contains_key("session"));
}

#[test]
fn noninteractive_schema_failure_is_preserved() {
    let err = load_cookie_domain_tag_if_needed(true, KeychainInteraction::Never, || {
        Err(anyhow::anyhow!("schema query failed"))
    })
    .expect_err("non-interactive extraction must fail closed on unknown schema");
    assert!(err.to_string().contains("schema query failed"));
}

#[test]
fn rich_encrypted_rows_require_a_key() {
    let rows = vec![RichCookieRow {
        name: "session".into(),
        value: String::new(),
        encrypted_bytes: vec![1, 2, 3],
        host_key: ".example.com".into(),
        path: "/".into(),
        expires_utc: 0,
        is_httponly: true,
        is_secure: true,
        samesite: 1,
    }];
    assert!(rich_cookie_rows_need_key(&rows));
}

#[test]
fn noninteractive_cookie_lookup_never_invokes_prompt_capable_fallback() {
    let fallback_called = std::cell::Cell::new(false);
    let native_error = anyhow::anyhow!("Keychain interaction is not allowed");

    let err = resolve_cookie_lookup(Err(native_error), KeychainInteraction::Never, || {
        fallback_called.set(true);
        Ok(std::collections::HashMap::from([(
            "session".to_string(),
            "must-not-be-read".to_string(),
        )]))
    })
    .expect_err("the native diagnostic should be preserved");

    assert!(err.to_string().contains("interaction is not allowed"));
    assert!(!fallback_called.get(), "Python fallback could prompt again");
}

#[test]
fn noninteractive_empty_native_result_stays_empty_without_fallback() {
    let fallback_called = std::cell::Cell::new(false);
    let cookies = resolve_cookie_lookup(
        Ok(std::collections::HashMap::new()),
        KeychainInteraction::Never,
        || {
            fallback_called.set(true);
            Ok(std::collections::HashMap::from([(
                "session".to_string(),
                "unexpected".to_string(),
            )]))
        },
    )
    .expect("an empty native store is not an error");

    assert!(cookies.is_empty());
    assert!(!fallback_called.get(), "Python fallback could prompt again");
}

#[test]
fn interactive_lookup_retains_python_fallback() {
    let cookies = resolve_cookie_lookup(
        Ok(std::collections::HashMap::new()),
        KeychainInteraction::Allow,
        || {
            Ok(std::collections::HashMap::from([(
                "session".to_string(),
                "expected".to_string(),
            )]))
        },
    )
    .expect("interactive fallback should remain available");

    assert_eq!(cookies.get("session").map(String::as_str), Some("expected"));
}

#[cfg(target_os = "macos")]
#[test]
fn cookie_paths_use_macos_locations() {
    let app_support = dirs::config_dir().expect("macOS should expose Application Support");
    let home = dirs::home_dir().expect("home directory should be available");

    assert_eq!(
        CookieSource::Brave.cookie_path().unwrap(),
        app_support.join("BraveSoftware/Brave-Browser/Default/Cookies")
    );
    assert_eq!(
        CookieSource::Chrome.cookie_path().unwrap(),
        app_support.join("Google/Chrome/Default/Cookies")
    );
    assert_eq!(
        CookieSource::Firefox.cookie_path().unwrap(),
        app_support.join("Firefox/Profiles")
    );
    assert_eq!(
        CookieSource::Safari.cookie_path().unwrap(),
        home.join("Library/Cookies/Cookies.binarycookies")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cookie_paths_use_linux_locations() {
    let config_dir = dirs::config_dir().expect("Linux should expose ~/.config");
    let home = dirs::home_dir().expect("home directory should be available");

    assert_eq!(
        CookieSource::Brave.cookie_path().unwrap(),
        config_dir.join("BraveSoftware/Brave-Browser/Default/Cookies")
    );
    assert_eq!(
        CookieSource::Chrome.cookie_path().unwrap(),
        config_dir.join("google-chrome/Default/Cookies")
    );
    assert_eq!(
        CookieSource::Firefox.cookie_path().unwrap(),
        home.join(".mozilla/firefox")
    );
    assert!(
        CookieSource::Safari.cookie_path().is_none(),
        "Safari should not advertise a Linux cookie store"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn cookie_paths_use_windows_locations() {
    let local_data = dirs::data_local_dir().expect("Windows should expose LocalAppData");
    let config_dir = dirs::config_dir().expect("Windows should expose AppData/Roaming");

    assert_eq!(
        CookieSource::Brave.cookie_path().unwrap(),
        local_data.join("BraveSoftware/Brave-Browser/User Data/Default/Cookies")
    );
    assert_eq!(
        CookieSource::Chrome.cookie_path().unwrap(),
        local_data.join("Google/Chrome/User Data/Default/Cookies")
    );
    assert_eq!(
        CookieSource::Firefox.cookie_path().unwrap(),
        config_dir.join("Mozilla/Firefox/Profiles")
    );
    assert!(
        CookieSource::Safari.cookie_path().is_none(),
        "Safari should not advertise a Windows cookie store"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_keychain_lookup_returns_fallback_error() {
    let err = CookieSource::Chrome
        .get_keychain_key_with_interaction(KeychainInteraction::Allow)
        .expect_err("non-macOS should not attempt native keychain lookup");
    assert!(
        err.to_string().contains("Python cookie fallback"),
        "error should direct callers toward the Python fallback: {err}"
    );
}

// ─── PBKDF2 key derivation ────────────────────────────────────────────────────

#[test]
fn derive_cookie_key_known_vector() {
    // GIVEN: password "peanuts" (known test vector matching Python browser_cookie3 output)
    let password = b"peanuts";

    // WHEN: key is derived with Chrome parameters
    let key = derive_cookie_key(password).expect("key derivation must succeed");

    // THEN: key matches the browser_cookie3-compatible reference bytes
    assert_eq!(key.len(), CHROME_KEY_LEN, "derived key must be 16 bytes");
    assert_eq!(hex::encode(key), "d9a09d499b4e1b7461f28e67972c6dbd");
}

#[test]
fn derive_cookie_key_empty_password_succeeds() {
    // GIVEN: empty password (edge case — Keychain could theoretically return empty)
    // WHEN: key is derived
    let key = derive_cookie_key(b"").expect("derivation must not panic on empty input");
    // THEN: still 16 bytes
    assert_eq!(key.len(), CHROME_KEY_LEN);
}

#[test]
fn derive_cookie_key_is_deterministic() {
    // GIVEN: same password
    let pw = b"my-brave-password";
    // WHEN: derived twice
    let k1 = derive_cookie_key(pw).unwrap();
    let k2 = derive_cookie_key(pw).unwrap();
    // THEN: identical
    assert_eq!(k1, k2);
}

// ─── AES-128-CBC decryption ───────────────────────────────────────────────────

#[test]
fn aes_iv_is_16_space_bytes_not_zero_bytes() {
    // GIVEN: the AES_CBC_IV constant
    // THEN: it must be 16 × 0x20 (space), matching Chromium's os_crypt_mac.mm
    assert_eq!(AES_CBC_IV, [0x20u8; 16], "IV must be 16 space bytes (0x20)");
    assert_ne!(AES_CBC_IV, [0u8; 16], "IV must NOT be zero bytes");
}

#[test]
fn decrypt_cookie_value_round_trip_simple() {
    // GIVEN: a known plaintext and derived key (no domain tag, schema < 24)
    let password = b"test-key";
    let key = derive_cookie_key(password).unwrap();
    let plaintext = b"session_token_abc123";
    let blob = encrypt_v10(plaintext, &key);

    // WHEN: decrypted without domain-tag stripping
    let result = decrypt_cookie_value(&blob, &key, false).expect("decryption must succeed");

    // THEN: plaintext recovered
    assert_eq!(result, "session_token_abc123");
}

#[test]
fn decrypt_cookie_value_round_trip_v24_domain_tag_stripped() {
    // GIVEN: v24+ blob with 32-byte SHA-256 prefix before the actual value
    let key = derive_cookie_key(b"brave-key").unwrap();
    let host = ".linkedin.com";
    let value = b"my_session_value";
    let blob = encrypt_v10_v24(value, &key, host);

    // WHEN: decrypted with has_domain_tag=true
    let result = decrypt_cookie_value(&blob, &key, true).unwrap();

    // THEN: only the actual value is returned, not the SHA-256 prefix
    assert_eq!(result, "my_session_value");
}

#[test]
fn decrypt_cookie_value_v24_without_tag_flag_returns_garbage() {
    // GIVEN: v24+ blob (SHA-256 prefix present)
    let key = derive_cookie_key(b"brave-key-2").unwrap();
    let blob = encrypt_v10_v24(b"val", &key, ".example.com");

    // WHEN: decrypted WITHOUT setting has_domain_tag (wrong flag)
    // THEN: result is either an error or contains the SHA-256 garbage bytes
    let result = decrypt_cookie_value(&blob, &key, false).unwrap_or_default();
    assert_ne!(
        result, "val",
        "domain tag must be stripped for correct result"
    );
}

#[test]
fn decrypt_cookie_value_round_trip_unicode() {
    // GIVEN: UTF-8 cookie value (no domain tag)
    let key = derive_cookie_key(b"unicode-test").unwrap();
    let plaintext = "café=résumé".as_bytes();
    let blob = encrypt_v10(plaintext, &key);

    // WHEN: decrypted
    let result = decrypt_cookie_value(&blob, &key, false).unwrap();

    // THEN: unicode preserved
    assert_eq!(result, "café=résumé");
}

#[test]
fn decrypt_cookie_value_round_trip_exactly_16_bytes() {
    // GIVEN: plaintext that is exactly 16 bytes (one full AES block, needs +1 padding block)
    let key = derive_cookie_key(b"block-aligned").unwrap();
    let plaintext = b"0123456789abcdef"; // exactly 16
    let blob = encrypt_v10(plaintext, &key);

    // WHEN: decrypted
    let result = decrypt_cookie_value(&blob, &key, false).unwrap();

    // THEN: exact match
    assert_eq!(result, "0123456789abcdef");
}

#[test]
fn decrypt_cookie_value_empty_blob_returns_error() {
    // GIVEN: empty input
    // WHEN: decryption attempted
    let err = decrypt_cookie_value(&[], &[0u8; 16], false).unwrap_err();
    // THEN: descriptive error
    assert!(
        err.to_string().contains("too short"),
        "error should mention too short: {err}"
    );
}

#[test]
fn decrypt_cookie_value_wrong_prefix_returns_error() {
    // GIVEN: blob with wrong prefix
    let mut blob = b"v11".to_vec();
    blob.extend_from_slice(&[0u8; 16]);

    // WHEN: decryption attempted
    let err = decrypt_cookie_value(&blob, &[0u8; 16], false).unwrap_err();

    // THEN: error mentions v10
    assert!(
        err.to_string().contains("v10"),
        "error should mention expected prefix: {err}"
    );
}

#[test]
fn decrypt_cookie_value_only_prefix_no_ciphertext_returns_error() {
    // GIVEN: only the v10 prefix, no ciphertext
    let blob = V10_PREFIX.to_vec();

    // WHEN: decryption attempted
    let err = decrypt_cookie_value(&blob, &[0u8; 16], false).unwrap_err();

    // THEN: error is descriptive
    assert!(!err.to_string().is_empty());
}

#[test]
fn decrypt_cookie_value_wrong_key_length_returns_error() {
    // GIVEN: blob with valid prefix but wrong key length
    let blob = encrypt_v10(b"hello", &[0u8; 16]);

    // WHEN: called with 32-byte key
    let err = decrypt_cookie_value(&blob, &[0u8; 32], false).unwrap_err();

    // THEN: error is descriptive
    assert!(!err.to_string().is_empty(), "should fail: {err}");
}

#[test]
fn decrypt_cookie_value_v24_too_short_for_domain_tag_returns_error() {
    // GIVEN: a valid AES-CBC blob that decrypts to fewer than 32 bytes
    let key = derive_cookie_key(b"short-test").unwrap();
    let blob = encrypt_v10(b"tiny", &key);

    // WHEN: decoded with has_domain_tag=true
    let err = decrypt_cookie_value(&blob, &key, true).unwrap_err();

    // THEN: error mentions the domain tag being too short
    assert!(
        err.to_string().contains("too short"),
        "error should mention too short for domain tag: {err}"
    );
}

// ─── Domain condition builder ─────────────────────────────────────────────────

#[test]
fn build_domain_conditions_includes_exact_and_parent() {
    // GIVEN: subdomain
    let conds = build_domain_conditions("login.example.com");

    // THEN: exact match + dotted variants present
    assert!(conds.iter().any(|c| c.contains("'login.example.com'")));
    assert!(conds.iter().any(|c| c.contains("'.login.example.com'")));
    assert!(conds.iter().any(|c| c.contains("'.example.com'")));
    assert!(conds.iter().any(|c| c.contains("'.com'")));
}

#[test]
fn build_domain_conditions_apex_domain() {
    // GIVEN: apex domain (no subdomain)
    let conds = build_domain_conditions("example.com");

    // THEN: exact + dotted apex
    assert!(conds.iter().any(|c| c.contains("'example.com'")));
    assert!(conds.iter().any(|c| c.contains("'.example.com'")));
}

#[test]
fn build_domain_conditions_apex_domain_includes_www_variants() {
    let conds = build_domain_conditions("linkedin.com");

    assert!(conds.contains(&"host_key = 'linkedin.com'".to_string()));
    assert!(conds.contains(&"host_key = '.linkedin.com'".to_string()));
    assert!(conds.contains(&"host_key = 'www.linkedin.com'".to_string()));
    assert!(conds.contains(&"host_key = '.www.linkedin.com'".to_string()));
}

#[test]
fn build_domain_conditions_www_domain_includes_apex_variants() {
    let conds = build_domain_conditions("www.linkedin.com");

    assert!(conds.contains(&"host_key = 'www.linkedin.com'".to_string()));
    assert!(conds.contains(&"host_key = '.www.linkedin.com'".to_string()));
    assert!(conds.contains(&"host_key = 'linkedin.com'".to_string()));
    assert!(conds.contains(&"host_key = '.linkedin.com'".to_string()));
}

// ─── parse_cookie_rows ────────────────────────────────────────────────────────

#[test]
fn parse_cookie_rows_plaintext_value() {
    // GIVEN: tab-separated row with a plaintext value and empty hex blob
    let input = "session_id\tabc123\t\n";

    // WHEN: parsed
    let rows = parse_cookie_rows(input);

    // THEN: one row, value set, no encrypted bytes
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "session_id");
    assert_eq!(rows[0].value, "abc123");
    assert!(rows[0].encrypted_bytes.is_empty());
}

#[test]
fn parse_cookie_rows_hex_encrypted_value() {
    // GIVEN: tab-separated row with empty value and hex-encoded encrypted blob
    // "v10" = 76 31 30
    let hex = "763130";
    let input = format!("token\t\t{hex}\n");

    // WHEN: parsed
    let rows = parse_cookie_rows(&input);

    // THEN: encrypted_bytes decoded correctly
    assert_eq!(rows[0].encrypted_bytes, b"v10");
}

#[test]
fn parse_cookie_rows_malformed_lines_skipped() {
    // GIVEN: mix of valid and invalid lines
    let input = "good\tvalue\t\nno_tab_here\ngood2\tval2\t\n";

    // WHEN: parsed
    let rows = parse_cookie_rows(input);

    // THEN: only 2 valid rows
    assert_eq!(rows.len(), 2);
}

// ─── decrypt_rows ─────────────────────────────────────────────────────────────

#[test]
fn decrypt_rows_plaintext_passthrough() {
    // GIVEN: rows with only plaintext values (no encryption)
    let rows = vec![
        CookieRow {
            name: "a".into(),
            value: "v1".into(),
            encrypted_bytes: vec![],
        },
        CookieRow {
            name: "b".into(),
            value: "v2".into(),
            encrypted_bytes: vec![],
        },
    ];

    // WHEN: decrypted with no key
    let result = decrypt_rows(rows, None, false);

    // THEN: both cookies present
    assert_eq!(result["a"], "v1");
    assert_eq!(result["b"], "v2");
}

#[test]
fn decrypt_rows_encrypted_without_key_is_skipped() {
    // GIVEN: encrypted row but no key provided
    let key = derive_cookie_key(b"skip-test").unwrap();
    let blob = encrypt_v10(b"secret", &key);
    let rows = vec![CookieRow {
        name: "tok".into(),
        value: String::new(),
        encrypted_bytes: blob,
    }];

    // WHEN: decrypted without key
    let result = decrypt_rows(rows, None, false);

    // THEN: cookie skipped, not present
    assert!(!result.contains_key("tok"));
}

#[test]
fn decrypt_rows_encrypted_with_correct_key_schema_pre24() {
    // GIVEN: encrypted row, schema < 24 (no domain tag), with correct key
    let key = derive_cookie_key(b"my-browser-password").unwrap();
    let blob = encrypt_v10(b"my_session_value", &key);
    let rows = vec![CookieRow {
        name: "session".into(),
        value: String::new(),
        encrypted_bytes: blob,
    }];

    // WHEN: decrypted with key, has_domain_tag=false
    let result = decrypt_rows(rows, Some(&key), false);

    // THEN: value recovered
    assert_eq!(result["session"], "my_session_value");
}

#[test]
fn decrypt_rows_encrypted_with_correct_key_schema_v24() {
    // GIVEN: v24+ encrypted row (SHA-256 domain prefix present), correct key
    let key = derive_cookie_key(b"brave-real-password").unwrap();
    let blob = encrypt_v10_v24(b"real_cookie_value", &key, ".example.com");
    let rows = vec![CookieRow {
        name: "auth".into(),
        value: String::new(),
        encrypted_bytes: blob,
    }];

    // WHEN: decrypted with key and has_domain_tag=true (v24+ path)
    let result = decrypt_rows(rows, Some(&key), true);

    // THEN: domain prefix stripped, actual value recovered
    assert_eq!(result["auth"], "real_cookie_value");
}
