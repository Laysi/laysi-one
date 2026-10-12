//! Releases end to end against PostgreSQL: the console's upload and release
//! forms, then the check answer each licence gets.
//!
//! Runs only when `LICENSE_TEST_DATABASE_URL` names a disposable database, so
//! `cargo test` still needs none. Not `TEST_DATABASE_URL`: that one points at
//! the product's test database, and this applies the licence server's schema
//! to whatever it is given. Every row it writes carries a fresh tag and is
//! deleted at the end.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRequest, Multipart, Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;
use axum::Form;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use sqlx::{PgPool, Row};

use super::tests::{key, manifest};
use crate::check::{self, CheckRequest};
use crate::{auth, console, db, ratelimit, token, Shared};

struct Fixture {
    s: Shared,
    tag: String,
    vendor: HeaderMap,
    reseller: HeaderMap,
}

impl Fixture {
    fn pool(&self) -> &PgPool {
        &self.s.pool
    }

    fn key(&self, licence: &str) -> String {
        format!("TEST-{}-{licence}", self.tag)
    }

    fn no(&self, licence: &str) -> String {
        format!("T{}-{licence}", self.tag)
    }

    /// A check from the installation that holds licence `licence`.
    async fn check(
        &self,
        licence: &str,
        version: &str,
        nonce: Option<String>,
    ) -> (StatusCode, Value) {
        let request = CheckRequest {
            activation_key: self.key(licence),
            install_id: uuid::Uuid::from_u128(licence.as_bytes()[0] as u128),
            fingerprint: format!("{:064x}", licence.as_bytes()[0]),
            version: version.into(),
            nonce,
        };
        let response = check::run(&self.s, &request, "127.0.0.1").await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn upload(&self, who: &HeaderMap, bytes: &[u8]) -> String {
        let mut body = b"--BOUNDARY\r\nContent-Disposition: form-data; name=\"manifest\"; \
filename=\"release-manifest.txt\"\r\nContent-Type: text/plain\r\n\r\n"
            .to_vec();
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n--BOUNDARY--\r\n");
        let request = Request::builder()
            .method("POST")
            .uri("/releases")
            .header("content-type", "multipart/form-data; boundary=BOUNDARY")
            .body(Body::from(body))
            .unwrap();
        let form = Multipart::from_request(request, &()).await.unwrap();
        location(console::upload_manifest(State(self.s.clone()), who.clone(), form).await)
    }

    async fn release(&self, who: &HeaderMap, licence: &str, version: &str) -> String {
        let form = serde_json::from_value(serde_json::json!({ "version": version })).unwrap();
        location(
            console::set_release(
                State(self.s.clone()),
                who.clone(),
                Path(self.no(licence)),
                Form(form),
            )
            .await,
        )
    }

    async fn released(&self, licence: &str) -> Option<String> {
        sqlx::query("SELECT release_version FROM license WHERE license_no = $1")
            .bind(self.no(licence))
            .fetch_one(self.pool())
            .await
            .unwrap()
            .get("release_version")
    }

    async fn stored(&self, version: &str) -> Option<String> {
        db::manifest(self.pool(), version).await.unwrap()
    }
}

/// Where a console handler sent the browser, query string decoded.
fn location(response: Response) -> String {
    assert!(
        response.status().is_redirection(),
        "預期導回某一頁，實際 {}",
        response.status()
    );
    let raw = response.headers()["location"].to_str().unwrap();
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            out.push(u8::from_str_radix(&raw[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn keys(answer: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = answer
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    keys
}

async fn signed_in(pool: &PgPool, email: &str) -> HeaderMap {
    let session = auth::login(pool, email, "release-test-password")
        .await
        .unwrap()
        .expect("測試帳號應登入得了");
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        format!("{}={session}", auth::COOKIE).parse().unwrap(),
    );
    headers
}

async fn partner(pool: &PgPool, no: &str, name: &str) -> i32 {
    sqlx::query("INSERT INTO partner (partner_no, name) VALUES ($1, $2) RETURNING id")
        .bind(no)
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
        .get("id")
}

async fn setup(url: &str) -> Fixture {
    let pool = db::connect(url).await.unwrap();
    let tag = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let hash = auth::hash_password("release-test-password").unwrap();

    // The vendor's partner and a reseller, each with an admin. The reseller's
    // admin manages every licence below and still may not release.
    let vendor_partner = partner(&pool, &format!("TV-{tag}"), "原廠（測試）").await;
    let reseller_partner = partner(&pool, &format!("TR-{tag}"), "經銷商（測試）").await;
    for (partner_id, who, is_vendor) in [
        (vendor_partner, "vendor", true),
        (reseller_partner, "reseller", false),
    ] {
        sqlx::query(
            "INSERT INTO partner_user (partner_id, email, password_hash, is_vendor, is_admin)
             VALUES ($1, $2, $3, $4, true)",
        )
        .bind(partner_id)
        .bind(format!("{who}-{tag}@release.test"))
        .bind(&hash)
        .bind(is_vendor)
        .execute(&pool)
        .await
        .unwrap();
    }

    let s: Shared = Arc::new(crate::State {
        pool: pool.clone(),
        signing_key: SigningKey::from_bytes(&[9; 32]),
        mailer: None,
        login_rate_limiter: ratelimit::RateLimiter::new(auth::LOGIN_ATTEMPTS_PER_MINUTE),
        // Two release keys, so one version can be signed twice and both
        // verify; key 9 stands for every key this build does not trust.
        release_keys: vec![key(7).verifying_key(), key(8).verifying_key()],
    });
    let fixture = Fixture {
        vendor: signed_in(&pool, &format!("vendor-{tag}@release.test")).await,
        reseller: signed_in(&pool, &format!("reseller-{tag}@release.test")).await,
        s,
        tag,
    };
    for (licence, status) in [("A", "issued"), ("B", "issued"), ("C", "revoked")] {
        sqlx::query(
            "INSERT INTO license (license_no, partner_id, licensee, key_hash, key_prefix,
                                  expires_on, status)
             VALUES ($1, $2, '測試客戶', $3, 'TEST', current_date + 365, $4)",
        )
        .bind(fixture.no(licence))
        .bind(reseller_partner)
        .bind(token::key_hash(&fixture.key(licence)))
        .bind(status)
        .execute(&pool)
        .await
        .unwrap();
    }
    fixture
}

async fn teardown(f: &Fixture, version: &str) {
    let pool = f.pool();
    sqlx::query("DELETE FROM license WHERE license_no LIKE $1")
        .bind(format!("T{}-%", f.tag))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM release_manifest WHERE version = $1")
        .bind(version)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM partner_user WHERE email LIKE $1")
        .bind(format!("%-{}@release.test", f.tag))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM partner WHERE partner_no IN ($1, $2)")
        .bind(format!("TV-{}", f.tag))
        .bind(format!("TR-{}", f.tag))
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_release_reaches_only_the_licence_it_is_set_on() {
    let Ok(url) = std::env::var("LICENSE_TEST_DATABASE_URL") else {
        eprintln!("略過：未設定 LICENSE_TEST_DATABASE_URL");
        return;
    };
    let f = setup(&url).await;
    // A pre-release of its own, so no stored version is ever reused.
    let version = format!("v0.0.2-t{}", f.tag);
    let text = manifest(&version, &key(7));

    // Before anything is released: the answer has the two fields it always had.
    let (status, a) = f.check("A", "v0.0.1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keys(&a), ["nonce", "token"]);
    let (_, b) = f.check("B", "v0.0.1", None).await;
    assert_eq!(keys(&b), ["nonce", "token"]);

    // Uploading is Laysi's: a reseller's admin is sent away and nothing lands.
    assert_eq!(f.upload(&f.reseller, text.as_bytes()).await, "/licenses");
    assert_eq!(f.stored(&version).await, None);

    // Refused uploads say why on the version page and store nothing.
    let untrusted = manifest(&version, &key(9));
    let refused = f.upload(&f.vendor, untrusted.as_bytes()).await;
    assert!(
        refused.starts_with("/releases?note=") && refused.contains("信任的簽章"),
        "{refused}"
    );
    let crlf = text.replace('\n', "\r\n");
    assert!(f.upload(&f.vendor, crlf.as_bytes()).await.contains("CRLF"));
    let oversize = vec![b'x'; laysi_license_token::release::MAX_MANIFEST_BYTES + 1];
    assert!(f.upload(&f.vendor, &oversize).await.contains("位元組"));
    assert_eq!(f.stored(&version).await, None);

    // Laysi staff upload it; the same bytes again change nothing.
    assert!(f
        .upload(&f.vendor, text.as_bytes())
        .await
        .starts_with("/releases?done="));
    assert_eq!(f.stored(&version).await.as_deref(), Some(text.as_str()));
    assert!(f
        .upload(&f.vendor, text.as_bytes())
        .await
        .starts_with("/releases?done="));

    // Other bytes for a version already stored are refused, even signed by a
    // trusted key, and the first upload stays.
    let rival = manifest(&version, &key(8));
    let refused = f.upload(&f.vendor, rival.as_bytes()).await;
    assert!(refused.contains("內容不同"), "{refused}");
    assert_eq!(f.stored(&version).await.as_deref(), Some(text.as_str()));

    // Releasing is Laysi's as well.
    assert_eq!(f.release(&f.reseller, "A", &version).await, "/licenses");
    assert_eq!(f.released("A").await, None);

    // Released to A. A's next answer carries the manifest byte for byte, and
    // B's is exactly what it was.
    let back = f.release(&f.vendor, "A", &version).await;
    assert_eq!(back, format!("/licenses/{}", f.no("A")));
    assert_eq!(f.released("A").await.as_deref(), Some(version.as_str()));
    let (_, a) = f
        .check("A", "v0.0.1", a["nonce"].as_str().map(String::from))
        .await;
    assert_eq!(keys(&a), ["nonce", "release_manifest", "token"]);
    assert_eq!(a["release_manifest"].as_str(), Some(text.as_str()));
    let (_, b) = f
        .check("B", "v0.0.1", b["nonce"].as_str().map(String::from))
        .await;
    assert_eq!(keys(&b), ["nonce", "token"]);
    assert_eq!(f.released("B").await, None);

    // B's machine already runs something later: releasing the older one is
    // refused with the reason, and B stays as it was.
    let (_, b) = f
        .check("B", "v0.0.3", b["nonce"].as_str().map(String::from))
        .await;
    let refused = f.release(&f.vendor, "B", &version).await;
    assert!(refused.contains("v0.0.3"), "{refused}");
    assert_eq!(f.released("B").await, None);
    assert_eq!(keys(&b), ["nonce", "token"]);

    // A refused check carries no release, whatever its row says.
    db::set_release(f.pool(), &f.no("C"), Some(&version))
        .await
        .unwrap();
    let (status, c) = f.check("C", "v0.0.1", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(c.get("release_manifest").is_none(), "{c}");

    // Cleared: A is back to the answer it had before.
    assert_eq!(
        f.release(&f.vendor, "A", "").await,
        format!("/licenses/{}", f.no("A"))
    );
    let (_, a) = f
        .check("A", "v0.0.1", a["nonce"].as_str().map(String::from))
        .await;
    assert_eq!(keys(&a), ["nonce", "token"]);

    teardown(&f, &version).await;
}
