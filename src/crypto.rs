//! At-rest encryption for `secret = true` outputs.
//!
//! Marker format: `<blue:enc:v1:HMAC_HEX:BASE64_AGE_CIPHERTEXT>`
//!
//! The HMAC is a fingerprint for change-detection in diffs, NOT a
//! confidentiality boundary — it is HMAC-SHA256 keyed by a public salt
//! (`{resource_name}.{field_path}`), so equal plaintexts at the same salt
//! produce equal fingerprints. That's intentional: it lets the diff engine
//! compare two encrypted values without decrypting them.

use std::env;
use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Wrapping marker tokens. The version segment lets us evolve the format
/// without ambiguity later — a future `v2` would be parsed by a separate
/// branch and rejected by this version's decrypt path.
pub const MARKER_PREFIX: &str = "<blue:enc:v1:";
pub const MARKER_SUFFIX: &str = ">";

/// True iff `s` looks like our v1 marker. Used by the state walker to
/// decide whether to attempt decrypt; non-marker strings pass through
/// untouched (so cleartext state files keep working).
pub fn is_marker(s: &str) -> bool {
    s.starts_with(MARKER_PREFIX) && s.ends_with(MARKER_SUFFIX)
}

/// Parse raw recipient strings into typed X25519 recipients. Each error
/// names the offending string so the user can find it in their config.
/// Empty input returns an empty Vec (callers decide whether that's OK).
pub fn parse_recipients(raw: &[String]) -> Result<Vec<age::x25519::Recipient>, String> {
    raw.iter()
        .map(|s| {
            s.parse::<age::x25519::Recipient>()
                .map_err(|e| format!("invalid recipient '{s}': {e}"))
        })
        .collect()
}

/// Load decrypt identities from environment. `BLUE_AGE_IDENTITY` points to
/// a file (one or more identities, age-keygen format); `BLUE_AGE_IDENTITY_KEY`
/// is a literal `AGE-SECRET-KEY-1...` string. Both may be set; identities
/// from both sources are concatenated. Returns Ok(empty) if neither is set —
/// callers decide whether the absence is fatal (it is for state with
/// markers; not for fresh state).
pub fn load_identities() -> Result<Vec<Box<dyn age::Identity>>, String> {
    let mut out: Vec<Box<dyn age::Identity>> = Vec::new();

    if let Ok(path) = env::var("BLUE_AGE_IDENTITY") {
        let id_file = age::IdentityFile::from_file(path.clone())
            .map_err(|e| format!("failed to read BLUE_AGE_IDENTITY file '{path}': {e}"))?;
        let ids = id_file
            .into_identities()
            .map_err(|e| format!("failed to parse identities in '{path}': {e}"))?;
        out.extend(ids);
    }

    if let Ok(key_str) = env::var("BLUE_AGE_IDENTITY_KEY") {
        let id: age::x25519::Identity = key_str
            .parse()
            .map_err(|e| format!("BLUE_AGE_IDENTITY_KEY is not a valid age identity: {e}"))?;
        out.push(Box::new(id));
    }

    Ok(out)
}

/// Encrypt `plaintext` to `recipients`, producing a v1 marker string.
/// `salt` is `{resource_name}.{field_path}` — used as the HMAC key for
/// the fingerprint (see module docs).
pub fn encrypt_value(
    salt: &str,
    plaintext: &str,
    recipients: &[age::x25519::Recipient],
) -> Result<String, String> {
    if recipients.is_empty() {
        return Err("encrypt_value called with no recipients".to_string());
    }

    let fingerprint = compute_hmac_fingerprint(salt, plaintext);

    let recipient_refs: Vec<&dyn age::Recipient> = recipients
        .iter()
        .map(|r| r as &dyn age::Recipient)
        .collect();

    let encryptor = age::Encryptor::with_recipients(recipient_refs.into_iter())
        .map_err(|e| format!("age encryptor build failed: {e}"))?;

    let mut ciphertext: Vec<u8> = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|e| format!("age wrap_output failed: {e}"))?;
    writer
        .write_all(plaintext.as_bytes())
        .map_err(|e| format!("age write failed: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("age finish failed: {e}"))?;

    let b64 = BASE64.encode(&ciphertext);
    Ok(format!("{MARKER_PREFIX}{fingerprint}:{b64}{MARKER_SUFFIX}"))
}

/// Decrypt a v1 marker string. Returns Err if `marker` isn't our format,
/// the version doesn't match, the base64 is malformed, or no identity
/// can decrypt (each identity is tried in turn — age handles this when
/// passed all of them at once).
pub fn decrypt_value(
    marker: &str,
    identities: &[Box<dyn age::Identity>],
) -> Result<String, String> {
    let inner = marker
        .strip_prefix(MARKER_PREFIX)
        .and_then(|s| s.strip_suffix(MARKER_SUFFIX))
        .ok_or_else(|| "value is not a blue v1 encrypted marker".to_string())?;

    // inner = "HMAC_HEX:B64". Take only the *last* colon as the separator
    // (HMAC hex never contains a colon; base64 also never does — but being
    // explicit avoids surprises if the format ever sprouts more fields).
    let (_fingerprint, b64) = inner
        .split_once(':')
        .ok_or_else(|| "malformed marker: missing fingerprint:ciphertext separator".to_string())?;

    let ciphertext = BASE64
        .decode(b64.as_bytes())
        .map_err(|e| format!("malformed marker: base64 decode failed: {e}"))?;

    let decryptor = age::Decryptor::new(&ciphertext[..])
        .map_err(|e| format!("age decryptor build failed: {e}"))?;

    let identity_refs: Vec<&dyn age::Identity> = identities
        .iter()
        .map(|i| i.as_ref() as &dyn age::Identity)
        .collect();

    let mut reader = decryptor
        .decrypt(identity_refs.into_iter())
        .map_err(|e| format!("age decrypt failed (no matching identity?): {e}"))?;

    let mut plaintext = String::new();
    reader
        .read_to_string(&mut plaintext)
        .map_err(|e| format!("age read failed: {e}"))?;

    Ok(plaintext)
}

/// HMAC-SHA256 keyed by `salt`, hex-encoded. Fingerprint only — see module
/// docs. Used to (a) detect when a stored encrypted value's plaintext has
/// changed without decrypting, (b) bind the ciphertext to its location
/// (resource + field) so a copy-paste between fields is detectable.
fn compute_hmac_fingerprint(salt: &str, plaintext: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(salt.as_bytes())
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(plaintext.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;
    use std::sync::Mutex;

    /// Tests that mutate process-wide env vars must run serially.
    /// Rust runs tests in parallel by default; without this lock, two
    /// tests setting `BLUE_AGE_IDENTITY` at the same time would race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn gen_identity() -> (age::x25519::Identity, age::x25519::Recipient) {
        let id = age::x25519::Identity::generate();
        let recipient = id.to_public();
        (id, recipient)
    }

    #[test]
    fn encrypt_then_decrypt_roundtrip() {
        let (id, recipient) = gen_identity();
        let marker = encrypt_value("res.field", "hello world", &[recipient]).unwrap();
        assert!(is_marker(&marker), "expected marker format, got: {marker}");

        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        let plaintext = decrypt_value(&marker, &identities).unwrap();
        assert_eq!(plaintext, "hello world");
    }

    #[test]
    fn multi_recipient_each_identity_can_decrypt() {
        // Two recipients, two identities. Encrypt once with both; each
        // identity should be able to decrypt independently.
        let (id_a, rec_a) = gen_identity();
        let (id_b, rec_b) = gen_identity();

        let marker = encrypt_value("res.field", "shared secret", &[rec_a, rec_b]).unwrap();

        let only_a: Vec<Box<dyn age::Identity>> = vec![Box::new(id_a)];
        assert_eq!(decrypt_value(&marker, &only_a).unwrap(), "shared secret");

        let only_b: Vec<Box<dyn age::Identity>> = vec![Box::new(id_b)];
        assert_eq!(decrypt_value(&marker, &only_b).unwrap(), "shared secret");
    }

    #[test]
    fn decrypt_rejects_non_marker() {
        let (id, _) = gen_identity();
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        let err = decrypt_value("plain string", &identities).unwrap_err();
        assert!(err.contains("not a blue v1 encrypted marker"), "got: {err}");
    }

    #[test]
    fn decrypt_rejects_wrong_version() {
        let (id, _) = gen_identity();
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        // Future v2 marker — current code rejects on the prefix check.
        let err = decrypt_value("<blue:enc:v2:abc:def>", &identities).unwrap_err();
        assert!(err.contains("not a blue v1 encrypted marker"), "got: {err}");
    }

    #[test]
    fn decrypt_rejects_malformed_b64() {
        let (id, _) = gen_identity();
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        let err = decrypt_value("<blue:enc:v1:abc:not!!!base64>", &identities).unwrap_err();
        assert!(err.contains("base64"), "got: {err}");
    }

    #[test]
    fn decrypt_rejects_missing_separator() {
        let (id, _) = gen_identity();
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        // No colon between fingerprint and ciphertext.
        let err = decrypt_value("<blue:enc:v1:nofingerprintseparator>", &identities).unwrap_err();
        assert!(
            err.contains("separator") || err.contains("base64"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_recipients_accepts_valid() {
        let (_id, recipient) = gen_identity();
        let raw = vec![recipient.to_string()];
        let parsed = parse_recipients(&raw).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_recipients_rejects_garbage() {
        let raw = vec!["age1notreal".to_string()];
        let err = parse_recipients(&raw).unwrap_err();
        assert!(err.contains("age1notreal"), "got: {err}");
    }

    #[test]
    fn parse_recipients_empty_returns_empty() {
        let parsed = parse_recipients(&[]).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn is_marker_distinguishes_plaintext() {
        // is_marker is a cheap shape check (prefix + suffix only); the
        // full validation lives in decrypt_value. Anything without the
        // <blue:enc:v1:...> shape must be rejected here.
        assert!(!is_marker(""));
        assert!(!is_marker("hello"));
        assert!(!is_marker("<blue:enc:v0:abc:def>"));
        assert!(!is_marker("blue:enc:v1:abc:def>")); // missing leading <
    }

    #[test]
    fn is_marker_shape_check_accepts_well_formed() {
        let (_id, recipient) = gen_identity();
        let marker = encrypt_value("res.field", "x", &[recipient]).unwrap();
        assert!(is_marker(&marker));
    }

    #[test]
    fn fingerprint_changes_with_plaintext() {
        let a = compute_hmac_fingerprint("res.field", "value-1");
        let b = compute_hmac_fingerprint("res.field", "value-2");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_changes_with_salt() {
        let a = compute_hmac_fingerprint("res.field_a", "value");
        let b = compute_hmac_fingerprint("res.field_b", "value");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_stable_across_calls() {
        let a = compute_hmac_fingerprint("res.field", "value");
        let b = compute_hmac_fingerprint("res.field", "value");
        assert_eq!(a, b);
    }

    #[test]
    fn load_identities_from_file_env_var() {
        let _g = ENV_LOCK.lock().unwrap();
        let id = age::x25519::Identity::generate();
        let id_str = id.to_string();

        let tmp =
            std::path::PathBuf::from(format!("/tmp/blue_test_id_{}.txt", uuid::Uuid::new_v4()));
        let mut f = std::fs::File::create(&tmp).unwrap();
        // age-keygen identity files allow comment lines starting with '#';
        // include one so we exercise the comment-tolerant parsing path.
        writeln!(f, "# created: 2026-01-01T00:00:00Z").unwrap();
        writeln!(f, "{}", id_str.expose_secret()).unwrap();
        drop(f);

        unsafe {
            std::env::set_var("BLUE_AGE_IDENTITY", &tmp);
            std::env::remove_var("BLUE_AGE_IDENTITY_KEY");
        }
        let ids = load_identities().unwrap();
        unsafe {
            std::env::remove_var("BLUE_AGE_IDENTITY");
        }
        std::fs::remove_file(&tmp).ok();

        assert_eq!(ids.len(), 1, "expected one identity loaded from file");
    }

    #[test]
    fn load_identities_from_literal_env_var() {
        let _g = ENV_LOCK.lock().unwrap();
        let id = age::x25519::Identity::generate();
        let id_str = id.to_string();

        unsafe {
            std::env::remove_var("BLUE_AGE_IDENTITY");
            std::env::set_var("BLUE_AGE_IDENTITY_KEY", id_str.expose_secret());
        }
        let ids = load_identities().unwrap();
        unsafe {
            std::env::remove_var("BLUE_AGE_IDENTITY_KEY");
        }

        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn load_identities_returns_empty_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("BLUE_AGE_IDENTITY");
            std::env::remove_var("BLUE_AGE_IDENTITY_KEY");
        }
        let ids = load_identities().unwrap();
        assert!(ids.is_empty());
    }
}
