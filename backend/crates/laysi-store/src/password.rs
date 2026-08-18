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

/// Generate the high-entropy temporary password the admin tool shows once.
pub fn temporary_password() -> String {
    let mut bytes = [0_u8; 18];
    TokenOsRng.fill_bytes(&mut bytes);
    format!("tmp-{}", hex::encode(bytes))
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

    #[test]
    fn temporary_password_is_high_entropy_and_hashable() {
        let first = temporary_password();
        let second = temporary_password();
        assert!(first.starts_with("tmp-"));
        assert_eq!(first.len(), 40);
        assert_ne!(first, second);

        let encoded = hash_password(&first).expect("暫時密碼應可雜湊");
        assert!(verify_password(&first, &encoded));
        assert!(!encoded.contains(&first));
    }
}
