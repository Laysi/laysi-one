//! `POST /v1/license/check`: the one endpoint an installation ever calls.
//!
//! Activation and the periodic check are the same request. The first one
//! binds the licence to the installation that made it; every later one
//! renews the lease and reports what that installation now looks like. There
//! is no second endpoint because there is no second decision.
//!
//! What travels here is deliberately thin — an identifier, a fingerprint and
//! a version. The fingerprint is a hash of the host, not of anything about
//! the business: nothing here says how large the customer is, let alone what
//! is in their books.

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use std::net::SocketAddr;

use crate::release;
use crate::token::{self, LicensePayload};
use crate::Shared;

#[derive(Deserialize)]
pub struct CheckRequest {
    pub activation_key: String,
    pub install_id: uuid::Uuid,
    /// BLAKE3 of what the installation is running on, as 64 hex characters.
    /// The installation decides what goes into it; this server only has to
    /// notice when it changes.
    pub fingerprint: String,
    #[serde(default)]
    pub version: String,
    /// The nonce handed out by the previous answer. Absent on the first
    /// check, and on any installation restored from a backup taken before the
    /// last one.
    #[serde(default)]
    pub nonce: Option<String>,
}

#[derive(Serialize)]
pub struct CheckResponse {
    pub token: String,
    pub nonce: String,
    /// The signed release manifest Laysi has offered this licence, as the
    /// text that was uploaded. Left out entirely when nothing is offered, so
    /// that answer is the one installations got before the field existed.
    /// Signed by the release key rather than this server's, so it travels
    /// beside the token rather than inside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_manifest: Option<String>,
}

/// A refusal, in the words the installation will print.
fn refuse(code: &str, message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

/// Source address as far as this server can tell.
///
/// Behind the reverse proxy that terminates TLS, so the first hop of
/// `X-Forwarded-For` is the client's own address and the socket address is
/// the proxy. Recorded rather than trusted: it appears on the denial list a
/// person reads, and decides nothing.
fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| peer.ip().to_string())
}

pub async fn check(
    State(s): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CheckRequest>,
) -> Response {
    let ip = client_ip(&headers, peer);
    match run(&s, &req, &ip).await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "licence check failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": { "code": "server_error", "message": "授權伺服器暫時無法回應，請稍後再試。" } })),
            )
                .into_response()
        }
    }
}

pub(crate) async fn run(s: &Shared, req: &CheckRequest, ip: &str) -> anyhow::Result<Response> {
    // Shape first. A fingerprint that is not 64 hex characters is a client
    // that has not been written yet, not a licence problem.
    if req.fingerprint.len() != 64 || !req.fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(refuse("bad_request", "識別碼格式不正確。"));
    }

    let mut tx = s.pool.begin().await?;
    // Locked for the length of the decision: two installations presenting the
    // same key at the same moment must not both come out bound.
    let row = sqlx::query(
        "SELECT l.id, l.license_no, l.licensee, l.licensee_tax_id, l.expires_on, l.status,
                l.fingerprint, l.install_id, l.nonce, p.name AS partner_name, p.partner_no,
                p.status AS partner_status, m.manifest AS release_manifest
           FROM license l JOIN partner p ON p.id = l.partner_id
           LEFT JOIN release_manifest m ON m.version = l.release_version
          WHERE l.key_hash = $1
          FOR UPDATE OF l",
    )
    .bind(token::key_hash(&req.activation_key))
    .fetch_optional(&mut *tx)
    .await?;

    // No row means no licence to attach a denial to, so nothing is recorded:
    // an unknown key is somebody's typo far more often than it is an attack,
    // and a table anyone can write to by guessing is not evidence.
    let Some(row) = row else {
        return Ok(refuse(
            "unknown_key",
            "查無此啟動金鑰，請確認金鑰是否輸入正確，或洽出售的經銷商。",
        ));
    };

    let id: i32 = row.get("id");
    let license_no: String = row.get("license_no");
    let status: String = row.get("status");
    let partner_status: String = row.get("partner_status");
    let partner_name: String = row.get("partner_name");
    let bound: Option<String> = row.get("fingerprint");
    let bound_install: Option<uuid::Uuid> = row.get("install_id");
    let stored_nonce: Option<String> = row.get("nonce");

    // Refusal code, recorded reason and the sentence the installation prints.
    // The code is what the product branches on; the sentence is what a person
    // reads, and it names who to call rather than what this server concluded.
    let refusal: Option<(&str, String)> = if partner_status != "active" {
        Some((
            "partner_suspended",
            format!("授權目前無法使用，請洽{partner_name}。"),
        ))
    } else if status == "revoked" {
        Some(("revoked", format!("本組授權已註銷，請洽{partner_name}。")))
    } else if status == "suspended" {
        Some(("suspended", format!("本組授權已暫停，請洽{partner_name}。")))
    } else if bound.as_deref().is_some_and(|f| f != req.fingerprint) {
        // Rebinding on sight would make binding meaningless, so the second
        // machine is refused and the partner is shown the attempt. Recoverable
        // in one click on their side, and the first machine keeps working
        // throughout, which is what a genuine server move needs.
        Some((
            "bound_elsewhere",
            format!("本組授權已在另一台主機啟用，請洽{partner_name}解除綁定後再試。"),
        ))
    } else {
        None
    };

    if let Some((code, message)) = refusal {
        sqlx::query(
            "INSERT INTO license_denial (license_id, fingerprint, ip, reason)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(&req.fingerprint)
        .bind(ip)
        .bind(code)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(refuse(code, &message));
    }

    // A database that has never been answered has no nonce to carry, and a
    // fresh one draws its own `install_id`. Both together on the host this
    // licence is already bound to is a reinstall, which is the most ordinary
    // thing an administrator does: the fingerprint says it is the same
    // machine, so nothing here is a second installation.
    let reinstalled = is_reinstall(req.nonce.as_deref(), bound_install, req.install_id);

    // Two copies of one database each carry the other's nonce, so each check
    // invalidates the other. Counted and shown, never blocked: a restore from
    // yesterday's backup is indistinguishable from a clone, and locking a
    // customer out of their own books on a heuristic is the worse mistake.
    let continuity_broken = bound.is_some()
        && stored_nonce.is_some()
        && !reinstalled
        && req.nonce.as_deref() != stored_nonce.as_deref();
    if continuity_broken {
        sqlx::query(
            "UPDATE license SET conflicts = conflicts + 1, last_conflict_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO license_denial (license_id, fingerprint, ip, reason)
             VALUES ($1, $2, $3, 'duplicate_install')",
        )
        .bind(id)
        .bind(&req.fingerprint)
        .bind(ip)
        .execute(&mut *tx)
        .await?;
    }

    let nonce = token::new_nonce();
    let first_time = bound.is_none();
    sqlx::query(
        "UPDATE license
            SET fingerprint = $2,
                install_id = $3,
                bound_at = COALESCE(bound_at, now()),
                status = CASE WHEN status = 'issued' THEN 'active' ELSE status END,
                nonce = $4,
                last_seen_at = now(),
                last_ip = $5,
                version = $6
          WHERE id = $1",
    )
    .bind(id)
    .bind(&req.fingerprint)
    .bind(req.install_id)
    .bind(&nonce)
    .bind(ip)
    .bind(req.version.chars().take(40).collect::<String>())
    .execute(&mut *tx)
    .await?;

    let expires_on: NaiveDate = row.get("expires_on");
    let payload = LicensePayload {
        license_no: license_no.clone(),
        fingerprint: req.fingerprint.clone(),
        install_id: req.install_id.to_string(),
        licensee: row.get("licensee"),
        licensee_tax_id: row.get("licensee_tax_id"),
        partner: partner_name,
        partner_no: row.get("partner_no"),
        issued_at: Utc::now(),
        not_after: lease_until(s.today(), expires_on),
        expires_on,
    };
    let signed = token::mint(&s.signing_key, &payload)?;
    // Only on an answer that renews the lease: a refused installation has
    // nothing it may run, let alone a new release of it.
    let release_manifest =
        release::forward(&license_no, row.get("release_manifest"), &s.release_keys);
    tx.commit().await?;

    tracing::info!(
        license = %license_no,
        first_time,
        continuity_broken,
        release = release_manifest.is_some(),
        "licence check answered"
    );
    Ok(Json(CheckResponse {
        token: signed,
        nonce,
        release_manifest,
    })
    .into_response())
}

/// Whether this check is a fresh database on the host the licence is already
/// bound to.
///
/// Reached only once the fingerprint has matched, so the machine is not in
/// question: what is left is whether the books are the same books. A database
/// that has never been answered carries no nonce and draws its own
/// `install_id`, and neither of those travels with a copy.
fn is_reinstall(
    nonce: Option<&str>,
    bound_install: Option<uuid::Uuid>,
    install_id: uuid::Uuid,
) -> bool {
    nonce.unwrap_or_default().is_empty() && bound_install != Some(install_id)
}

/// How long this answer is good for.
///
/// The lease never reaches past the licence year the partner sold, so a
/// contract that ended yesterday hands back a token that has already lapsed
/// rather than a refusal. That is deliberate: the installation can then say
/// *when* it expired and *who* to call, which a 403 cannot carry.
fn lease_until(today: NaiveDate, expires_on: NaiveDate) -> NaiveDate {
    let lease = today + chrono::Duration::days(token::LEASE_DAYS);
    lease.min(expires_on)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn a_lease_never_outlives_the_licence_year() {
        let today = d(2026, 9, 2);
        // Well inside the term: the lease is the full window, so a site off
        // the network for a fortnight never sees a warning.
        assert_eq!(lease_until(today, d(2027, 9, 1)), d(2026, 10, 17));
        // Close to the end: the lease stops at the end of the term, so the
        // countdown on the installation starts on its own.
        assert_eq!(lease_until(today, d(2026, 9, 20)), d(2026, 9, 20));
        // Already over: the token comes back stale rather than refused, which
        // is what lets the screen name the date and the partner.
        assert_eq!(lease_until(today, d(2026, 8, 1)), d(2026, 8, 1));
    }

    #[test]
    fn a_rebuilt_installation_on_the_bound_host_is_not_a_duplicate() {
        let old = uuid::Uuid::from_u128(1);
        let new = uuid::Uuid::from_u128(2);
        // The nightly demo rebuild, and every reinstall: new database, with no
        // answer yet to carry back.
        assert!(is_reinstall(Some(""), Some(old), new));
        assert!(is_reinstall(None, Some(old), new));
        // A copy of the books carries both, so it stays visible.
        assert!(!is_reinstall(Some("n1"), Some(old), old));
        assert!(!is_reinstall(Some("n1"), Some(old), new));
        // The same database with its nonce lost is a restore, not a reinstall.
        assert!(!is_reinstall(Some(""), Some(old), old));
    }

    fn keys(answer: &CheckResponse) -> Vec<String> {
        let value = serde_json::to_value(answer).unwrap();
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    #[test]
    fn an_answer_with_nothing_released_is_the_answer_installations_always_got() {
        let answer = CheckResponse {
            token: "t".into(),
            nonce: "n".into(),
            release_manifest: None,
        };
        assert_eq!(keys(&answer), ["nonce", "token"]);
        assert_eq!(
            serde_json::to_string(&answer).unwrap(),
            r#"{"token":"t","nonce":"n"}"#
        );
    }

    #[test]
    fn a_released_manifest_travels_as_the_text_that_was_uploaded() {
        let text = crate::release::tests::manifest("v0.1.13", &crate::release::tests::key(7));
        let answer = CheckResponse {
            token: "t".into(),
            nonce: "n".into(),
            release_manifest: Some(text.clone()),
        };
        assert_eq!(keys(&answer), ["nonce", "release_manifest", "token"]);
        // Escaped in JSON, and the same bytes once the installation reads the
        // string back out: newlines and all, nothing normalised.
        let wire = serde_json::to_string(&answer).unwrap();
        let read: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(read["release_manifest"].as_str(), Some(text.as_str()));
    }

    #[test]
    fn the_forwarded_address_is_preferred_and_the_socket_is_the_fallback() {
        let peer: SocketAddr = "10.0.0.9:5555".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers, peer), "10.0.0.9");
        headers.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&headers, peer), "203.0.113.7");
    }
}
