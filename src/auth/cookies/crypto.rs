//! AES-128-CBC cookie decryption and PBKDF2 key derivation for Chromium browsers.
//!
//! # macOS Cookie Decryption
//!
//! Brave and Chrome encrypt cookies with AES-128-CBC:
//! - Key: PBKDF2-SHA1(`keychain_password`, salt=`"saltysalt"`, iterations=1003, 16 bytes)
//! - IV: 16 **space** bytes (`0x20 * 16`) — NOT zero bytes
//! - Ciphertext: `encrypted_value[3..]` (first 3 bytes are the `"v10"` prefix)
//! - Padding: PKCS7
//!
//! ## Schema v24+ domain-integrity prefix
//!
//! Starting with Chromium cookie DB schema version 24 (Chrome 130+, Brave 1.70+),
//! the first 32 bytes of every decrypted plaintext are `SHA-256(host_key)`.
//! These bytes must be stripped to recover the actual cookie value.
//! See: <https://issues.chromium.org/issues/40185252>
//!
//! # Security Notice — Post-Quantum Cryptography (PQC)
//!
//! **This entire module implements a read-only compatibility shim for Chromium's
//! on-disk cookie format. nab does not choose these algorithms — Chromium does.**
//!
//! The algorithms used here are NOT post-quantum safe:
//! - `PBKDF2-HMAC-SHA1` — classical KDF, no quantum resistance
//! - `AES-128-CBC` — 128-bit key provides only ~64 bits of security against
//!   Grover's algorithm on a large-scale quantum computer
//! - Static IV (`0x20 * 16`) — hardcoded by Chromium; CBC with a fixed IV is
//!   additionally vulnerable to chosen-plaintext attacks
//!
//! **These weaknesses cannot be fixed here.** The format is owned by the Chromium
//! project. Any PQC upgrade must originate in Chromium's `os_crypt` component.
//! Track upstream progress at: <https://issues.chromium.org/issues/40185252>
//!
//! nab-owned session persistence is handled separately in [`crate::session`] and
//! uses Argon2id-derived AES-256-GCM. This module remains strictly for Chromium
//! compatibility.

use anyhow::{Context, Result};

// ─── Constants ────────────────────────────────────────────────────────────────

// LEGACY: Chromium compatibility constants — these values are dictated by the Chromium
// on-disk cookie format and CANNOT be changed by nab.
// AES-128-CBC + PBKDF2-SHA1 are NOT post-quantum safe. See module-level doc for details.

/// PBKDF2 salt used by Chromium for cookie key derivation.
// LEGACY: Chromium compatibility — "saltysalt" is hard-coded in Chromium's os_crypt_mac.mm.
pub(super) const CHROME_PBKDF2_SALT: &[u8] = b"saltysalt";
/// PBKDF2 iteration count (1003 for macOS Chromium builds).
// LEGACY: Chromium compatibility — 1003 iterations of PBKDF2-HMAC-SHA1 is NOT
// post-quantum safe and is well below NIST SP 800-132 recommendations (≥210,000 for SHA-256).
pub(super) const CHROME_PBKDF2_ITERATIONS: u32 = 1003;
/// Derived key length in bytes (AES-128 = 16 bytes).
// LEGACY: Chromium compatibility — AES-128 provides only ~64 bits of post-quantum security
// (Grover's algorithm). AES-256 would be required for PQC readiness.
pub(super) const CHROME_KEY_LEN: usize = 16;
/// Prefix on every v10 encrypted cookie value (ASCII "v10").
pub(super) const V10_PREFIX: &[u8; 3] = b"v10";
/// AES-CBC IV: 16 **space** bytes (0x20). Chromium hard-codes this — NOT zero bytes.
/// Reference: `chromium/components/os_crypt/os_crypt_mac.mm`, `OSCryptImpl::DecryptString`.
// LEGACY: Chromium compatibility — a static, non-random IV is a known weakness of CBC mode.
// This makes the scheme additionally vulnerable to chosen-plaintext attacks beyond the
// baseline PQC concerns. Fixing this requires a Chromium format change.
pub(super) const AES_CBC_IV: [u8; 16] = [b' '; 16];
/// Domain-integrity prefix length added in DB schema v24+.
/// First 32 bytes of every decrypted value are `SHA-256(host_key)`.
pub(super) const DOMAIN_SHA256_LEN: usize = 32;
/// Minimum Chromium cookie DB schema version that prepends a SHA-256 domain tag.
pub(super) const SCHEMA_VERSION_WITH_DOMAIN_TAG: u32 = 24;

// ─── Key derivation ───────────────────────────────────────────────────────────

/// Derive the 16-byte AES key from the raw Keychain password using PBKDF2-SHA1.
///
/// This is the exact derivation used by all Chromium-based browsers on macOS:
/// `PBKDF2(password, salt="saltysalt", iterations=1003, key_len=16, prf=HMAC-SHA1)`
///
/// # Security Notice — Post-Quantum Cryptography (PQC)
///
/// **LEGACY: Chromium compatibility — AES-128-CBC is NOT post-quantum safe.**
///
/// This function implements the key derivation step of Chromium's cookie encryption
/// scheme. The parameters — PBKDF2-HMAC-SHA1, 1003 iterations, 16-byte key — are
/// fixed by the Chromium on-disk format and cannot be changed here.
///
/// For PQC-safe key derivation in new nab-native code, use Argon2id with a 256-bit
/// output key, or PBKDF2-HMAC-SHA256 with ≥210,000 iterations (NIST SP 800-132).
pub fn derive_cookie_key(password: &[u8]) -> Result<Vec<u8>> {
    use hmac::{Hmac, KeyInit, Mac};
    use sha1::Sha1;

    anyhow::ensure!(
        CHROME_PBKDF2_ITERATIONS > 0,
        "PBKDF2 key derivation requires at least one iteration"
    );

    let mut key = vec![0u8; CHROME_KEY_LEN];
    let mut offset = 0;
    let mut block_index = 1u32;

    while offset < key.len() {
        let mut salted_block = Vec::with_capacity(CHROME_PBKDF2_SALT.len() + 4);
        salted_block.extend_from_slice(CHROME_PBKDF2_SALT);
        salted_block.extend_from_slice(&block_index.to_be_bytes());

        let mut mac = Hmac::<Sha1>::new_from_slice(password)
            .map_err(|e| anyhow::anyhow!("HMAC key setup failed: {e}"))?;
        mac.update(&salted_block);

        let mut block = mac.finalize().into_bytes();
        let mut u = block;

        for _ in 1..CHROME_PBKDF2_ITERATIONS {
            let mut mac = Hmac::<Sha1>::new_from_slice(password)
                .map_err(|e| anyhow::anyhow!("HMAC key setup failed: {e}"))?;
            mac.update(&u);
            u = mac.finalize().into_bytes();

            for (lhs, rhs) in block.iter_mut().zip(u.iter()) {
                *lhs ^= *rhs;
            }
        }

        let take = (key.len() - offset).min(block.len());
        key[offset..offset + take].copy_from_slice(&block[..take]);
        offset += take;
        block_index = block_index
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("PBKDF2 block index overflow"))?;
    }

    Ok(key)
}

// ─── Decryption ───────────────────────────────────────────────────────────────

/// Decrypt a single AES-128-CBC encrypted cookie blob.
///
/// # Format (macOS Chromium v10 cookies)
/// ```text
/// [ 'v' | '1' | '0' | ciphertext... ]
///   3 bytes prefix     N bytes (must be a nonzero multiple of 16)
/// ```
///
/// After PKCS7 unpadding the plaintext layout depends on the DB schema version:
/// - **Schema < 24**: `[actual_cookie_value]`
/// - **Schema ≥ 24**: `[SHA-256(host_key) 32 bytes][actual_cookie_value]`
///
/// Pass `has_domain_tag = true` for schema v24+ databases.
///
/// # Security Notice — Post-Quantum Cryptography (PQC)
///
/// **LEGACY: Chromium compatibility — AES-128-CBC is NOT post-quantum safe.**
///
/// This function decrypts cookies using Chromium's on-disk format. The algorithm
/// (AES-128-CBC, static IV, PKCS7 padding) is owned by the Chromium project.
/// nab cannot change these without breaking cookie extraction entirely.
///
/// Do NOT use this function or copy its algorithm for any nab-native data storage.
/// For nab-native encryption, use the AES-256-GCM session storage in
/// [`crate::session`] with a random 96-bit nonce.
///
/// # Errors
/// Returns an error if the blob is too short, the prefix is wrong, AES
/// decryption/unpadding fails, or the result is not valid UTF-8.
pub fn decrypt_cookie_value(encrypted: &[u8], key: &[u8], has_domain_tag: bool) -> Result<String> {
    // LEGACY: Chromium compatibility — AES-128-CBC is NOT post-quantum safe.
    // This cipher + IV combination is fixed by the Chromium on-disk cookie format.
    // Reference: chromium/components/os_crypt/os_crypt_mac.mm OSCryptImpl::DecryptString
    use aes::Aes128;
    use cbc::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcDec = cbc::Decryptor<Aes128>;

    anyhow::ensure!(
        encrypted.len() > V10_PREFIX.len(),
        "Encrypted blob too short ({} bytes)",
        encrypted.len()
    );
    anyhow::ensure!(
        encrypted.starts_with(V10_PREFIX),
        "Unexpected cookie prefix (expected v10, got {:?})",
        &encrypted[..V10_PREFIX.len()]
    );
    anyhow::ensure!(key.len() == CHROME_KEY_LEN, "Key must be 16 bytes");

    let ciphertext = &encrypted[V10_PREFIX.len()..];
    anyhow::ensure!(
        !ciphertext.is_empty() && ciphertext.len().is_multiple_of(16),
        "Ciphertext length {} is not a nonzero multiple of 16",
        ciphertext.len()
    );

    let decryptor = Aes128CbcDec::new_from_slices(key, &AES_CBC_IV)
        .map_err(|e| anyhow::anyhow!("AES key/IV setup failed: {e}"))?;

    let mut buf = ciphertext.to_vec();
    let plaintext = decryptor
        .decrypt_padded::<Pkcs7>(&mut buf)
        .map_err(|e| anyhow::anyhow!("AES-CBC unpadding failed: {e}"))?;

    let value_bytes = if has_domain_tag {
        anyhow::ensure!(
            plaintext.len() >= DOMAIN_SHA256_LEN,
            "Decrypted blob too short for domain tag ({} bytes)",
            plaintext.len()
        );
        &plaintext[DOMAIN_SHA256_LEN..]
    } else {
        plaintext
    };

    String::from_utf8(value_bytes.to_vec()).context("Decrypted cookie value is not valid UTF-8")
}
