//! REST authentication: password login, server-side sessions, password change.
//!
//! The browser holds only a high-entropy session plaintext while the database
//! keeps a BLAKE3 hash. Every successful check updates `last_seen_at`, and an
//! absolute expiry from the creation time caps a session's total lifetime.

use crate::Shared;
use anyhow::anyhow;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use laysi_server::password;
use laysi_store::{CreateSession, StoreError, UserAccount, UserSession};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;

const SESSION_COOKIE_NAME: &str = "laysi_session";
pub(crate) const MIN_PASSWORD_CHARS: usize = 8;
const LOGIN_FAILED_MESSAGE: &str = "登入名稱或密碼不正確";

/// How long a session may sit idle, from `company_setting`.
pub fn idle_timeout() -> Duration {
    Duration::minutes(laysi_ops::session_idle_minutes())
}

/// How long a session may live at all, from `company_setting`.
pub fn absolute_timeout() -> Duration {
    Duration::hours(laysi_ops::session_max_hours())
}

/// Whether the session cookie gets the `Secure` flag.
///
/// Derived from the request rather than configured. A flag somebody has to
/// remember to flip when the site moves behind TLS is a flag that ships a
/// session cookie in the clear, and always setting it would break the office
/// server reached over plain HTTP at a LAN address, where `localhost` does not
/// apply. `X-Forwarded-Proto` is what a TLS proxy says, and the bundled Caddy
/// preserves it from private ranges only.
fn cookie_secure(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .next()
                .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
        })
}

/// Source address of the request, for the audit record.
///
/// Read from what the reverse proxy says, taking the first hop of
/// `X-Forwarded-For` and falling back to `X-Real-IP`. Parsed as an address and
/// dropped when it is not one: the header is client-supplied when no proxy
/// rewrites it, and an audit row that stores whatever arrived is a place to
/// put text of the caller's choosing. Never used for a decision -- only
/// recorded, so a wrong value misleads an investigation but grants nothing.
pub(crate) fn client_ip(headers: &HeaderMap) -> Option<String> {
    ["x-forwarded-for", "x-real-ip"]
        .iter()
        .filter_map(|name| headers.get(*name))
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(',').next())
        .find_map(|v| v.trim().parse::<std::net::IpAddr>().ok())
        .map(|ip| ip.to_string())
}

/// Authentication settings. The dummy password hash makes a non-existent
/// account run Argon2id too, reducing the timing difference.
#[derive(Clone)]
pub struct AuthConfig {
    dummy_password_hash: Arc<str>,
}

impl AuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::new()
    }

    fn new() -> anyhow::Result<Self> {
        let dummy_password_hash = password::hash_password("這不是任何使用者的登入密碼")
            .map_err(|e| anyhow!("無法建立登入時間差防護用的 Argon2id 雜湊：{e}"))?;
        Ok(Self {
            dummy_password_hash: dummy_password_hash.into(),
        })
    }

    #[cfg(test)]
    fn for_tests() -> Self {
        Self::new().expect("測試認證設定必須有效")
    }
}

pub(crate) fn env_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    let Some(raw) = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
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
    /// Login name, absent when the credential is an MCP token.
    pub username: Option<String>,
    pub employee_id: Option<i32>,
}

impl VerifiedIdentity {
    pub fn from_user(user: &UserAccount) -> Self {
        Self {
            actor: user.display_name.clone(),
            actor_role: user.role_code.clone(),
            username: Some(user.username.clone()),
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
            Self::Unauthorized => {
                error_response(StatusCode::UNAUTHORIZED, "unauthorized", "請先登入再繼續")
            }
            Self::Store(error) => {
                tracing::error!(%error, "the database lookup while verifying the session failed");
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
    getrandom::fill(&mut bytes).expect("OS entropy is unavailable");
    format!("lys_s_{}", hex::encode(bytes))
}

fn session_cookie(secret: &str, secure_flag: bool) -> String {
    let secure = if secure_flag { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE_NAME}={secret}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure}",
        absolute_timeout().num_seconds()
    )
}

fn clear_session_cookie(secure_flag: bool) -> String {
    let secure = if secure_flag { "; Secure" } else { "" };
    format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}")
}

fn set_cookie(response: &mut Response, value: String) {
    match HeaderValue::from_str(&value) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        Err(error) => {
            tracing::error!(%error, "the server produced an invalid session cookie");
        }
    }
}

/// Create a session for a verified account, returning the plaintext token
/// that is handed to the browser exactly once.
// The error is the response to send, which is how an axum handler carries a
// refusal; 128 bytes on a login path is not worth boxing and unboxing.
#[allow(clippy::result_large_err)]
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
    let expires_at = Utc::now() + absolute_timeout();
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
            tracing::error!(%error, "creating the login session failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_create_error",
                "目前無法建立登入狀態，請稍後再試",
            )
        })?;
    Ok(secret)
}

/// Attach the session cookie to a login or OIDC callback response.
pub(crate) fn attach_session_cookie(response: &mut Response, secret: &str, headers: &HeaderMap) {
    set_cookie(response, session_cookie(secret, cookie_secure(headers)));
}

/// Resolve the UI identity from the session cookie alone; no other request
/// header participates.
pub async fn authenticate_session(
    state: &Shared,
    headers: &HeaderMap,
) -> Result<VerifiedSession, AuthError> {
    let secret = cookie_secret(headers).ok_or(AuthError::Unauthorized)?;
    let token_hash = laysi_store::token_hash(secret);
    let idle_cutoff = Utc::now() - idle_timeout();
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

/// Record an authentication event in `ai_audit_log`.
///
/// The endpoints under `/v1/auth` do not go through `Registry::dispatch`, so
/// without this a sign-in, a sign-out and a password change left nothing
/// behind but a line in the container's stdout, which the next restart drops.
/// Access to an account is the event an investigation starts from.
///
/// `actor_role` is empty for a name that has no account. The screen reads it
/// through a fallback rather than showing a code.
///
/// The summaries here separate "no such account" from "wrong password" while
/// the response to the caller does not: the distinction is what makes the
/// record useful, and only an administrator can read it back.
pub(crate) async fn write_auth_audit(
    state: &Shared,
    headers: &HeaderMap,
    actor: &str,
    actor_role: &str,
    event: &str,
    summary: &str,
    outcome: &str,
) {
    if let Err(error) = state
        .store
        .write_audit(
            actor,
            actor_role,
            event,
            "read_only",
            None,
            "ui",
            &json!({}),
            summary,
            outcome,
            None,
            client_ip(headers).as_deref(),
        )
        .await
    {
        tracing::error!(%error, event, "authentication audit write failed");
    }
}

fn login_failure_response(_reason: LoginFailure) -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        LOGIN_FAILED_MESSAGE,
    )
}

/// How many Argon2id verifications run at once.
///
/// Login verifies every attempt, against `dummy_password_hash` when the name
/// has no account, so a flood of made-up names is verification work the rate
/// limit does not see. Unbounded, each would hold 19 MiB on one of up to 512
/// blocking threads. One per core keeps memory flat while sign-in queues.
///
/// The slot travels with the blocking task rather than the request: a client
/// that disconnects drops the request, not the verification already running,
/// and a slot released on disconnect would let disconnecting clients run any
/// number of them.
static VERIFY_SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    Arc::new(Semaphore::new(
        std::thread::available_parallelism().map_or(1, |n| n.get()),
    ))
});

// The error is the response to send; see `issue_session`.
#[allow(clippy::result_large_err)]
async fn password_matches(password: String, encoded: String) -> Result<bool, Response> {
    verify_in(&VERIFY_SLOTS, password, encoded).await
}

#[allow(clippy::result_large_err)]
async fn verify_in(
    slots: &Arc<Semaphore>,
    password: String,
    encoded: String,
) -> Result<bool, Response> {
    let slot = Arc::clone(slots)
        .acquire_owned()
        .await
        .map_err(verification_failed)?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        password::verify_password(&password, &encoded)
    })
    .await
    .map_err(verification_failed)
}

fn verification_failed(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "the password verification task did not finish");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "password_worker_error",
        "目前無法驗證密碼，請稍後再試",
    )
}

/// Login attempts per minute for one login name.
///
/// Argon2id already makes each attempt expensive, so this is not the only
/// defence; it is here because guessing at machine speed against one account
/// should not be free. The bucket is the login name rather than the client
/// address: behind a reverse proxy every request arrives from the same
/// address, which would turn one attacker into a lockout for everybody.
pub const LOGIN_ATTEMPTS_PER_MINUTE: u32 = 10;

pub async fn login(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> Response {
    if !state.login_rate_limiter.allow(input.username.trim()) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "login_rate_limited",
            "登入嘗試過於頻繁，請稍候再試",
        );
    }

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
            tracing::error!(%error, "cannot count the accounts before sign-in");
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
            tracing::error!(%error, "the account lookup during sign-in failed");
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

    // Both failure paths write exactly one audit row, so the work done before
    // the shared response is the same either way.
    let Some(user) = user else {
        write_auth_audit(
            &state,
            &headers,
            input.username.trim(),
            "",
            "login",
            "登入失敗：查無此帳號",
            "blocked",
        )
        .await;
        return login_failure_response(LoginFailure::UnknownUser);
    };
    if !verified || user.password_hash.is_none() || user.status != "active" {
        let why = match (verified, user.status.as_str()) {
            (true, "active") => "登入失敗：本帳號只用外部登入",
            (true, _) => "登入失敗：帳號已停用",
            _ => "登入失敗：密碼不正確",
        };
        write_auth_audit(
            &state,
            &headers,
            &user.display_name,
            &user.role_code,
            "login",
            why,
            "blocked",
        )
        .await;
        return login_failure_response(LoginFailure::WrongPassword);
    }

    // Checked only after the password has already been confirmed correct, so
    // this never becomes a second way to learn whether a login name exists.
    // The system administrator role is exempt: turning this on and then
    // having the IdP break would otherwise strand the whole installation
    // with no way in and no way to reach the settings screen that could turn
    // it back off.
    if user.role_code != laysi_ops::ROLE_ADMIN {
        if let Some(oidc) = crate::oidc::current(&state).await {
            if oidc.password_login_disabled() {
                write_auth_audit(
                    &state,
                    &headers,
                    &user.display_name,
                    &user.role_code,
                    "login",
                    "登入失敗：本站已停用帳號密碼登入",
                    "blocked",
                )
                .await;
                return error_response(
                    StatusCode::FORBIDDEN,
                    "password_login_disabled",
                    &format!("本站已改用{}登入，請使用上方的登入按鈕", oidc.label()),
                );
            }
        }
    }

    let secret = match issue_session(&state, &user, &headers).await {
        Ok(secret) => secret,
        Err(response) => return response,
    };
    write_auth_audit(
        &state,
        &headers,
        &user.display_name,
        &user.role_code,
        "login",
        "登入成功",
        "ok",
    )
    .await;

    let mut response = Json(json!({
        "ok": true,
        "user": user_json_with_role(&state, &user).await,
    }))
    .into_response();
    attach_session_cookie(&mut response, &secret, &headers);
    response
}

pub async fn logout(State(state): State<Shared>, headers: HeaderMap) -> Response {
    // Read before the session is revoked; afterwards there is no name to
    // record. A request with no valid session logs nothing: there is no
    // identity to attribute it to and the caller learns nothing either way.
    if let Ok(verified) = authenticate_session(&state, &headers).await {
        let user = &verified.session.user;
        write_auth_audit(
            &state,
            &headers,
            &user.display_name,
            &user.role_code,
            "logout",
            "登出",
            "ok",
        )
        .await;
    }

    if let Some(secret) = cookie_secret(&headers) {
        let token_hash = laysi_store::token_hash(secret);
        if let Err(error) = state.store.revoke_session(&token_hash, "使用者登出").await {
            tracing::error!(%error, "revoking the login session failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_revoke_error",
                "目前無法安全登出，請稍後再試",
            );
        }
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    set_cookie(&mut response, clear_session_cookie(cookie_secure(&headers)));
    response
}

pub async fn me(State(state): State<Shared>, headers: HeaderMap) -> Response {
    match authenticate_session(&state, &headers).await {
        Ok(verified) => Json(json!({
            "ok": true,
            "user": user_json_with_role(&state, &verified.session.user).await,
            "oauth": crate::oidc::describe(&state).await,
            "setup": setup_json(&state, true).await,
            "brand": brand_json(&state).await,
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
                "oauth": crate::oidc::describe(&state).await,
                "setup": setup_json(&state, false).await,
                "brand": brand_json(&state).await,
            })),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

/// The company identity the sign-in screen and the sidebar draw.
///
/// Answered without a session because the sign-in screen is one of the two
/// places that show it, and what it carries is what the company chose to put
/// on that page. The logo is left out when the display mode does not use it:
/// it is hundreds of kilobytes travelling with every load, and an unused one
/// would be paid for on every install that never uploaded a logo.
pub async fn brand_json(state: &Shared) -> Value {
    let settings = match state.store.company_settings().await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::error!(%error, "reading the company brand failed");
            Vec::new()
        }
    };
    let value = |code: &str| {
        settings
            .iter()
            .find(|setting| setting.code == code)
            .map(|setting| setting.value.clone())
            .unwrap_or_default()
    };
    let logo = value(laysi_ops::ops::settings::BRAND_LOGO_SETTING);
    let mut display = value(laysi_ops::ops::settings::BRAND_DISPLAY_SETTING);
    // With no logo there is nothing for the other two modes to draw, and an
    // empty box is worse than the name.
    if logo.is_empty() || !laysi_ops::ops::settings::BRAND_DISPLAYS.contains(&display.as_str()) {
        display = laysi_ops::ops::settings::BRAND_DISPLAYS[0].to_string();
    }
    let shows_logo = display != laysi_ops::ops::settings::BRAND_DISPLAYS[0];
    json!({
        "name": value("company_name"),
        "display": display,
        "logo": if shows_logo { logo } else { String::new() },
    })
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
                tracing::error!(%error, "the bootstrap state lookup failed");
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
    #[serde(default)]
    current_password: Option<String>,
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
    // The forced first change is the step right after the login that proved the
    // current password, so asking for it again establishes nothing the session
    // has not already established. Every other change still has to prove it.
    if !verified.session.user.must_change_password {
        let Some(current_hash) = verified.session.user.password_hash.clone() else {
            return error_response(
                StatusCode::BAD_REQUEST,
                "local_password_not_set",
                "本帳號未設定本機密碼，請洽管理員重設密碼",
            );
        };
        let current_matches = match password_matches(
            input.current_password.unwrap_or_default(),
            current_hash,
        )
        .await
        {
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
    }
    if input.new_password.chars().count() < MIN_PASSWORD_CHARS {
        return error_response(
            StatusCode::BAD_REQUEST,
            "password_too_short",
            "新密碼至少需要 8 個字元",
        );
    }

    let new_password = input.new_password;
    let new_hash =
        match tokio::task::spawn_blocking(move || password::hash_password(&new_password)).await {
            Ok(Ok(hash)) => hash,
            Ok(Err(error)) => {
                tracing::error!(%error, "hashing the new password failed");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "password_hash_error",
                    "目前無法更新密碼，請稍後再試",
                );
            }
            Err(error) => {
                tracing::error!(%error, "the password hashing task did not finish");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "password_worker_error",
                    "目前無法更新密碼，請稍後再試",
                );
            }
        };

    let session = &verified.session;
    match state
        .store
        .change_own_password(
            session.user.id,
            &new_hash,
            session.id,
            &session.user.display_name,
        )
        .await
    {
        Ok((user, revoked)) => {
            write_auth_audit(
                &state,
                &headers,
                &user.display_name,
                &user.role_code,
                "change_password",
                &format!("變更密碼 · 登出其他 {revoked} 個工作階段"),
                "ok",
            )
            .await;
            let body = json!({ "ok": true, "user": user_json_with_role(&state, &user).await });
            Json(body).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "updating the password hash failed");
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

/// Whether a role clears an operation's risk floor.
///
/// An operation missing from the registry is left in: the permission tables
/// and the registry are kept equal by a test, so a mismatch is a bug to see
/// rather than a row to hide.
fn callable_by(registry: &laysi_ops::Registry, name: &str, role: &str) -> bool {
    registry
        .get(name)
        .is_none_or(|op| op.risk().allows_role(role))
}

/// `user_json` plus the role's display name.
///
/// An unknown name falls back to the code: the sidebar showing `acct` is
/// better than the whole login response failing because a role was renamed.
pub(crate) async fn user_json_with_role(state: &Shared, user: &UserAccount) -> Value {
    let name = match state.store.role_name(&user.role_code).await {
        Ok(name) => name,
        Err(error) => {
            tracing::error!(%error, "the role name lookup failed, falling back to the role code");
            None
        }
    };

    // The operations this role may call, so the sidebar can list only the
    // sections the person can work in. This is the same set `dispatch`
    // consults, read from the same tables; it decides what is drawn, never
    // what is allowed. Every call is still checked server-side, so a stale or
    // tampered copy of this list gains nothing.
    //
    // The two risk floors are subtracted here as well, because `dispatch`
    // checks them before it checks the matrix. The owner holds
    // `update_company_setting` in the matrix but is refused by the admin
    // floor, so a list that kept it made the settings screen draw a pencil,
    // accept a value, and fail on save. This list has to mean "what this
    // identity can actually call".
    //
    // An empty list on error rather than a fabricated one: a sidebar with no
    // sections is visibly wrong, while a full sidebar that then refuses every
    // click looks like the product is broken.
    let permissions = match state.store.permitted_ops(&user.role_code).await {
        Ok(ops) => {
            let mut ops: Vec<String> = ops
                .into_iter()
                .filter(|name| callable_by(&state.registry, name, &user.role_code))
                .collect();
            ops.sort();
            ops
        }
        Err(error) => {
            tracing::error!(%error, "the role permission lookup failed, navigation will list no sections");
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

    /// Held for longer than a verification takes, so a verification that
    /// skipped the slot would have finished by the check.
    #[tokio::test]
    async fn a_verification_waits_for_a_free_slot() {
        let slots = Arc::new(Semaphore::new(1));
        let encoded = password::hash_password("正確的密碼").expect("應可產生密碼雜湊");
        let held = slots.acquire().await.expect("名額不會被關閉");

        let waiting = Arc::clone(&slots);
        let pending =
            tokio::spawn(async move { verify_in(&waiting, "正確的密碼".into(), encoded).await });
        tokio::task::spawn_blocking(|| std::thread::sleep(std::time::Duration::from_millis(500)))
            .await
            .expect("等待不會失敗");
        assert!(!pending.is_finished(), "名額用完時要等");

        drop(held);
        let verified = pending.await.expect("驗證任務應完成");
        assert!(matches!(verified, Ok(true)), "有名額之後照常驗證");
    }

    /// A client that disconnects drops the request, not the verification
    /// already running on a blocking thread.
    #[tokio::test]
    async fn a_cancelled_request_keeps_its_slot_until_the_verification_ends() {
        let slots = Arc::new(Semaphore::new(1));
        let encoded = password::hash_password("正確的密碼").expect("應可產生密碼雜湊");

        let running = Arc::clone(&slots);
        let request =
            tokio::spawn(async move { verify_in(&running, "正確的密碼".into(), encoded).await });
        tokio::task::yield_now().await;
        assert_eq!(slots.available_permits(), 0, "驗證開始後名額應被佔用");

        request.abort();
        assert!(request.await.is_err(), "請求應已被取消");
        assert_eq!(slots.available_permits(), 0, "驗證還在跑，名額不能先還");

        let _slot = slots.acquire().await.expect("驗證結束後名額應回來");
    }

    fn test_state() -> Shared {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgres@127.0.0.1/laysi_test")
            .expect("測試用延遲連線池必須可建立");
        state_with(laysi_store::Store::from_pool(pool))
    }

    fn state_with(store: laysi_store::Store) -> Shared {
        Arc::new(crate::AppState {
            store,
            registry: laysi_ops::Registry::new(),
            objects: None,
            rustfs_endpoint: String::new(),
            auth: AuthConfig::for_tests(),
            setup: crate::setup::SetupConfig::for_tests(),
            oidc: tokio::sync::RwLock::new(crate::oidc::OidcCache::default()),
            mcp_rate_limiter: crate::ratelimit::RateLimiter::new(60),
            login_rate_limiter: crate::ratelimit::RateLimiter::new(LOGIN_ATTEMPTS_PER_MINUTE),
            oauth_register_rate_limiter: crate::ratelimit::RateLimiter::new(
                crate::oauth::REGISTRATIONS_PER_MINUTE,
            ),
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
            username: Some("trusted".into()),
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
        // forged header has no path into identity resolution. The source
        // address is passed already parsed, and is only ever recorded.
        let ctx = crate::ctx_from(&state, &identity, None, laysi_ops::Channel::Ui, None);
        assert_eq!(ctx.actor, "可信帳號");
        assert_eq!(ctx.actor_role, "acct");
        assert_eq!(ctx.employee_id, Some(7));
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn session_tokens_have_32_bytes_of_random_material_and_only_hashes_are_stored() {
        let secret = session_token();
        let encoded = secret
            .strip_prefix("lys_s_")
            .expect("session token 應有固定前綴");
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
        assert!(std::str::from_utf8(&unknown_body)
            .expect("JSON 必須是 UTF-8")
            .contains(LOGIN_FAILED_MESSAGE));
    }

    #[test]
    fn cookie_flags_match_the_session_security_contract() {
        let cookie = session_cookie("secret", false);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(!cookie.contains("Secure"));
        assert!(session_cookie("secret", true).contains("; Secure"));
    }

    /// `Secure` follows the request rather than a setting, so the office
    /// server reached over plain HTTP still logs in and the same build behind
    /// TLS still marks the cookie.
    #[test]
    fn secure_follows_the_proxy_rather_than_a_setting() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-proto", HeaderValue::from_str(value).unwrap());
            cookie_secure(&headers)
        };
        assert!(
            !cookie_secure(&HeaderMap::new()),
            "沒有標頭就是明文，不加 Secure"
        );
        assert!(!with("http"));
        assert!(with("https"));
        assert!(with("HTTPS"), "協定名稱不分大小寫");
        // A chain of proxies appends, and the client's own protocol is first.
        assert!(with("https, http"));
        assert!(!with("http, https"));
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
        let stored_hash: String = sqlx::query("SELECT token_hash FROM user_session WHERE id = $1")
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

    async fn signed_in(store: &laysi_store::Store, user_id: i32) -> HeaderMap {
        let secret = session_token();
        store
            .create_session(CreateSession {
                token_hash: &laysi_store::token_hash(&secret),
                user_id,
                expires_at: Utc::now() + Duration::hours(1),
                user_agent: "整合測試",
                ip: "",
            })
            .await
            .expect("應可建立測試 session");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE_NAME}={secret}")).unwrap(),
        );
        headers
    }

    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
    /// changing_the_password_signs_out_every_other_session -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn changing_the_password_signs_out_every_other_session() {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = laysi_store::Store::connect(&database_url, 2)
            .await
            .expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");

        let username = format!("password-test-{}", uuid::Uuid::new_v4());
        let current = "password integration current";
        let password_hash = password::hash_password(current).expect("應可產生測試密碼雜湊");
        let user = store
            .create_user(laysi_store::CreateUser {
                username: &username,
                display_name: &username,
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
        let mine = signed_in(&store, user.id).await;
        let other = signed_in(&store, user.id).await;
        let state = state_with(store);
        let change = |current_password: &str, new_password: &str| {
            change_password(
                State(state.clone()),
                mine.clone(),
                Json(ChangePasswordInput {
                    current_password: Some(current_password.into()),
                    new_password: new_password.into(),
                }),
            )
        };

        let wrong = change("not the current password", "a fresh password").await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        let short = change(current, "short").await;
        assert_eq!(short.status(), StatusCode::BAD_REQUEST);
        assert!(
            authenticate_session(&state, &other).await.is_ok(),
            "變更失敗時不撤銷任何工作階段"
        );

        let changed = change(current, "a fresh password").await;
        assert_eq!(changed.status(), StatusCode::OK);
        assert!(
            matches!(
                authenticate_session(&state, &other).await,
                Err(AuthError::Unauthorized)
            ),
            "另一個瀏覽器的工作階段要結束"
        );
        assert!(
            authenticate_session(&state, &mine).await.is_ok(),
            "執行變更的工作階段維持登入"
        );

        let summary: String = sqlx::query(
            "SELECT result_summary FROM ai_audit_log
              WHERE actor = $1 AND tool_name = 'change_password'
              ORDER BY id DESC LIMIT 1",
        )
        .bind(&username)
        .fetch_one(state.store.pool())
        .await
        .expect("應可讀回變更密碼的稽核紀錄")
        .get("result_summary");
        assert_eq!(summary, "變更密碼 · 登出其他 1 個工作階段");

        sqlx::query("DELETE FROM user_account WHERE id = $1")
            .bind(user.id)
            .execute(state.store.pool())
            .await
            .expect("應可清除測試帳號");
    }

    #[test]
    fn client_ip_takes_the_first_hop_and_drops_anything_that_is_not_an_address() {
        let ip = |name: &'static str, value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(name, HeaderValue::from_static(value));
            client_ip(&headers)
        };

        assert_eq!(
            ip("x-forwarded-for", "203.0.113.7, 10.0.0.1, 10.0.0.2"),
            Some("203.0.113.7".to_string()),
            "第一跳是來源，其後的是代理"
        );
        assert_eq!(
            ip("x-forwarded-for", "  203.0.113.7  "),
            Some("203.0.113.7".to_string())
        );
        assert_eq!(
            ip("x-real-ip", "2001:db8::1"),
            Some("2001:db8::1".to_string())
        );

        // No proxy rewrites the header when the server is reached directly, so
        // the value is the caller's to choose. Anything that is not an address
        // is stored as nothing rather than as text.
        assert_eq!(ip("x-forwarded-for", "'; DROP TABLE ai_audit_log --"), None);
        assert_eq!(ip("x-real-ip", "unknown"), None);
        assert_eq!(client_ip(&HeaderMap::new()), None);
    }

    #[test]
    fn a_malformed_forwarded_for_falls_through_to_the_real_ip_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("unknown"));
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.4"));
        assert_eq!(client_ip(&headers), Some("198.51.100.4".to_string()));
    }
}
