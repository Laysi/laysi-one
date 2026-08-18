//! REST authentication: password login, server-side sessions, password change.
//!
//! The browser holds only a high-entropy session plaintext while the database
//! keeps a BLAKE3 hash. Every successful check updates `last_seen_at`, and an
//! absolute expiry from the creation time caps a session's total lifetime.

use crate::Shared;
use anyhow::{anyhow, Context};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use laysi_server::password;
use laysi_store::{CreateSession, StoreError, UserAccount, UserSession};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

const SESSION_COOKIE_NAME: &str = "laysi_session";
const DEFAULT_SESSION_IDLE_MINUTES: i64 = 60;
const DEFAULT_SESSION_ABSOLUTE_HOURS: i64 = 168;
pub(crate) const MIN_PASSWORD_CHARS: usize = 12;
const LOGIN_FAILED_MESSAGE: &str = "登入名稱或密碼不正確";

/// Tunable settings for REST sessions.
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    pub cookie_secure: bool,
    pub idle_timeout: Duration,
    pub absolute_timeout: Duration,
}

/// Authentication settings. The dummy password hash makes a non-existent
/// account run Argon2id too, reducing the timing difference.
#[derive(Clone)]
pub struct AuthConfig {
    pub session: SessionConfig,
    dummy_password_hash: Arc<str>,
}

impl AuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let cookie_secure = env_bool("SESSION_COOKIE_SECURE", false)?;
        let idle_minutes =
            env_positive_i64("SESSION_IDLE_MINUTES", DEFAULT_SESSION_IDLE_MINUTES)?;
        let absolute_hours =
            env_positive_i64("SESSION_ABSOLUTE_HOURS", DEFAULT_SESSION_ABSOLUTE_HOURS)?;
        Self::new(SessionConfig {
            cookie_secure,
            idle_timeout: Duration::minutes(idle_minutes),
            absolute_timeout: Duration::hours(absolute_hours),
        })
    }

    fn new(session: SessionConfig) -> anyhow::Result<Self> {
        let dummy_password_hash = password::hash_password("這不是任何使用者的登入密碼")
            .map_err(|e| anyhow!("無法建立登入時間差防護用的 Argon2id 雜湊：{e}"))?;
        Ok(Self { session, dummy_password_hash: dummy_password_hash.into() })
    }

    #[cfg(test)]
    fn for_tests() -> Self {
        Self::new(SessionConfig {
            cookie_secure: false,
            idle_timeout: Duration::minutes(30),
            absolute_timeout: Duration::hours(24),
        })
        .expect("測試認證設定必須有效")
    }
}

fn env_positive_i64(name: &str, default: i64) -> anyhow::Result<i64> {
    let Some(raw) = std::env::var(name).ok().filter(|value| !value.trim().is_empty()) else {
        return Ok(default);
    };
    let value = raw
        .parse::<i64>()
        .with_context(|| format!("{name} 必須是正整數，收到「{raw}」"))?;
    if value <= 0 {
        return Err(anyhow!("{name} 必須大於零，收到「{raw}」"));
    }
    Ok(value)
}

pub(crate) fn env_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    let Some(raw) = std::env::var(name).ok().filter(|value| !value.trim().is_empty()) else {
        return Ok(default);
    };
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(anyhow!("{name} 必須是 true 或 false，收到「{raw}」")),
    }
}

/// An operating identity already resolved from a server-side credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub actor: String,
    pub actor_role: String,
    pub employee_id: Option<i32>,
}

impl VerifiedIdentity {
    pub fn from_user(user: &UserAccount) -> Self {
        Self {
            actor: user.display_name.clone(),
            actor_role: user.role_code.clone(),
            employee_id: user.employee_id,
        }
    }
}

/// An account verified through a session.
#[derive(Debug)]
pub struct VerifiedSession {
    pub session: UserSession,
}

impl VerifiedSession {
    pub fn identity(&self) -> VerifiedIdentity {
        VerifiedIdentity::from_user(&self.session.user)
    }
}

#[derive(Debug)]
pub enum AuthError {
    Unauthorized,
    Store(StoreError),
}

impl From<StoreError> for AuthError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "請先登入再繼續",
            ),
            Self::Store(error) => {
                tracing::error!(%error, "驗證 session 時查詢資料庫失敗");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_store_error",
                    "目前無法驗證登入狀態，請稍後再試",
                )
            }
        }
    }
}

pub(crate) fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

fn cookie_secret(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == SESSION_COOKIE_NAME && !value.is_empty()).then_some(value)
            })
        })
}

fn session_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("lys_s_{}", hex::encode(bytes))
}

fn session_cookie(secret: &str, config: SessionConfig) -> String {
    let secure = if config.cookie_secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE_NAME}={secret}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure}",
        config.absolute_timeout.num_seconds()
    )
}

fn clear_session_cookie(config: SessionConfig) -> String {
    let secure = if config.cookie_secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}"
    )
}

fn set_cookie(response: &mut Response, value: String) {
    match HeaderValue::from_str(&value) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        Err(error) => {
            tracing::error!(%error, "伺服器產生了無效的 session cookie");
        }
    }
}

/// Create a session for a verified account, returning the plaintext token
/// that is handed to the browser exactly once.
pub(crate) async fn issue_session(
    state: &Shared,
    user: &UserAccount,
    headers: &HeaderMap,
) -> Result<String, Response> {
    let secret = session_token();
    let token_hash = laysi_store::token_hash(&secret);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .chars()
        .take(512)
        .collect::<String>();
    let expires_at = Utc::now() + state.auth.session.absolute_timeout;
    state
        .store
        .create_session(CreateSession {
            token_hash: &token_hash,
            user_id: user.id,
            expires_at,
            user_agent: &user_agent,
            ip: "",
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "建立登入 session 失敗");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_create_error",
                "目前無法建立登入狀態，請稍後再試",
            )
        })?;
    Ok(secret)
}

/// Attach the session cookie to a login or OIDC callback response.
pub(crate) fn attach_session_cookie(response: &mut Response, secret: &str, state: &Shared) {
    set_cookie(response, session_cookie(secret, state.auth.session));
}

/// Resolve the UI identity from the session cookie alone; no other request
/// header participates.
pub async fn authenticate_session(
    state: &Shared,
    headers: &HeaderMap,
) -> Result<VerifiedSession, AuthError> {
    let secret = cookie_secret(headers).ok_or(AuthError::Unauthorized)?;
    let token_hash = laysi_store::token_hash(secret);
    let idle_cutoff = Utc::now() - state.auth.session.idle_timeout;
    let session = state
        .store
        .session_by_hash(&token_hash, idle_cutoff)
        .await?
        .ok_or(AuthError::Unauthorized)?;

    // The lookup and the update apply the same idle cutoff, so a session that
    // has just expired is not silently extended.
    if !state.store.touch_session(&token_hash, idle_cutoff).await? {
        return Err(AuthError::Unauthorized);
    }
    Ok(VerifiedSession { session })
}

#[derive(Deserialize)]
pub struct LoginInput {
    username: String,
    password: String,
}

#[derive(Clone, Copy)]
enum LoginFailure {
    UnknownUser,
    WrongPassword,
}

fn login_failure_response(_reason: LoginFailure) -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        LOGIN_FAILED_MESSAGE,
    )
}

async fn password_matches(password: String, encoded: String) -> Result<bool, Response> {
    tokio::task::spawn_blocking(move || password::verify_password(&password, &encoded))
        .await
        .map_err(|error| {
            tracing::error!(%error, "密碼驗證工作無法完成");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "password_worker_error",
                "目前無法驗證密碼，請稍後再試",
            )
        })
}

pub async fn login(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> Response {
    match state.store.user_count().await {
        Ok(0) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "no_user_account",
                "系統尚未建立任何使用者帳號，請先完成初始化",
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(%error, "登入前無法確認帳號數量");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth_store_error",
                "目前無法登入，請稍後再試",
            );
        }
    }

    let user = match state.store.user_by_username(input.username.trim()).await {
        Ok(user) => user,
        Err(error) => {
            tracing::error!(%error, "登入時查詢帳號失敗");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth_store_error",
                "目前無法登入，請稍後再試",
            );
        }
    };
    let encoded = user
        .as_ref()
        .and_then(|account| account.password_hash.clone())
        .unwrap_or_else(|| state.auth.dummy_password_hash.to_string());
    let verified = match password_matches(input.password, encoded).await {
        Ok(verified) => verified,
        Err(response) => return response,
    };

    let Some(user) = user else {
        return login_failure_response(LoginFailure::UnknownUser);
    };
    if !verified || user.password_hash.is_none() || user.status != "active" {
        return login_failure_response(LoginFailure::WrongPassword);
    }

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

pub async fn logout(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(secret) = cookie_secret(&headers) {
        let token_hash = laysi_store::token_hash(secret);
        if let Err(error) = state.store.revoke_session(&token_hash, "使用者登出").await {
            tracing::error!(%error, "撤銷登入 session 失敗");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_revoke_error",
                "目前無法安全登出，請稍後再試",
            );
        }
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    set_cookie(&mut response, clear_session_cookie(state.auth.session));
    response
}

pub async fn me(State(state): State<Shared>, headers: HeaderMap) -> Response {
    match authenticate_session(&state, &headers).await {
        Ok(verified) => Json(json!({
            "ok": true,
            "user": user_json_with_role(&state, &verified.session.user).await,
            "oauth": { "enabled": state.oidc.is_some() },
            "setup": setup_json(&state, true).await,
        }))
        .into_response(),
        // `oauth` and `setup` are returned even when signed out. The signed-out
        // screen needs to know whether to render "sign in with OAuth" and
        // whether this installation still has no accounts, and this is the only
        // place it can ask, since every other endpoint requires a session.
        // Without these fields the frontend would probe
        // `/v1/auth/oauth/start` and leave an orphan PKCE entry behind.
        // Neither fact is a secret: OAuth is visible on the login screen, and
        // an empty installation already says so through login's 503.
        Err(AuthError::Unauthorized) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "ok": false,
                "error": { "code": "unauthorized", "message": "請先登入再繼續" },
                "oauth": { "enabled": state.oidc.is_some() },
                "setup": setup_json(&state, false).await,
            })),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

/// Whether the browser may still initialise this installation.
///
/// Holding a session proves an account exists, so the signed-in path answers
/// without asking the database. A store error reports the window as closed:
/// the endpoint would refuse anyway, and offering initialisation on a
/// guess is the wrong way round.
async fn setup_json(state: &Shared, signed_in: bool) -> Value {
    let available = !signed_in
        && state.setup.allow_web()
        && match state.store.bootstrap_open().await {
            Ok(open) => open,
            Err(error) => {
                tracing::error!(%error, "查詢初始化狀態失敗");
                false
            }
        };
    json!({
        "bootstrap_available": available,
        "token_required": state.setup.token_required(),
    })
}

#[derive(Deserialize)]
pub struct ChangePasswordInput {
    current_password: String,
    new_password: String,
}

pub async fn change_password(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<ChangePasswordInput>,
) -> Response {
    let verified = match authenticate_session(&state, &headers).await {
        Ok(verified) => verified,
        Err(error) => return error.into_response(),
    };
    let Some(current_hash) = verified.session.user.password_hash.clone() else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "local_password_not_set",
            "本帳號未設定本機密碼，請洽管理員重設密碼",
        );
    };
    let current_matches = match password_matches(input.current_password, current_hash).await {
        Ok(matches) => matches,
        Err(response) => return response,
    };
    if !current_matches {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "current_password_incorrect",
            "目前密碼不正確",
        );
    }
    if input.new_password.chars().count() < MIN_PASSWORD_CHARS {
        return error_response(
            StatusCode::BAD_REQUEST,
            "password_too_short",
            "新密碼至少需要 12 個字元",
        );
    }

    let new_password = input.new_password;
    let new_hash = match tokio::task::spawn_blocking(move || password::hash_password(&new_password))
        .await
    {
        Ok(Ok(hash)) => hash,
        Ok(Err(error)) => {
            tracing::error!(%error, "產生新密碼雜湊失敗");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "password_hash_error",
                "目前無法更新密碼，請稍後再試",
            );
        }
        Err(error) => {
            tracing::error!(%error, "密碼雜湊工作無法完成");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "password_worker_error",
                "目前無法更新密碼，請稍後再試",
            );
        }
    };

    match state
        .store
        .set_password_hash(verified.session.user.id, Some(&new_hash), false)
        .await
    {
        Ok(user) => {
            let body = json!({ "ok": true, "user": user_json_with_role(&state, &user).await });
            Json(body).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "更新密碼雜湊失敗");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "password_update_error",
                "目前無法更新密碼，請稍後再試",
            )
        }
    }
}

fn user_json(user: &UserAccount) -> Value {
    json!({
        "id": user.id,
        "username": user.username,
        "display_name": user.display_name,
        "email": user.email,
        "role": user.role_code,
        "employee_id": user.employee_id,
        "must_change_password": user.must_change_password,
    })
}

/// `user_json` plus the role's display name.
///
/// An unknown name falls back to the code: the sidebar showing `acct` is
/// better than the whole login response failing because a role was renamed.
pub(crate) async fn user_json_with_role(state: &Shared, user: &UserAccount) -> Value {
    let name = match state.store.role_name(&user.role_code).await {
        Ok(name) => name,
        Err(error) => {
            tracing::error!(%error, "查詢角色名稱失敗，改用角色代碼顯示");
            None
        }
    };

    // The operations this role may call, so the sidebar can list only the
    // sections the person can work in. This is the same set `dispatch`
    // consults, read from the same tables; it decides what is drawn, never
    // what is allowed. Every call is still checked server-side, so a stale or
    // tampered copy of this list gains nothing.
    //
    // An empty list on error rather than a fabricated one: a sidebar with no
    // sections is visibly wrong, while a full sidebar that then refuses every
    // click looks like the product is broken.
    let permissions = match state.store.permitted_ops(&user.role_code).await {
        Ok(ops) => {
            let mut ops: Vec<String> = ops.into_iter().collect();
            ops.sort();
            ops
        }
        Err(error) => {
            tracing::error!(%error, "查詢角色權限失敗，導覽將不列出任何分段");
            Vec::new()
        }
    };

    let mut value = user_json(user);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "role_name".into(),
            json!(name.unwrap_or_else(|| user.role_code.clone())),
        );
        object.insert("permissions".into(), json!(permissions));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::Row;

    fn test_state() -> Shared {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgres@127.0.0.1/laysi_test")
            .expect("測試用延遲連線池必須可建立");
        Arc::new(crate::AppState {
            store: laysi_store::Store::from_pool(pool),
            registry: laysi_ops::Registry::new(),
            brand: laysi_brand::Report {
                files: 0,
                signed: true,
                fingerprint: "test".into(),
            },
            objects: None,
            rustfs_endpoint: String::new(),
            auth: AuthConfig::for_tests(),
            setup: crate::setup::SetupConfig::for_tests(),
            oidc: None,
            mcp_rate_limiter: crate::ratelimit::RateLimiter::new(60),
            setup_rate_limiter: crate::ratelimit::RateLimiter::new(
                crate::setup::ATTEMPTS_PER_MINUTE,
            ),
            mcp_allowed_origins: Vec::new(),
        })
    }

    #[tokio::test]
    async fn forged_actor_headers_without_session_are_unauthorized() {
        let state = test_state();
        let mut headers = HeaderMap::new();
        headers.insert(
            concat!("x-laysi-", "actor"),
            HeaderValue::from_static("forged"),
        );
        headers.insert(
            concat!("x-laysi-", "role"),
            HeaderValue::from_static("boss"),
        );

        let response = authenticate_session(&state, &headers)
            .await
            .expect_err("沒有 session cookie 必須被拒絕")
            .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn verified_identity_never_reads_impersonation_headers() {
        let state = test_state();
        let identity = VerifiedIdentity {
            actor: "可信帳號".into(),
            actor_role: "acct".into(),
            employee_id: Some(7),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            concat!("x-laysi-", "actor"),
            HeaderValue::from_static("forged"),
        );
        headers.insert(
            concat!("x-laysi-", "role"),
            HeaderValue::from_static("boss"),
        );

        // `ctx_from`'s type signature does not accept a HeaderMap at all, so a
        // forged header has no path into identity resolution.
        let ctx = crate::ctx_from(&state, &identity, None, laysi_ops::Channel::Ui);
        assert_eq!(ctx.actor, "可信帳號");
        assert_eq!(ctx.actor_role, "acct");
        assert_eq!(ctx.employee_id, Some(7));
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn session_tokens_have_32_bytes_of_random_material_and_only_hashes_are_stored() {
        let secret = session_token();
        let encoded = secret.strip_prefix("lys_s_").expect("session token 應有固定前綴");
        assert_eq!(encoded.len(), 64);
        assert_ne!(laysi_store::token_hash(&secret), secret);
    }

    #[tokio::test]
    async fn unknown_user_and_wrong_password_share_one_failure_message() {
        let unknown = login_failure_response(LoginFailure::UnknownUser);
        let wrong = login_failure_response(LoginFailure::WrongPassword);
        assert_eq!(unknown.status(), wrong.status());
        let unknown_body = axum::body::to_bytes(unknown.into_body(), usize::MAX)
            .await
            .expect("應可讀取登入失敗回應");
        let wrong_body = axum::body::to_bytes(wrong.into_body(), usize::MAX)
            .await
            .expect("應可讀取登入失敗回應");
        assert_eq!(unknown_body, wrong_body);
        assert!(
            std::str::from_utf8(&unknown_body)
                .expect("JSON 必須是 UTF-8")
                .contains(LOGIN_FAILED_MESSAGE)
        );
    }

    #[test]
    fn cookie_flags_match_the_session_security_contract() {
        let config = AuthConfig::for_tests().session;
        let cookie = session_cookie("secret", config);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(!cookie.contains("Secure"));

        let secure = session_cookie(
            "secret",
            SessionConfig { cookie_secure: true, ..config },
        );
        assert!(secure.contains("; Secure"));
    }

    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
    /// database_sessions_are_hashed_expiring_and_revocable -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn database_sessions_are_hashed_expiring_and_revocable() {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = laysi_store::Store::connect(&database_url, 2)
            .await
            .expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");

        let username = format!("session-test-{}", uuid::Uuid::new_v4());
        let password_hash = password::hash_password("session integration test password")
            .expect("應可產生測試密碼雜湊");
        let user = store
            .create_user(laysi_store::CreateUser {
                username: &username,
                display_name: "工作階段測試",
                email: "",
                password_hash: Some(&password_hash),
                role_code: "staff",
                employee_id: None,
                status: "active",
                must_change_password: false,
                created_by: "整合測試",
            })
            .await
            .expect("應可建立測試帳號");
        let idle_cutoff = Utc::now() - Duration::minutes(30);

        let secret = session_token();
        let token_hash = laysi_store::token_hash(&secret);
        let live = store
            .create_session(CreateSession {
                token_hash: &token_hash,
                user_id: user.id,
                expires_at: Utc::now() + Duration::hours(1),
                user_agent: "整合測試",
                ip: "",
            })
            .await
            .expect("應可建立測試 session");
        let stored_hash: String =
            sqlx::query("SELECT token_hash FROM user_session WHERE id = $1")
                .bind(live.id)
                .fetch_one(store.pool())
                .await
                .expect("應可讀回測試 session")
                .get("token_hash");
        assert_eq!(stored_hash, token_hash);
        assert_ne!(stored_hash, secret);
        assert!(store
            .session_by_hash(&token_hash, idle_cutoff)
            .await
            .expect("應可驗證有效 session")
            .is_some());

        assert!(store
            .revoke_session(&token_hash, "整合測試")
            .await
            .expect("應可撤銷 session"));
        assert!(store
            .session_by_hash(&token_hash, idle_cutoff)
            .await
            .expect("應可查撤銷後狀態")
            .is_none());

        let expired_secret = session_token();
        let expired_hash = laysi_store::token_hash(&expired_secret);
        store
            .create_session(CreateSession {
                token_hash: &expired_hash,
                user_id: user.id,
                expires_at: Utc::now() - Duration::seconds(1),
                user_agent: "整合測試",
                ip: "",
            })
            .await
            .expect("資料庫可保存已達絕對上限的測試 session");
        assert!(store
            .session_by_hash(&expired_hash, idle_cutoff)
            .await
            .expect("應可判定絕對到期")
            .is_none());

        let idle_secret = session_token();
        let idle_hash = laysi_store::token_hash(&idle_secret);
        let idle = store
            .create_session(CreateSession {
                token_hash: &idle_hash,
                user_id: user.id,
                expires_at: Utc::now() + Duration::hours(1),
                user_agent: "整合測試",
                ip: "",
            })
            .await
            .expect("應可建立閒置到期測試 session");
        sqlx::query(
            "UPDATE user_session
                SET last_seen_at = now() - interval '2 hours'
              WHERE id = $1",
        )
        .bind(idle.id)
        .execute(store.pool())
        .await
        .expect("應可調整測試 session 的最後活動時間");
        assert!(store
            .session_by_hash(&idle_hash, idle_cutoff)
            .await
            .expect("應可判定閒置到期")
            .is_none());

        sqlx::query("DELETE FROM user_account WHERE id = $1")
            .bind(user.id)
            .execute(store.pool())
            .await
            .expect("應可清除測試帳號");
    }
}
