//! Activation keys and signed licence tokens.
//!
//! Two different strings travel between here and an installation, and
//! confusing them is the whole security model:
//!
//! - The **activation key** is a shared secret the partner hands to the
//!   customer. It authenticates a `POST /v1/license/check`. Only its BLAKE3
//!   and its first characters are stored, the same contract Laysi One's
//!   `api_token` keeps.
//! - The **licence token** is what this server hands back: a payload signed
//!   with Ed25519. The installation verifies it against a public key compiled
//!   into the product, so a lapsed network connection does not stop the books
//!   working and a forged row in the customer's own database does not pass.
//!
//! The token is `base64url(payload).base64url(signature)`, and the signature
//! is over the base64url text rather than over the JSON. There is then no
//! canonical-form question: the bytes that were signed are the bytes on the
//! wire.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use chrono::{DateTime, NaiveDate, Utc};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// How long one successful check is good for.
///
/// The installation counts down from `not_after` on its own, so this is also
/// how long a site may stay off the network before it starts warning. Long
/// enough that a fortnight of holidays or a firewall change is not an
/// incident; short enough that a revoked licence stops working inside two
/// months without anybody visiting the site.
pub const LEASE_DAYS: i64 = 45;

/// What the installation is told, and what it can prove without us.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicensePayload {
    pub license_no: String,
    /// The installation this token was minted for. Carried inside the
    /// signature so that copying the token, or the whole database it sits in,
    /// onto a second machine produces a token that machine can see is not
    /// its own.
    pub fingerprint: String,
    pub install_id: String,
    pub licensee: String,
    pub licensee_tax_id: String,
    /// Display name of the partner that sold the licence. The screen names
    /// them because a customer with a problem should be able to read who to
    /// call off the same page that reports the problem.
    pub partner: String,
    pub partner_no: String,
    pub issued_at: DateTime<Utc>,
    /// The date this token stops being believed. One date decides the whole
    /// state machine on the installation side; `expires_on` below is shown to
    /// a person and decides nothing.
    pub not_after: NaiveDate,
    /// End of the licence year the partner sold.
    pub expires_on: NaiveDate,
}

/// Sign a payload into the token an installation stores.
pub fn mint(key: &SigningKey, payload: &LicensePayload) -> Result<String> {
    let json = serde_json::to_vec(payload).context("serialising the licence payload")?;
    let body = B64.encode(json);
    let signature = key.sign(body.as_bytes());
    let token = format!("{body}.{}", B64.encode(signature.to_bytes()));
    // Read back what was just written. A signing key that loaded wrong should
    // fail here, in front of the person issuing the licence, rather than at
    // the customer's site where nobody can tell a bad key from a bad network.
    verify(&key.verifying_key(), &token).context("the token just minted does not verify")?;
    Ok(token)
}

/// Verify a token and return what it says. Used by the offline signing tool
/// to check its own output; the product has its own copy of this half.
pub fn verify(key: &VerifyingKey, token: &str) -> Result<LicensePayload> {
    let (body, sig) = token
        .split_once('.')
        .ok_or_else(|| anyhow!("licence token has no signature part"))?;
    let sig: [u8; 64] = B64
        .decode(sig)
        .context("decoding the signature")?
        .try_into()
        .map_err(|_| anyhow!("signature is not 64 bytes"))?;
    key.verify_strict(body.as_bytes(), &sig.into())
        .context("signature does not verify")?;
    let json = B64.decode(body).context("decoding the payload")?;
    serde_json::from_slice(&json).context("parsing the payload")
}

// Activation keys

/// Alphabet for the activation key: Crockford base32 without the four
/// characters a person reads back wrong (I, L, O, U). The key gets read over
/// the phone and typed into a browser, so the shape matters more than the
/// density.
const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A fresh activation key, `LAYSI-XXXXX-XXXXX-XXXXX-XXXXX`.
///
/// 20 characters out of a 32-character alphabet is 100 bits, drawn from the
/// OS. Rejection sampling rather than `% 32` so every character is uniform.
pub fn new_activation_key() -> String {
    let mut out = String::from("LAYSI");
    let mut buf = [0u8; 64];
    let mut taken = 0;
    while taken < 20 {
        getrandom::fill(&mut buf).expect("the OS random source is unavailable");
        for byte in buf {
            if taken == 20 {
                break;
            }
            // 248 = 31 * 8: the tail of the byte range would favour the first
            // eight letters, so it is drawn again instead.
            if byte >= 248 {
                continue;
            }
            if taken % 5 == 0 {
                out.push('-');
            }
            out.push(ALPHABET[(byte % 32) as usize] as char);
            taken += 1;
        }
    }
    out
}

/// The comparable form of whatever the customer typed.
///
/// Case and separators are presentation: someone will paste it with spaces,
/// someone else will type it in lower case, and both are the same key.
pub fn normalise_key(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// What is stored instead of the key itself.
pub fn key_hash(raw: &str) -> String {
    blake3::hash(normalise_key(raw).as_bytes()).to_hex().to_string()
}

/// The part of a key that goes on a screen, so a person can tell two rows
/// apart without either of them being usable.
pub fn key_prefix(raw: &str) -> String {
    normalise_key(raw).chars().take(10).collect()
}

/// A fresh continuity nonce.
///
/// Handed out with every answer and required on the next check. Two copies of
/// the same database each invalidate the other's, which is the only in-band
/// signal that a byte-for-byte clone exists at all.
pub fn new_nonce() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source is unavailable");
    hex::encode(bytes)
}

/// Load the signing key from its file: 32 bytes of seed, hex encoded.
pub fn signing_key_from_hex(text: &str) -> Result<SigningKey> {
    let raw = hex::decode(text.trim()).context("the signing key file is not hex")?;
    let seed: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow!("the signing key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(key: &SigningKey) -> (LicensePayload, String) {
        let payload = LicensePayload {
            license_no: "L-2609-0001".into(),
            fingerprint: "a".repeat(64),
            install_id: "8b1f6f3e-0000-4000-8000-000000000000".into(),
            licensee: "測試股份有限公司".into(),
            licensee_tax_id: "12345678".into(),
            partner: "示範資訊".into(),
            partner_no: "P-001".into(),
            issued_at: Utc::now(),
            not_after: NaiveDate::from_ymd_opt(2026, 10, 17).unwrap(),
            expires_on: NaiveDate::from_ymd_opt(2027, 9, 1).unwrap(),
        };
        let token = mint(key, &payload).expect("應簽得出來");
        (payload, token)
    }

    #[test]
    fn a_minted_token_verifies_and_a_tampered_one_does_not() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let (payload, token) = sample(&key);

        let read = verify(&key.verifying_key(), &token).expect("應驗得過");
        assert_eq!(read.license_no, payload.license_no);
        assert_eq!(read.not_after, payload.not_after);

        // Extending the expiry by editing the payload has to fail: that is
        // the only thing standing between a customer's own database and a
        // licence that never ends.
        let (body, sig) = token.split_once('.').unwrap();
        let mut edited: LicensePayload =
            serde_json::from_slice(&B64.decode(body).unwrap()).unwrap();
        edited.not_after = NaiveDate::from_ymd_opt(2099, 1, 1).unwrap();
        let forged = format!("{}.{sig}", B64.encode(serde_json::to_vec(&edited).unwrap()));
        assert!(verify(&key.verifying_key(), &forged).is_err(), "改過的內容不得驗過");

        // Nor may another key's signature pass.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        assert!(verify(&other.verifying_key(), &token).is_err(), "別的金鑰不得驗過");
    }

    #[test]
    fn activation_keys_are_typeable_and_compare_after_normalising() {
        let key = new_activation_key();
        assert_eq!(key.len(), "LAYSI-XXXXX-XXXXX-XXXXX-XXXXX".len());
        assert!(key.starts_with("LAYSI-"));
        // The prefix is a literal; only the drawn part comes from ALPHABET.
        for c in key["LAYSI-".len()..].chars().filter(|c| *c != '-') {
            assert!(ALPHABET.contains(&(c as u8)), "{c} 不在字母表內");
        }

        let typed = key.to_lowercase().replace('-', " ");
        assert_eq!(key_hash(&typed), key_hash(&key), "大小寫與分隔符不該影響比對");
        assert_ne!(key_hash(&key), key_hash(&new_activation_key()));
        assert_eq!(key_prefix(&key).len(), 10);
        assert!(!key_hash(&key).contains(&normalise_key(&key)), "雜湊不得含有金鑰本身");
    }
}
