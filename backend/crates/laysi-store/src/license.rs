//! The licence row and the fingerprint that binds it to this machine.
//!
//! `license_state` is one row. It holds the activation key somebody typed,
//! the signed token the licence server handed back for it, and the nonce that
//! has to travel with the next check. Nothing here decides whether the
//! licence is valid — that is `laysi_ops::license`, which verifies the
//! signature — because a decision made from columns is a decision an editor
//! with database access can change.
//!
//! The fingerprint is what stops one activation key running two machines. It
//! is the host's machine id, which survives a container being recreated and
//! does not travel with a database copied to another host.
//!
//! It identifies the **machine**, not the database, and that distinction is
//! the whole of it. Folding `install_id` in as well would make reinstalling
//! on the same server look like a second machine, so every rebuild would need
//! the reseller to unbind the licence first — a support call for the most
//! ordinary thing an administrator does.
//!
//! `install_id` is the fallback for a host that exposes no machine id at all.
//! Without one every such installation would hash to the same value, the
//! first would bind and the rest would be told they were somebody else's.
//! The bundled compose mounts `/etc/machine-id`, so that path is for
//! deployments that do not.
//!
//! Either input is stable or absent, never intermittent: a fingerprint that
//! moved between boots would lock a paying customer out of their own books,
//! and what it guards against is a partner selling one key twice.

use crate::{Result, Store};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row;
use uuid::Uuid;

/// Where the host identity is read from, in order. The first readable one
/// wins; none of them being there is also an answer, and a stable one.
const MACHINE_ID_PATHS: [&str; 2] = ["/etc/machine-id", "/var/lib/dbus/machine-id"];

#[derive(Debug, Clone)]
pub struct LicenseState {
    pub install_id: Uuid,
    pub activation_key: String,
    /// The signed token, verbatim. Empty until the first successful check.
    pub token: String,
    pub nonce: String,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_error: String,
    /// Set only on installations that predate licensing.
    pub grace_until: Option<NaiveDate>,
}

/// This machine, as 64 hex characters.
///
/// Not secret and not meant to be: the licence server only has to notice when
/// it changes, and the customer reads it off their own screen when a partner
/// has to unbind the machine.
pub fn fingerprint(install_id: Uuid) -> String {
    let machine = MACHINE_ID_PATHS
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    let mut hasher = blake3::Hasher::new();
    match machine {
        Some(id) => {
            hasher.update(b"machine\0");
            hasher.update(id.as_bytes());
        }
        None => {
            hasher.update(b"install\0");
            hasher.update(install_id.as_bytes());
        }
    }
    hasher.finalize().to_hex().to_string()
}

impl Store {
    pub async fn license_state(&self) -> Result<LicenseState> {
        let row = sqlx::query(
            "SELECT install_id, activation_key, token, nonce, last_check_at, last_error,
                    grace_until
               FROM license_state WHERE id = 1",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(LicenseState {
            install_id: row.get("install_id"),
            activation_key: row.get("activation_key"),
            token: row.get("token"),
            nonce: row.get("nonce"),
            last_check_at: row.get("last_check_at"),
            last_error: row.get("last_error"),
            grace_until: row.get("grace_until"),
        })
    }

    /// Record a successful check.
    ///
    /// The grace window is cleared here rather than left to expire: once a
    /// real licence has arrived, an installation that predates licensing is
    /// no longer one of the cases that needed it, and leaving the date behind
    /// would make a later lapse look like it still had time.
    pub async fn save_license(&self, key: &str, token: &str, nonce: &str) -> Result<()> {
        sqlx::query(
            "UPDATE license_state
                SET activation_key = $1, token = $2, nonce = $3,
                    last_check_at = now(), last_error = '', grace_until = NULL
              WHERE id = 1",
        )
        .bind(key)
        .bind(token)
        .bind(nonce)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a check that did not succeed.
    ///
    /// The token is left alone. A refused or unreachable check does not
    /// revoke anything on its own: the lease inside the token is what decides,
    /// and it runs out by itself.
    pub async fn record_license_error(&self, message: &str) -> Result<()> {
        sqlx::query("UPDATE license_state SET last_error = $1, last_check_at = now() WHERE id = 1")
            .bind(message.chars().take(300).collect::<String>())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_is_stable_and_shaped_like_an_identifier() {
        let a = Uuid::parse_str("8b1f6f3e-0000-4000-8000-000000000001").unwrap();

        // Same input, same answer: a fingerprint that moved between boots
        // would lock a paying customer out of their own books.
        assert_eq!(fingerprint(a), fingerprint(a));
        assert_eq!(fingerprint(a).len(), 64);
        assert!(fingerprint(a).chars().all(|c| c.is_ascii_hexdigit()));
        // The identifier itself must not be readable out of the hash.
        assert!(!fingerprint(a).contains(&a.to_string().replace('-', "")));
    }

    /// Reinstalling on the same server must not look like a second machine:
    /// the licence is sold per company, and a rebuild is the most ordinary
    /// thing an administrator does. Only the host with no machine id at all
    /// falls back to the database's own identifier.
    #[test]
    fn a_new_database_on_the_same_host_keeps_the_same_fingerprint() {
        let a = Uuid::parse_str("8b1f6f3e-0000-4000-8000-000000000001").unwrap();
        let b = Uuid::parse_str("8b1f6f3e-0000-4000-8000-000000000002").unwrap();
        let host_has_machine_id = MACHINE_ID_PATHS
            .iter()
            .any(|path| std::fs::read_to_string(path).is_ok_and(|t| !t.trim().is_empty()));

        if host_has_machine_id {
            assert_eq!(fingerprint(a), fingerprint(b), "同一台主機重裝不該換識別碼");
        } else {
            assert_ne!(
                fingerprint(a),
                fingerprint(b),
                "沒有主機識別碼時才退回安裝識別碼"
            );
        }
    }
}
