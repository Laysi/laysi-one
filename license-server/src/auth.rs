//! Console sign-in.
//!
//! A session is a random token in an HttpOnly cookie and a row holding its
//! BLAKE3. Stateful rather than a signed cookie, because this console mints
//! licences: signing out and disabling an account both have to end a session
//! that is already in someone's browser.

use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::http::HeaderMap;
use sqlx::{PgPool, Row};

/// Name of the session cookie.
pub const COOKIE: &str = "laysi_one_license_session";

/// How long a console session lasts without being renewed.
const SESSION_HOURS: i64 = 12;

/// Argon2id cost, defined here and nowhere else so no entry point can end up
/// with its own. The same values the product uses.
fn engine() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None)
        .expect("Argon2id parameters are constants and must build");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

pub fn hash_password(plain: &str) -> Result<String> {
    // The salt comes straight from the OS: `SaltString::generate` wants a
    // `rand_core::OsRng` that only exists when some other crate in the build
    // happens to turn that feature on.
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).context("drawing a password salt")?;
    let salt =
        SaltString::encode_b64(&raw).map_err(|e| anyhow::anyhow!("encoding the salt: {e}"))?;
    Ok(engine()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing the password: {e}"))?
        .to_string())
}

/// Both a wrong password and a stored value that is not a hash come back
/// false, so no caller can tell them apart.
fn password_matches(plain: &str, stored: &str) -> bool {
    PasswordHash::new(stored)
        .ok()
        .is_some_and(|hash| engine().verify_password(plain.as_bytes(), &hash).is_ok())
}

/// Sign in, returning the session token to put in the cookie.
///
/// One message for both "no such account" and "wrong password": telling them
/// apart turns the form into a way to find out who has an account here.
pub async fn login(pool: &PgPool, email: &str, password: &str) -> Result<Option<String>> {
    let row = sqlx::query(
        "SELECT u.id, u.password_hash FROM partner_user u
           JOIN partner p ON p.id = u.partner_id
          WHERE lower(u.email) = lower($1) AND u.status = 'active' AND p.status = 'active'",
    )
    .bind(email.trim())
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored: String = row.get("password_hash");
    if !password_matches(password, &stored) {
        return Ok(None);
    }

    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).context("drawing a session token")?;
    let token = hex::encode(bytes);
    sqlx::query(
        "INSERT INTO partner_session (token_hash, user_id, expires_at)
         VALUES ($1, $2, now() + make_interval(hours => $3))",
    )
    .bind(crate::db::hash_token(&token))
    .bind(row.get::<i32, _>("id"))
    .bind(SESSION_HOURS as i32)
    .execute(pool)
    .await?;
    Ok(Some(token))
}

pub async fn logout(pool: &PgPool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM partner_session WHERE token_hash = $1")
        .bind(crate::db::hash_token(token))
        .execute(pool)
        .await?;
    Ok(())
}

/// The session cookie sent with this request, if any.
pub fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
}

/// Whether the cookie this response sets should be `Secure`.
///
/// Derived from the request rather than configured, for the reason the
/// product settled on: a flag someone has to remember to turn over on the day
/// the site moves to HTTPS is a flag that ships a session cookie in the clear.
pub fn secure(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

pub fn set_cookie(token: &str, secure: bool) -> String {
    let flag = if secure { "; Secure" } else { "" };
    format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{flag}",
        SESSION_HOURS * 3600
    )
}

pub fn clear_cookie(secure: bool) -> String {
    let flag = if secure { "; Secure" } else { "" };
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{flag}")
}

/// The shortest password this console accepts. Generated ones are longer;
/// this bounds what a person may replace them with.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Whether this account's current password is `plain`. The change form asks
/// for it, except where the stored password was generated and the account has
/// already presented it once to get this far.
pub async fn password_is(pool: &PgPool, id: i32, plain: &str) -> Result<bool> {
    let Some(row) = sqlx::query("SELECT password_hash FROM partner_user WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(false);
    };
    Ok(password_matches(
        plain,
        &row.get::<String, _>("password_hash"),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_verifies_against_its_own_hash_and_not_another() {
        let hash = hash_password("正確的密碼").expect("應雜湊得出來");
        assert!(password_matches("正確的密碼", &hash));
        assert!(!password_matches("錯的密碼", &hash));
        // A stored value that is not a hash at all must fail closed rather
        // than panic: an empty column would otherwise let anyone in.
        assert!(!password_matches("任何字串", ""));
    }

    #[test]
    fn the_session_cookie_is_read_out_of_a_header_with_others_in_it() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            format!("theme=dark; {COOKIE}=abc123; other=1")
                .parse()
                .unwrap(),
        );
        assert_eq!(cookie(&headers).as_deref(), Some("abc123"));

        headers.insert("x-forwarded-proto", "https, http".parse().unwrap());
        assert!(secure(&headers));
        assert!(set_cookie("abc", true).contains("; Secure"));
        assert!(!set_cookie("abc", false).contains("; Secure"));
    }
}
