//! Password hashing and verification.
//!
//! The Argon2id parameters are defined here and nowhere else. Login, password
//! change and the admin tool all call this module, so no entry point can end
//! up with its own cost or format.

use argon2::password_hash::{
    rand_core::OsRng as PasswordOsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::rngs::OsRng as TokenOsRng;
use rand::RngCore;

/// Argon2id memory cost, in KiB.
const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
/// Argon2id iterations.
const ARGON2_ITERATIONS: u32 = 2;
/// Argon2id parallelism.
const ARGON2_PARALLELISM: u32 = 1;

fn engine() -> Argon2<'static> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .expect("Argon2id 參數是固定常數，必須在編譯與測試階段就能建立");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Produce a PHC string ready to store in `user_account.password_hash`.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut PasswordOsRng);
    engine()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
}

/// Verify a password. Both an invalid format and a wrong password return
/// `false`, so the login endpoint cannot tell them apart.
pub fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded)
        .ok()
        .is_some_and(|hash| engine().verify_password(password.as_bytes(), &hash).is_ok())
}

/// Alphabet for temporary passwords, without the characters that are read or
/// heard wrong: `0`/`o`, `1`/`l`/`i`. Lower case only, for the same reason.
const TEMPORARY_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

/// Number of characters in a temporary password. Thirty-one symbols to the
/// eighth is about 39 bits, which is more than a password that must be changed
/// on first use needs, and short enough to read out over the phone.
const TEMPORARY_LENGTH: usize = 8;

/// Generate the temporary password shown once when an account is created or
/// its password reset.
///
/// It is dictated to a person, usually by telephone, so length is a cost paid
/// by the user rather than by an attacker: the account cannot be used without
/// changing it, and every login goes through the same Argon2id cost above.
pub fn temporary_password() -> String {
    // Rejection sampling: taking a byte modulo the alphabet length would make
    // the first few symbols more likely than the rest.
    let limit = (u8::MAX as usize / TEMPORARY_ALPHABET.len() * TEMPORARY_ALPHABET.len()) as u8;
    let mut out = String::with_capacity(TEMPORARY_LENGTH);
    let mut buffer = [0_u8; 32];
    while out.len() < TEMPORARY_LENGTH {
        TokenOsRng.fill_bytes(&mut buffer);
        for byte in buffer {
            if byte < limit {
                out.push(TEMPORARY_ALPHABET[byte as usize % TEMPORARY_ALPHABET.len()] as char);
                if out.len() == TEMPORARY_LENGTH {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2id_hash_verifies_without_storing_plaintext() {
        let password = "一組不會直接落進資料庫的測試密碼";
        let encoded = hash_password(password).expect("應可產生密碼雜湊");

        assert!(encoded.starts_with("$argon2id$"));
        assert_ne!(encoded, password);
        assert!(!encoded.contains(password));
        assert!(verify_password(password, &encoded));
        assert!(!verify_password("錯誤密碼", &encoded));
    }

    /// It is read out loud to the person it belongs to, so every character
    /// has to survive being spoken and written down.
    #[test]
    fn a_temporary_password_can_be_dictated_and_still_hashes() {
        let first = temporary_password();
        let second = temporary_password();
        assert_eq!(first.chars().count(), TEMPORARY_LENGTH);
        assert_ne!(first, second);
        assert!(first.bytes().all(|b| TEMPORARY_ALPHABET.contains(&b)));
        for confusable in ['0', 'o', '1', 'l', 'i'] {
            assert!(!first.contains(confusable), "{first} 含有會聽錯的字元");
        }

        let encoded = hash_password(&first).expect("暫時密碼應可雜湊");
        assert!(verify_password(&first, &encoded));
        assert!(!encoded.contains(&first));
    }
}
