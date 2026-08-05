//! Optional OpenID Connect login.
//!
//! Uses discovery, the authorization code flow, PKCE, state and nonce. JWT
//! and JWKS validation are left entirely to the `openidconnect` crate; this
//! module never parses or verifies a JWT itself.
//!
//! Flows in progress live in an in-process map with a TTL. This deployment
//! runs a single API process for a small number of users, and a permanent
//! table for one-shot state would only leave unnecessary sensitive residue.
//! The map is cleared on restart, so the worst case is a user starting the
//! login again. If the API is ever scaled to several replicas, this
//! short-lived state has to move to shared storage.

use crate::{auth, Shared};
use anyhow::{anyhow, Context};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, TokenResponse,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const FLOW_TTL: Duration = Duration::from_secs(10 * 60);

type OidcClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

struct PendingFlow {
    verifier: PkceCodeVerifier,
    nonce: Nonce,
    created_at: Instant,
}

/// OIDC configuration ready to serve requests once discovery has completed.
pub struct OidcState {
    client: OidcClient,
    http_client: openidconnect::reqwest::Client,
    /// `user_identity.provider` uses the normalised issuer returned by
    /// discovery.
    provider: String,
    pending: Mutex<HashMap<String, PendingFlow>>,
}

impl OidcState {
    /// Disabled when all four environment variables are empty, and also when
    /// only some are set, so a half-configured setup cannot become a fake
    /// login entry point.
    pub async fn from_env() -> anyhow::Result<Option<Self>> {
        let issuer = nonempty_env("OIDC_ISSUER");
        let client_id = nonempty_env("OIDC_CLIENT_ID");
        let client_secret = nonempty_env("OIDC_CLIENT_SECRET");
        let redirect_url = nonempty_env("OIDC_REDIRECT_URL");
        let configured = [&issuer, &client_id, &client_secret, &redirect_url]
            .iter()
            .filter(|value| value.is_some())
            .count();

        if configured == 0 {
            return Ok(None);
        }
        if configured != 4 {
            let missing = [
                ("OIDC_ISSUER", &issuer),
                ("OIDC_CLIENT_ID", &client_id),
                ("OIDC_CLIENT_SECRET", &client_secret),
                ("OIDC_REDIRECT_URL", &redirect_url),
            ]
            .into_iter()
            .filter_map(|(name, value)| value.is_none().then_some(name))
            .collect::<Vec<_>>()
            .join("、");
            tracing::warn!(%missing, "OIDC 設定不完整，OAuth 登入完全停用");
            return Ok(None);
        }

        let issuer = issuer.expect("已確認四個 OIDC 設定都存在");
        let client_id = client_id.expect("已確認四個 OIDC 設定都存在");
        let client_secret = client_secret.expect("已確認四個 OIDC 設定都存在");
        let redirect_url = redirect_url.expect("已確認四個 OIDC 設定都存在");
        let http_client = openidconnect::reqwest::ClientBuilder::new()
            // Discovery and the token endpoint do not follow redirects, so the
            // issuer cannot be used for SSRF.
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .build()
            .context("無法建立 OIDC HTTP client")?;
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer).context("OIDC_ISSUER 不是有效網址")?,
            &http_client,
        )
        .await
        .map_err(|error| anyhow!("OIDC discovery 失敗：{error}"))?;
        let provider = provider_key(metadata.issuer().as_str());
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(client_id),
            Some(ClientSecret::new(client_secret)),
        )
        .set_redirect_uri(
            RedirectUrl::new(redirect_url).context("OIDC_REDIRECT_URL 不是有效網址")?,
        );

        Ok(Some(Self {
            client,
            http_client,
            provider,
            pending: Mutex::new(HashMap::new()),
        }))
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    fn insert_pending(&self, state: &CsrfToken, verifier: PkceCodeVerifier, nonce: Nonce) {
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.retain(|_, flow| flow.created_at.elapsed() <= FLOW_TTL);
        pending.insert(
            state.secret().to_string(),
            PendingFlow {
                verifier,
                nonce,
                created_at: Instant::now(),
            },
        );
    }

    fn take_pending(&self, state: &str) -> Option<PendingFlow> {
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let flow = pending.remove(state)?;
        (flow.created_at.elapsed() <= FLOW_TTL).then_some(flow)
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn provider_key(issuer: &str) -> String {
    issuer.trim_end_matches('/').to_string()
}

fn redirect(location: &str) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    match HeaderValue::from_str(location) {
        Ok(location) => {
            response.headers_mut().insert(header::LOCATION, location);
            response
        }
        Err(error) => {
            tracing::error!(%error, "OIDC 產生了無效的重新導向網址");
            auth::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oidc_redirect_error",
                "目前無法開始外部登入，請稍後再試",
            )
        }
    }
}

fn disabled() -> Response {
    auth::error_response(
        StatusCode::NOT_FOUND,
        "oauth_not_enabled",
        "本站未啟用 OAuth 登入",
    )
}

/// Generate state, nonce and PKCE, store the private verifier server side and
/// redirect to the IdP with a 302.
pub async fn start(State(state): State<Shared>) -> Response {
    let Some(oidc) = state.oidc.as_ref() else {
        return disabled();
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf, nonce) = oidc
        .client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .set_pkce_challenge(challenge)
        .url();
    oidc.insert_pending(&csrf, verifier, nonce);
    redirect(url.as_str())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
enum LinkedUserError {
    Unlinked,
    Store(laysi_store::StoreError),
}

async fn linked_user(
    store: &laysi_store::Store,
    provider: &str,
    subject: &str,
) -> Result<laysi_store::UserAccount, LinkedUserError> {
    store
        .user_by_identity(provider, subject)
        .await
        .map_err(LinkedUserError::Store)?
        .ok_or(LinkedUserError::Unlinked)
}

fn unlinked_identity() -> Response {
    auth::error_response(
        StatusCode::FORBIDDEN,
        "oauth_identity_not_linked",
        "本帳號尚未連結至任何使用者，請先由管理員完成連結",
    )
}

/// Exchange the code on the backend, validate the id_token and nonce, then
/// look up an existing link by issuer and sub only.
pub async fn callback(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(oidc) = state.oidc.as_ref() else {
        return disabled();
    };
    if let Some(error) = query.error {
        tracing::warn!(%error, "IdP 拒絕或取消 OIDC 登入");
        return auth::error_response(
            StatusCode::BAD_REQUEST,
            "oidc_provider_error",
            "外部登入未完成，請重新嘗試",
        );
    }
    let (Some(code), Some(returned_state)) = (query.code, query.state) else {
        return auth::error_response(
            StatusCode::BAD_REQUEST,
            "oidc_callback_incomplete",
            "外部登入 callback 缺少授權碼或 state",
        );
    };
    let Some(pending) = oidc.take_pending(&returned_state) else {
        return auth::error_response(
            StatusCode::BAD_REQUEST,
            "oidc_state_invalid",
            "外部登入 state 無效、已使用或已逾時，請重新開始登入",
        );
    };

    let exchange = match oidc.client.exchange_code(AuthorizationCode::new(code)) {
        Ok(exchange) => exchange.set_pkce_verifier(pending.verifier),
        Err(error) => {
            tracing::error!(%error, "OIDC discovery 未提供可用的 token endpoint");
            return auth::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oidc_token_endpoint_missing",
                "外部登入設定缺少 token endpoint",
            );
        }
    };
    let token_response = match exchange.request_async(&oidc.http_client).await {
        Ok(response) => response,
        Err(_) => {
            // The error body is not logged, in case an upstream response
            // unexpectedly contains the code or a token.
            tracing::warn!("OIDC 授權碼交換失敗");
            return auth::error_response(
                StatusCode::BAD_GATEWAY,
                "oidc_token_exchange_failed",
                "外部登入服務目前無法完成授權，請稍後再試",
            );
        }
    };
    let Some(id_token) = token_response.id_token() else {
        return auth::error_response(
            StatusCode::BAD_GATEWAY,
            "oidc_id_token_missing",
            "外部登入服務未回傳 id_token",
        );
    };
    let verifier = oidc.client.id_token_verifier();
    let claims = match id_token.claims(&verifier, &pending.nonce) {
        Ok(claims) => claims,
        Err(_) => {
            tracing::warn!("OIDC id_token 或 nonce 驗證失敗");
            return auth::error_response(
                StatusCode::UNAUTHORIZED,
                "oidc_id_token_invalid",
                "外部登入憑證驗證失敗，請重新登入",
            );
        }
    };
    if let Some(expected_hash) = claims.access_token_hash() {
        let actual_hash = AccessTokenHash::from_token(
            token_response.access_token(),
            match id_token.signing_alg() {
                Ok(algorithm) => algorithm,
                Err(_) => {
                    return auth::error_response(
                        StatusCode::UNAUTHORIZED,
                        "oidc_id_token_invalid",
                        "外部登入憑證驗證失敗，請重新登入",
                    );
                }
            },
            match id_token.signing_key(&verifier) {
                Ok(key) => key,
                Err(_) => {
                    return auth::error_response(
                        StatusCode::UNAUTHORIZED,
                        "oidc_id_token_invalid",
                        "外部登入憑證驗證失敗，請重新登入",
                    );
                }
            },
        );
        if actual_hash.as_ref() != Ok(expected_hash) {
            return auth::error_response(
                StatusCode::UNAUTHORIZED,
                "oidc_access_token_invalid",
                "外部登入 access token 驗證失敗，請重新登入",
            );
        }
    }

    let subject = claims.subject().as_str();
    let user = match linked_user(&state.store, oidc.provider(), subject).await {
        Ok(user) => user,
        Err(LinkedUserError::Unlinked) => return unlinked_identity(),
        Err(LinkedUserError::Store(error)) => {
            tracing::error!(%error, "依 OIDC subject 查詢本機帳號失敗");
            return auth::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oidc_account_lookup_failed",
                "目前無法確認外部身分連結，請稍後再試",
            );
        }
    };
    if user.status != "active" {
        return auth::error_response(
            StatusCode::FORBIDDEN,
            "account_suspended",
            "這個使用者帳號目前已停用",
        );
    }
    let secret = match auth::issue_session(&state, &user, &headers).await {
        Ok(secret) => secret,
        Err(response) => return response,
    };
    let mut response = redirect("/");
    auth::attach_session_cookie(&mut response, &secret, &state);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_key_is_the_discovered_issuer_not_email() {
        assert_eq!(
            provider_key("https://login.example.test/tenant/"),
            "https://login.example.test/tenant"
        );
    }

    #[tokio::test]
    async fn unlinked_identity_returns_the_required_message() {
        let response = unlinked_identity();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("應可讀取未連結身分回應");
        assert!(
            std::str::from_utf8(&body)
                .expect("JSON 必須是 UTF-8")
                .contains("本帳號尚未連結至任何使用者，請先由管理員完成連結")
        );
    }

    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
    /// oauth_unlinked_subject_never_creates_user_account -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn oauth_unlinked_subject_never_creates_user_account() {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = laysi_store::Store::connect(&database_url, 2)
            .await
            .expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");
        let before = store.user_count().await.expect("應可計算測試前帳號數");
        let provider = format!("https://issuer.invalid/{}", uuid::Uuid::new_v4());
        let subject = uuid::Uuid::new_v4().to_string();

        assert!(matches!(
            linked_user(&store, &provider, &subject).await,
            Err(LinkedUserError::Unlinked)
        ));
        let after = store.user_count().await.expect("應可計算測試後帳號數");
        assert_eq!(before, after, "未連結的 OIDC subject 絕不能自動建立帳號");
    }
}
