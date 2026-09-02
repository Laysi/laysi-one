//! First-run initialisation from the browser.
//!
//! Creating the first account is the one request in the system that no
//! permission check can guard, because there is no identity to check yet.
//! `laysi-admin create-user` solves that by requiring host access; this
//! endpoint solves it by being open only while the installation has no
//! accounts at all, and closing permanently the moment it has one.
//!
//! The gate is not a Rust `if`. `Store::bootstrap_first_user` writes the claim
//! and the account in one transaction against a single-row table, so two
//! concurrent requests cannot both succeed and emptying `user_account`
//! afterwards does not reopen anything. The check here only produces a better
//! error message than a constraint violation would.
//!
//! `SETUP_TOKEN` closes the remaining window: between `docker compose up` and
//! the owner reaching the browser, whoever arrives first becomes the owner.
//! On a private network that window is uninteresting; on a public address it
//! is not, and the operator is already editing `.env` before the first boot.
//!
//! The role is fixed to `admin` and cannot be chosen by the caller. A first
//! account without the permission to grant permissions would leave the
//! installation unusable, and an unauthenticated endpoint should not offer a
//! degree of freedom that has no correct value other than one.
//!
//! The login name and display name are fixed for the same reason. This account
//! stands for the installation, not for whoever ran the installer: it has no
//! employee record, no address, and nothing to do once the owner account
//! exists. Asking for an address here would only produce one that stops being
//! true when that person leaves, and would put the one login that has to work
//! when the IdP does not back onto an identity the IdP controls.

use crate::auth::{
    attach_session_cookie, env_bool, error_response, issue_session, user_json_with_role,
    MIN_PASSWORD_CHARS,
};
use crate::{today, Shared};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use laysi_server::password;
use laysi_store::CreateUser;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// The role the first account always gets. The system administrator, not the
/// owner: whoever installs the product must not thereby read everyone's
/// salary. They create the owner account afterwards with
/// `create_user_account`.
const BOOTSTRAP_ROLE: &str = laysi_ops::ROLE_ADMIN;
/// Shown wherever this account acts: the audit trail, the account bar. It
/// names the role because that is all this account is.
const BOOTSTRAP_DISPLAY_NAME: &str = "系統管理員";
/// Recorded in `user_account.created_by`, so an account made this way is
/// distinguishable from `laysi-admin`'s afterwards.
const CREATED_BY: &str = "web-setup";
/// Attempts per minute for the whole endpoint, counted in process. Low on
/// purpose: it is used once in the life of an installation, and it is the one
/// place where a wrong `SETUP_TOKEN` can be guessed.
pub(crate) const ATTEMPTS_PER_MINUTE: u32 = 5;
/// This endpoint is called once in the life of an installation and has no
/// caller identity to bucket by, so every caller shares one bucket.
const RATE_BUCKET: &str = "bootstrap";

/// Whether and how the browser may initialise this installation.
#[derive(Clone)]
pub struct SetupConfig {
    allow_web: bool,
    /// BLAKE3 of `SETUP_TOKEN`, not the token. Comparing hashes removes the
    /// timing question from the comparison, and the plaintext then exists only
    /// in the environment.
    token_hash: Option<Arc<str>>,
}

impl SetupConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let allow_web = env_bool("SETUP_ALLOW_WEB", true)?;
        let token_hash = laysi_store::env_or_file("SETUP_TOKEN")
            .map(|value| Arc::from(laysi_store::token_hash(&value)));
        Ok(Self {
            allow_web,
            token_hash,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            allow_web: true,
            token_hash: None,
        }
    }

    pub fn allow_web(&self) -> bool {
        self.allow_web
    }

    pub fn token_required(&self) -> bool {
        self.token_hash.is_some()
    }

    fn token_accepted(&self, presented: &str) -> bool {
        match &self.token_hash {
            None => true,
            Some(expected) => laysi_store::token_hash(presented.trim()) == **expected,
        }
    }
}

#[derive(Deserialize)]
pub struct BootstrapInput {
    password: String,
    #[serde(default)]
    setup_token: String,
    /// The activation key the reseller supplied. Checked, and bound to this
    /// machine, before the account exists: an installation that turns out not
    /// to be licensed should leave nothing behind to clean up, and the person
    /// holding the key is the person standing at this screen.
    #[serde(default)]
    activation_key: String,
    /// A signed licence file, for an installation with no way out to the
    /// internet. One of the two is required.
    #[serde(default)]
    license_token: String,
}

/// A validated account request. Building one is the only way to reach the
/// store, so no field can skip its check.
struct FirstAccount {
    password: String,
    activation_key: String,
    license_token: String,
}

fn validate(input: BootstrapInput) -> Result<FirstAccount, &'static str> {
    // The same minimum as changing a password later. The account is created
    // with the password its owner chose, so unlike `laysi-admin create-user`
    // there is no temporary password to force a change from.
    if input.password.chars().count() < MIN_PASSWORD_CHARS {
        return Err("密碼至少需要 8 個字元");
    }

    let activation_key = input.activation_key.trim().to_string();
    let license_token = input.license_token.trim().to_string();
    if activation_key.is_empty() && license_token.is_empty() {
        return Err("請輸入經銷商提供的啟動金鑰");
    }

    Ok(FirstAccount {
        password: input.password,
        activation_key,
        license_token,
    })
}

/// Activate before the account is created.
///
/// Deliberately the last thing that can fail: a refused key leaves an
/// installation with no accounts, which is the state it was already in, and
/// the person can try again with the right key.
/// Returns the response to send instead of continuing, or `None` when the
/// licence is good. Not a `Result`: an `axum::Response` is a large value to
/// carry in an error variant, and there is nothing here to propagate.
async fn activation_refusal(state: &Shared, account: &FirstAccount) -> Option<Response> {
    let result = if account.license_token.is_empty() {
        laysi_ops::ops::license::check_now(&state.store, &account.activation_key).await
    } else {
        laysi_ops::ops::license::activate_offline(&state.store, &account.license_token, today())
            .await
    };
    match result {
        Ok(licence) => {
            tracing::info!(standing = licence.label(), "licence activated at setup");
            None
        }
        Err(error) => Some(error_response(
            StatusCode::BAD_REQUEST,
            "license_rejected",
            &error.to_string(),
        )),
    }
}

fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .chars()
        .take(512)
        .collect()
}

pub async fn bootstrap(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(mut input): Json<BootstrapInput>,
) -> Response {
    if !state.setup.allow_web {
        return error_response(
            StatusCode::FORBIDDEN,
            "web_setup_disabled",
            "本站已停用網頁初始化，第一個帳號請由管理員在伺服器上建立",
        );
    }

    match state.store.bootstrap_open().await {
        Ok(true) => {}
        Ok(false) => return already_done(),
        Err(error) => {
            tracing::error!(%error, "cannot confirm the install state before bootstrap");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "setup_store_error",
                "目前無法初始化，請稍後再試",
            );
        }
    }

    let presented_token = std::mem::take(&mut input.setup_token);

    // Validation comes before the rate limit so that correcting a typo does
    // not spend the allowance; only attempts that reach the token check and
    // Argon2id do.
    let account = match validate(input) {
        Ok(account) => account,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, "invalid_input", message),
    };
    let username = laysi_ops::ADMIN_USERNAME;

    if !state.setup_rate_limiter.allow(RATE_BUCKET) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "setup_rate_limited",
            "初始化嘗試過於頻繁，請稍後再試",
        );
    }

    if !state.setup.token_accepted(&presented_token) {
        // Logged rather than audited: this is an unauthenticated caller on an
        // installation that has no accounts yet, so an audit row would let
        // anyone who can reach the port fill the table.
        tracing::warn!(%username, "web bootstrap rejected: wrong SETUP_TOKEN");
        return error_response(
            StatusCode::FORBIDDEN,
            "setup_token_invalid",
            "初始化代碼不正確",
        );
    }

    if let Some(response) = activation_refusal(&state, &account).await {
        return response;
    }
    let FirstAccount { password, .. } = account;

    let password_hash =
        match tokio::task::spawn_blocking(move || password::hash_password(&password)).await {
            Ok(Ok(hash)) => hash,
            Ok(Err(error)) => {
                tracing::error!(%error, "hashing the password during bootstrap failed");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "password_hash_error",
                    "目前無法初始化，請稍後再試",
                );
            }
            Err(error) => {
                tracing::error!(%error, "the password hashing task did not finish");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "password_worker_error",
                    "目前無法初始化，請稍後再試",
                );
            }
        };

    let created = state
        .store
        .bootstrap_first_user(
            CreateUser {
                username,
                display_name: BOOTSTRAP_DISPLAY_NAME,
                // Deliberately empty. See the module comment: an address here
                // would let an external identity resolve onto this account.
                email: "",
                password_hash: Some(&password_hash),
                role_code: BOOTSTRAP_ROLE,
                employee_id: None,
                status: "active",
                must_change_password: false,
                created_by: CREATED_BY,
            },
            &user_agent(&headers),
            state.setup.token_required(),
        )
        .await;

    let user = match created {
        Ok(Some(user)) => user,
        Ok(None) => return already_done(),
        Err(error) => {
            tracing::error!(%error, "creating the first account failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "setup_store_error",
                "建立第一個帳號失敗，請稍後再試",
            );
        }
    };

    // Written after the commit, not inside it: the account already exists, and
    // failing the whole initialisation because the trace could not be recorded
    // would leave an installation nobody can sign in to. The parameters carry
    // no password.
    if let Err(error) = state
        .store
        .write_audit(
            &user.display_name,
            &user.role_code,
            "bootstrap_install",
            "high_risk",
            None,
            "ui",
            &json!({ "username": user.username, "role": user.role_code }),
            "由網頁完成系統初始化，建立第一個帳號",
            "ok",
            None,
            crate::auth::client_ip(&headers).as_deref(),
        )
        .await
    {
        tracing::error!(%error, "bootstrap audit write failed; the account was created");
    }

    tracing::info!(
        username = %user.username,
        token_required = state.setup.token_required(),
        "系統已由網頁完成初始化"
    );

    let secret = match issue_session(&state, &user, &headers).await {
        Ok(secret) => secret,
        Err(response) => return response,
    };
    let mut response = Json(json!({
        "ok": true,
        "user": user_json_with_role(&state, &user).await,
    }))
    .into_response();
    attach_session_cookie(&mut response, &secret, &headers);
    response
}

fn already_done() -> Response {
    error_response(
        StatusCode::CONFLICT,
        "setup_already_done",
        "本系統已經完成初始化，請重新整理頁面登入",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(username: &'a str, password_hash: &'a str) -> CreateUser<'a> {
        CreateUser {
            username,
            display_name: BOOTSTRAP_DISPLAY_NAME,
            email: "",
            password_hash: Some(password_hash),
            role_code: BOOTSTRAP_ROLE,
            employee_id: None,
            status: "active",
            must_change_password: false,
            created_by: CREATED_BY,
        }
    }

    fn input(password: &str) -> BootstrapInput {
        BootstrapInput {
            password: password.into(),
            setup_token: String::new(),
            activation_key: "LAYSI-AAAAA-AAAAA-AAAAA-AAAAA".into(),
            license_token: String::new(),
        }
    }

    const GOOD_PASSWORD: &str = "一組夠長的系統初始化測試密碼";

    #[test]
    fn a_well_formed_request_is_accepted() {
        let account = validate(input(GOOD_PASSWORD)).expect("合法輸入應通過驗證");
        assert_eq!(account.password, GOOD_PASSWORD);
    }

    /// Initialisation asks for the key before it creates the account, so a
    /// request without one has nothing to activate and must not get as far as
    /// writing an administrator.
    #[test]
    fn a_request_without_a_licence_is_refused() {
        let mut without = input(GOOD_PASSWORD);
        without.activation_key = "  ".into();
        assert_eq!(validate(without).err(), Some("請輸入經銷商提供的啟動金鑰"));

        // The offline path is the other half of the same requirement.
        let mut offline = input(GOOD_PASSWORD);
        offline.activation_key = String::new();
        offline.license_token = "abc.def".into();
        assert_eq!(
            validate(offline).expect("離線授權檔也算").license_token,
            "abc.def"
        );
    }

    /// The caller supplies a password and nothing else. Everything that
    /// identifies this account is fixed here, so no request can point it at a
    /// person or give it an address an external identity could resolve onto.
    #[test]
    fn the_first_account_is_the_installation_not_a_person() {
        let hash = "argon2-placeholder";
        let created = request(laysi_ops::ADMIN_USERNAME, hash);
        assert_eq!(created.username, "admin");
        assert_eq!(created.email, "");
        assert_eq!(created.role_code, laysi_ops::ROLE_ADMIN);
        assert!(created.employee_id.is_none());
    }

    /// The password is chosen by its owner here, so it must meet the same
    /// minimum the change-password endpoint enforces rather than being a
    /// generated temporary one.
    #[test]
    fn the_password_minimum_matches_the_change_password_endpoint() {
        let short: String = "a".repeat(MIN_PASSWORD_CHARS - 1);
        assert!(validate(input(&short)).is_err());
        let exact: String = "a".repeat(MIN_PASSWORD_CHARS);
        assert!(validate(input(&exact)).is_ok());
    }

    #[test]
    fn an_unset_token_accepts_anything_and_a_set_one_accepts_only_itself() {
        let open = SetupConfig {
            allow_web: true,
            token_hash: None,
        };
        assert!(!open.token_required());
        assert!(open.token_accepted(""));

        let closed = SetupConfig {
            allow_web: true,
            token_hash: Some(Arc::from(laysi_store::token_hash("一組初始化代碼"))),
        };
        assert!(closed.token_required());
        assert!(closed.token_accepted("一組初始化代碼"));
        assert!(closed.token_accepted("  一組初始化代碼  "));
        assert!(!closed.token_accepted(""));
        assert!(!closed.token_accepted("一組初始化代碼x"));
    }

    /// The database, not this module, is what makes initialisation happen at
    /// most once. Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
    /// the_installation_can_only_be_claimed_once -- --ignored`
    ///
    /// Point it at a fresh database to exercise the success path; against one
    /// that already has accounts only the refusal is asserted, which is the
    /// half that must hold on a live installation.
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn the_installation_can_only_be_claimed_once() {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = laysi_store::Store::connect(&database_url, 2)
            .await
            .expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");

        let username = format!("bootstrap-test-{}", uuid::Uuid::new_v4());
        let password_hash =
            password::hash_password("初始化整合測試用的長密碼").expect("應可產生測試密碼雜湊");
        let open = store.bootstrap_open().await.expect("應可查詢初始化狀態");
        let first = store
            .bootstrap_first_user(request(&username, &password_hash), "整合測試", false)
            .await
            .expect("初始化查詢本身不應失敗");
        assert_eq!(
            first.is_some(),
            open,
            "bootstrap_open 與 bootstrap_first_user 必須對同一個安裝給出同樣的答案"
        );

        // Whether or not the first call created anything, the window is now
        // closed and the second call must write nothing.
        let second_username = format!("bootstrap-test-{}", uuid::Uuid::new_v4());
        assert!(
            !store
                .bootstrap_open()
                .await
                .expect("應可再次查詢初始化狀態"),
            "初始化之後視窗必須關閉"
        );
        assert!(
            store
                .bootstrap_first_user(request(&second_username, &password_hash), "整合測試", false)
                .await
                .expect("第二次初始化不應是錯誤，而是不做事")
                .is_none(),
            "第二次初始化必須不建立任何帳號"
        );
        assert!(
            store
                .user_by_username(&second_username)
                .await
                .expect("應可查詢帳號")
                .is_none(),
            "被拒絕的初始化不得留下帳號"
        );

        if let Some(user) = first {
            sqlx::query("DELETE FROM user_account WHERE id = $1")
                .bind(user.id)
                .execute(store.pool())
                .await
                .expect("應可清除測試帳號");
            sqlx::query("DELETE FROM install_bootstrap")
                .execute(store.pool())
                .await
                .expect("應可清除測試的初始化紀錄");
        }
    }

    /// The configured token must not be recoverable from the value the server
    /// keeps in memory.
    #[test]
    fn the_configured_token_is_stored_only_as_a_hash() {
        let secret = "一組初始化代碼";
        let config = SetupConfig {
            allow_web: true,
            token_hash: Some(Arc::from(laysi_store::token_hash(secret))),
        };
        let stored = config.token_hash.as_deref().expect("設定應保存雜湊");
        assert_ne!(stored, secret);
        assert!(!stored.contains(secret));
    }
}
