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
//! The role is fixed to `boss` and cannot be chosen by the caller. A first
//! account without the permission to grant permissions would leave the
//! installation unusable, and an unauthenticated endpoint should not offer a
//! degree of freedom that has no correct value other than one.

use crate::auth::{
    attach_session_cookie, env_bool, error_response, issue_session, user_json_with_role,
    MIN_PASSWORD_CHARS,
};
use crate::Shared;
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

const USERNAME_MIN_CHARS: usize = 3;
const USERNAME_MAX_CHARS: usize = 64;
const DISPLAY_NAME_MAX_CHARS: usize = 64;
const EMAIL_MAX_CHARS: usize = 254;

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
        let token_hash = std::env::var("SETUP_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| Arc::from(laysi_store::token_hash(&value)));
        Ok(Self { allow_web, token_hash })
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self { allow_web: true, token_hash: None }
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
    username: String,
    display_name: String,
    #[serde(default)]
    email: String,
    password: String,
    #[serde(default)]
    setup_token: String,
}

/// A validated account request. Building one is the only way to reach the
/// store, so no field can skip its check.
struct FirstAccount {
    username: String,
    display_name: String,
    email: String,
    password: String,
}

fn validate(input: BootstrapInput) -> Result<FirstAccount, &'static str> {
    let username = input.username.trim().to_string();
    let count = username.chars().count();
    if !(USERNAME_MIN_CHARS..=USERNAME_MAX_CHARS).contains(&count) {
        return Err("登入名稱需要 3 到 64 個字元");
    }
    // Same rule as `create_user_account`: only control characters are refused.
    if username.chars().any(char::is_control) {
        return Err("登入名稱不能包含控制字元");
    }

    let display_name = input.display_name.trim().to_string();
    if display_name.is_empty() {
        return Err("顯示名稱為必填");
    }
    if display_name.chars().count() > DISPLAY_NAME_MAX_CHARS {
        return Err("顯示名稱不可超過 64 個字元");
    }

    let email = input.email.trim().to_string();
    if !email.is_empty()
        && (email.chars().count() > EMAIL_MAX_CHARS
            || !email.contains('@')
            || email.chars().any(char::is_whitespace))
    {
        return Err("電子郵件格式不正確；此欄可以留空");
    }

    // The same minimum as changing a password later. The account is created
    // with the password its owner chose, so unlike `laysi-admin create-user`
    // there is no temporary password to force a change from.
    if input.password.chars().count() < MIN_PASSWORD_CHARS {
        return Err("密碼至少需要 8 個字元");
    }

    Ok(FirstAccount { username, display_name, email, password: input.password })
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
            tracing::error!(%error, "初始化前無法確認安裝狀態");
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
    let FirstAccount { username, display_name, email, password } = match validate(input) {
        Ok(account) => account,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, "invalid_input", message),
    };

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
        tracing::warn!(%username, "網頁初始化的 SETUP_TOKEN 不正確");
        return error_response(
            StatusCode::FORBIDDEN,
            "setup_token_invalid",
            "初始化代碼不正確",
        );
    }

    let password_hash = match tokio::task::spawn_blocking(move || password::hash_password(&password))
        .await
    {
        Ok(Ok(hash)) => hash,
        Ok(Err(error)) => {
            tracing::error!(%error, "初始化時產生密碼雜湊失敗");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "password_hash_error",
                "目前無法初始化，請稍後再試",
            );
        }
        Err(error) => {
            tracing::error!(%error, "密碼雜湊工作無法完成");
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
                username: &username,
                display_name: &display_name,
                email: &email,
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
            tracing::error!(%error, "建立第一個帳號失敗");
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
        )
        .await
    {
        tracing::error!(%error, "初始化稽核紀錄寫入失敗，帳號已建立");
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
    attach_session_cookie(&mut response, &secret, &state);
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
            display_name: "初始化測試",
            email: "",
            password_hash: Some(password_hash),
            role_code: BOOTSTRAP_ROLE,
            employee_id: None,
            status: "active",
            must_change_password: false,
            created_by: CREATED_BY,
        }
    }

    fn input(username: &str, password: &str) -> BootstrapInput {
        BootstrapInput {
            username: username.into(),
            display_name: "負責人".into(),
            email: String::new(),
            password: password.into(),
            setup_token: String::new(),
        }
    }

    const GOOD_PASSWORD: &str = "一組夠長的系統初始化測試密碼";

    #[test]
    fn a_well_formed_request_is_trimmed_and_accepted() {
        let mut raw = input("  owner  ", GOOD_PASSWORD);
        raw.display_name = "  負責人  ".into();
        raw.email = "  owner@example.test  ".into();
        let account = validate(raw).expect("合法輸入應通過驗證");
        assert_eq!(account.username, "owner");
        assert_eq!(account.display_name, "負責人");
        assert_eq!(account.email, "owner@example.test");
    }

    #[test]
    fn usernames_are_bounded_by_length_and_refuse_only_control_characters() {
        assert!(validate(input("ow", GOOD_PASSWORD)).is_err());
        assert!(validate(input(&"o".repeat(65), GOOD_PASSWORD)).is_err());
        assert!(validate(input("own\ter", GOOD_PASSWORD)).is_err());
        assert!(validate(input("own.er_1-2", GOOD_PASSWORD)).is_ok());
        assert!(validate(input("負責人", GOOD_PASSWORD)).is_ok());
        assert!(validate(input("owner name", GOOD_PASSWORD)).is_ok());
        assert!(validate(input("owner'--", GOOD_PASSWORD)).is_ok());
    }

    /// The password is chosen by its owner here, so it must meet the same
    /// minimum the change-password endpoint enforces rather than being a
    /// generated temporary one.
    #[test]
    fn the_password_minimum_matches_the_change_password_endpoint() {
        let short: String = "a".repeat(MIN_PASSWORD_CHARS - 1);
        assert!(validate(input("owner", &short)).is_err());
        let exact: String = "a".repeat(MIN_PASSWORD_CHARS);
        assert!(validate(input("owner", &exact)).is_ok());
    }

    #[test]
    fn an_empty_email_is_allowed_but_a_malformed_one_is_not() {
        let mut raw = input("owner", GOOD_PASSWORD);
        raw.email = String::new();
        assert!(validate(raw).is_ok());

        let mut raw = input("owner", GOOD_PASSWORD);
        raw.email = "not-an-address".into();
        assert!(validate(raw).is_err());
    }

    #[test]
    fn an_unset_token_accepts_anything_and_a_set_one_accepts_only_itself() {
        let open = SetupConfig { allow_web: true, token_hash: None };
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
        let password_hash = password::hash_password("初始化整合測試用的長密碼")
            .expect("應可產生測試密碼雜湊");
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
            !store.bootstrap_open().await.expect("應可再次查詢初始化狀態"),
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
