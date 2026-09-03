//! Optional OpenID Connect login.
//!
//! Uses discovery, the authorization code flow, PKCE, state and nonce. JWT
//! and JWKS validation are left entirely to the `openidconnect` crate; this
//! module never parses or verifies a JWT itself.
//!
//! The settings live in `oidc_setting` and are edited on the security
//! screen. They are not environment variables: the person who owns the
//! installation is the one who has the IdP's client id, and asking them to
//! edit a container's environment and restart it puts their own sign-in out
//! of their reach. A saved change takes effect on the next request, because
//! the client is rebuilt whenever the row's `updated_at` moves.
//!
//! Two of those settings decide who gets in. `allowed_domain` requires the
//! `hd` claim, which a directory-managed Google account carries and a
//! personal one does not, so the boundary is the organisation rather than
//! the shape of an address: a personal account can hold a verified address
//! at the company's domain, and it would pass an address check while nobody
//! can close it when the person leaves.
//!
//! `provision_role` then lets a first sign-in from inside that domain
//! create its own account with that role. It is refused without a domain,
//! because provisioning with no boundary would let any account anywhere
//! claim a login here.
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
use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreErrorResponseType,
    CoreGenderClaim, CoreJsonWebKey, CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm,
    CoreProviderMetadata, CoreRevocableToken, CoreRevocationErrorResponse,
    CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::{
    AccessTokenHash, AdditionalClaims, AuthorizationCode, Client, ClientId, ClientSecret,
    CsrfToken, EmptyExtraTokenFields, EndpointMaybeSet, EndpointNotSet, EndpointSet, IdTokenFields,
    IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    StandardErrorResponse, StandardTokenResponse, TokenResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FLOW_TTL: Duration = Duration::from_secs(10 * 60);

/// The hosted domain an account belongs to.
///
/// Not part of the OpenID Connect standard claims, so it needs its own type
/// for the crate to hand it back after verification. Reading it off the
/// verified id_token keeps every claim this module trusts inside one
/// signature check.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct HostedDomainClaims {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hd: Option<String>,
}

impl AdditionalClaims for HostedDomainClaims {}

type HdIdTokenFields = IdTokenFields<
    HostedDomainClaims,
    EmptyExtraTokenFields,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;

type HdTokenResponse = StandardTokenResponse<HdIdTokenFields, CoreTokenType>;

type OidcClient = Client<
    HostedDomainClaims,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    HdTokenResponse,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
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
    /// Where to land after the round trip, when it is not the front page.
    /// Held here rather than in a query parameter on the callback so that the
    /// target cannot be swapped between leaving and coming back.
    return_to: Option<String>,
}

/// The one address a sign-in is allowed to come back to other than `/`.
///
/// The consent screen keeps the whole authorization request in its query
/// string, so a sign-in that lands on the front page has destroyed the
/// request the connector was waiting on. Accepting any address instead would
/// make this an open redirect; the prefix is fixed and starts with a single
/// slash, so the target is always this site's consent screen.
fn return_target(asked: Option<&str>) -> Option<String> {
    let asked = asked?.trim();
    (asked.starts_with("/oauth/authorize?")
        && asked.len() <= 2048
        && !asked.contains(|c: char| c.is_control()))
    .then(|| asked.to_string())
}

/// The role a first sign-in creates its account with, resolved at startup so
/// a code that names no role fails the boot rather than the login.
struct Provisioning {
    role_code: String,
    role_name: String,
}

/// OIDC configuration ready to serve requests once discovery has completed.
pub struct OidcState {
    client: OidcClient,
    http_client: openidconnect::reqwest::Client,
    /// `user_identity.provider` uses the normalised issuer returned by
    /// discovery.
    provider: String,
    /// Required `hd` claim, lowercase and without the `@`.
    allowed_hd: Option<String>,
    provision: Option<Provisioning>,
    password_login_disabled: bool,
    pending: Mutex<HashMap<String, PendingFlow>>,
}

impl OidcState {
    /// Build a client from the stored settings, or `None` when external
    /// sign-in is switched off. A row that is on but incomplete is an error,
    /// not a quiet `None`: the settings screen has to be able to say why the
    /// button is missing.
    pub async fn from_setting(
        setting: &laysi_store::OidcSetting,
        store: &laysi_store::Store,
    ) -> anyhow::Result<Option<Self>> {
        if !setting.enabled {
            return Ok(None);
        }
        let missing = [
            ("供應商網址", setting.issuer.trim().is_empty()),
            ("用戶端 ID", setting.client_id.trim().is_empty()),
            ("用戶端密鑰", setting.client_secret.trim().is_empty()),
            ("轉址網址", setting.redirect_url.trim().is_empty()),
        ]
        .into_iter()
        .filter_map(|(label, missing)| missing.then_some(label))
        .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(anyhow!("外部登入設定還缺 {}", missing.join("、")));
        }

        let issuer = setting.issuer.trim().to_string();
        let client_id = setting.client_id.trim().to_string();
        let client_secret = setting.client_secret.clone();
        let redirect_url = setting.redirect_url.trim().to_string();
        let allowed_hd =
            Some(normalize_domain(&setting.allowed_domain)).filter(|value| !value.is_empty());
        let provision =
            resolve_provisioning(store, &setting.provision_role, allowed_hd.as_deref()).await?;

        let http_client = openidconnect::reqwest::ClientBuilder::new()
            // Discovery and the token endpoint do not follow redirects, so the
            // issuer cannot be used for SSRF.
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .build()
            .context("無法建立 OIDC HTTP client")?;
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer).context("供應商網址不是有效的網址")?,
            &http_client,
        )
        .await
        .map_err(|error| {
            // The message reaches the settings screen, so it says what to fix
            // rather than what the library returned; the detail goes to the
            // log for whoever is reading it.
            tracing::warn!(%error, "external login discovery failed");
            anyhow!("無法讀取這個供應商的登入設定，請確認供應商網址")
        })?;
        let provider = provider_key(metadata.issuer().as_str());
        let client = Client::from_provider_metadata(
            metadata,
            ClientId::new(client_id),
            Some(ClientSecret::new(client_secret)),
        )
        .set_redirect_uri(RedirectUrl::new(redirect_url).context("轉址網址不是有效的網址")?);

        Ok(Some(Self {
            client,
            http_client,
            provider,
            allowed_hd,
            provision,
            password_login_disabled: setting.password_login_disabled,
            pending: Mutex::new(HashMap::new()),
        }))
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Screen label for the sign-in button.
    pub fn label(&self) -> String {
        laysi_store::provider_label(&self.provider)
    }

    /// Whether an account inside the allowed domain is created on first
    /// sign-in. The login screen says which of the two it is, because the
    /// answer decides whether waiting for an administrator is the next step.
    pub fn provisions_accounts(&self) -> bool {
        self.provision.is_some()
    }

    pub fn allowed_hd(&self) -> Option<&str> {
        self.allowed_hd.as_deref()
    }

    /// Whether a working external sign-in currently replaces the password
    /// form for everyone except the system administrator role. Only ever
    /// true here when this client actually built successfully, which is what
    /// gives the setting its self-healing property: a broken IdP makes
    /// `current()` return `None` before this is ever read, so password login
    /// comes back for everyone the moment external sign-in stops working.
    pub fn password_login_disabled(&self) -> bool {
        self.password_login_disabled
    }

    fn insert_pending(
        &self,
        state: &CsrfToken,
        verifier: PkceCodeVerifier,
        nonce: Nonce,
        return_to: Option<String>,
    ) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.retain(|_, flow| flow.created_at.elapsed() <= FLOW_TTL);
        pending.insert(
            state.secret().to_string(),
            PendingFlow {
                verifier,
                nonce,
                created_at: Instant::now(),
                return_to,
            },
        );
    }

    fn take_pending(&self, state: &str) -> Option<PendingFlow> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let flow = pending.remove(state)?;
        (flow.created_at.elapsed() <= FLOW_TTL).then_some(flow)
    }
}

/// Refuse when the client is built rather than at the first sign-in: an
/// unknown role code or a missing domain would otherwise look like a working
/// configuration until somebody tried to use it.
async fn resolve_provisioning(
    store: &laysi_store::Store,
    role_code: &str,
    allowed_hd: Option<&str>,
) -> anyhow::Result<Option<Provisioning>> {
    let role_code = role_code.trim();
    if role_code.is_empty() {
        return Ok(None);
    }
    if allowed_hd.is_none() {
        return Err(anyhow!(
            "要自動建立帳號請先填公司電子郵件網域，否則任何外部帳號都能建立登入"
        ));
    }
    let roles = store
        .roles()
        .await
        .context("讀取角色表以驗證自動建帳號角色失敗")?;
    let Some(role) = roles.iter().find(|role| role.code == role_code) else {
        let known: Vec<&str> = roles.iter().map(|role| role.code.as_str()).collect();
        return Err(anyhow!(
            "自動建立帳號指定的角色「{role_code}」不存在；可用的角色是 {}",
            known.join("、")
        ));
    };
    tracing::info!(
        role = %role.code,
        domain = allowed_hd.unwrap_or_default(),
        "網域內的首次登入會自動建立帳號"
    );
    Ok(Some(Provisioning {
        role_code: role.code.clone(),
        role_name: role.name.clone(),
    }))
}

fn provider_key(issuer: &str) -> String {
    issuer.trim_end_matches('/').to_string()
}

/// Domains compare case-insensitively, and a leading `@` is what people type.
fn normalize_domain(value: &str) -> String {
    value.trim().trim_start_matches('@').to_ascii_lowercase()
}

/// A missing `hd` never passes: a personal account simply does not carry the
/// claim, which is the whole difference this check is looking for.
fn hd_matches(required: &str, claim: Option<&str>) -> bool {
    claim.map(normalize_domain).as_deref() == Some(required)
}

/// An address stands in for the person only when the IdP says it verified
/// it. Provisioning on an unverified address would let anyone who can type
/// one into a profile claim somebody else's login here.
fn usable_email(email: &str, verified: bool) -> Option<&str> {
    (verified && !email.is_empty()).then_some(email)
}

/// Same rule as `create_user_account`'s: the address doubles as the login
/// name, so it is the login name and there is nothing left to derive.
fn valid_login_email(value: &str) -> bool {
    let count = value.chars().count();
    if !(3..=254).contains(&count) || value.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// The client the settings currently describe, and the version of the
/// settings it was built from.
#[derive(Default)]
pub struct OidcCache {
    fingerprint: Option<chrono::DateTime<chrono::Utc>>,
    state: Option<Arc<OidcState>>,
}

/// The client for the stored settings, rebuilt when they have changed.
///
/// Discovery is a network call, so the result is kept against the row's
/// `updated_at`; saving moves that value and the next request pays for one
/// rebuild. The outcome is written back to the row because the person who
/// saved the settings has to see whether they work, and the settings screen
/// cannot run discovery itself.
pub async fn current(state: &Shared) -> Option<Arc<OidcState>> {
    let setting = match state.store.oidc_setting().await {
        Ok(setting) => setting,
        Err(error) => {
            tracing::error!(%error, "reading the external login settings failed");
            return None;
        }
    };
    {
        let cache = state.oidc.read().await;
        if cache.fingerprint == Some(setting.updated_at) {
            return cache.state.clone();
        }
    }

    let mut cache = state.oidc.write().await;
    if cache.fingerprint == Some(setting.updated_at) {
        return cache.state.clone();
    }
    let built = OidcState::from_setting(&setting, &state.store).await;
    let failure = match &built {
        Ok(_) => String::new(),
        Err(error) => format!("{error}"),
    };
    // Recording is skipped when the answer has not moved, so this never
    // touches `updated_at` and cannot chase its own tail.
    let stale = setting.verify_error != failure
        || (failure.is_empty() && setting.enabled && setting.verified_at.is_none());
    if stale {
        if let Err(error) = state.store.record_oidc_verification(&failure).await {
            tracing::error!(%error, "writing the external login verification result failed");
        }
    }
    let resolved = match built {
        Ok(built) => built.map(Arc::new),
        Err(error) => {
            tracing::warn!(%error, "the external login settings are currently unusable");
            None
        }
    };
    cache.fingerprint = Some(setting.updated_at);
    cache.state = resolved.clone();
    resolved
}

/// What the login screen needs to know about external sign-in: whether it is
/// on, what to call it, and whether a first sign-in from the domain creates
/// its own account or has to wait for an administrator.
pub async fn describe(state: &Shared) -> Value {
    match current(state).await {
        None => json!({ "enabled": false }),
        Some(oidc) => json!({
            "enabled": true,
            "label": oidc.label(),
            "domain": oidc.allowed_hd(),
            "creates_accounts": oidc.provisions_accounts(),
            "password_login_disabled": oidc.password_login_disabled(),
        }),
    }
}

fn redirect(location: &str) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    match HeaderValue::from_str(location) {
        Ok(location) => {
            response.headers_mut().insert(header::LOCATION, location);
            response
        }
        Err(error) => {
            tracing::error!(%error, "OIDC produced an invalid redirect URL");
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

#[derive(Deserialize)]
pub struct StartQuery {
    /// Where to come back to, when the sign-in began somewhere the front page
    /// would throw away. Checked by [`return_target`], not taken as given.
    return_to: Option<String>,
}

/// Generate state, nonce and PKCE, store the private verifier server side and
/// redirect to the IdP with a 302.
pub async fn start(State(state): State<Shared>, Query(query): Query<StartQuery>) -> Response {
    let Some(oidc) = current(&state).await else {
        return disabled();
    };
    let return_to = return_target(query.return_to.as_deref());
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = oidc
        .client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        // The address identifies the person and the name is what the account
        // roster shows; neither is a login key.
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .add_prompt(CoreAuthPrompt::SelectAccount)
        .set_pkce_challenge(challenge);
    if let Some(domain) = oidc.allowed_hd.as_deref() {
        // Narrows the account chooser to the right directory. The claim is
        // checked again on the way back; this only saves a wasted round trip.
        request = request.add_extra_param("hd", domain);
    }
    let (url, csrf, nonce) = request.url();
    oidc.insert_pending(&csrf, verifier, nonce, return_to);
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

fn suspended_account() -> Response {
    auth::error_response(
        StatusCode::FORBIDDEN,
        "account_suspended",
        "這個使用者帳號目前已停用",
    )
}

fn lookup_failed() -> Response {
    auth::error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "oidc_account_lookup_failed",
        "目前無法確認外部身分連結，請稍後再試",
    )
}

/// The verified facts a sign-in carries about the person.
struct ExternalIdentity<'a> {
    subject: &'a str,
    email: String,
    email_verified: bool,
    display_name: Option<String>,
}

/// Resolve the local account for a verified external identity.
///
/// The subject is the only login key. An address is used once, and only when
/// provisioning is on: to recognise an account an administrator already
/// created for the same person, so a first sign-in joins that account rather
/// than building a second one beside it. The match is not limited to active
/// accounts on purpose — a suspended account has to be found and refused,
/// not quietly replaced by a fresh one.
// The error is the response to send; see `auth::issue_session`.
#[allow(clippy::result_large_err)]
async fn resolve_account(
    state: &Shared,
    oidc: &OidcState,
    identity: &ExternalIdentity<'_>,
) -> Result<laysi_store::UserAccount, Response> {
    match linked_user(&state.store, oidc.provider(), identity.subject).await {
        Ok(user) => return Ok(user),
        Err(LinkedUserError::Unlinked) => {}
        Err(LinkedUserError::Store(error)) => {
            tracing::error!(%error, "looking up the local account by OIDC subject failed");
            return Err(lookup_failed());
        }
    }

    let Some(provision) = oidc.provision.as_ref() else {
        return Err(unlinked_identity());
    };
    let Some(email) = usable_email(&identity.email, identity.email_verified) else {
        return Err(auth::error_response(
            StatusCode::FORBIDDEN,
            "oidc_email_not_verified",
            "這個外部帳號沒有已驗證的電子郵件，無法建立登入帳號",
        ));
    };

    let existing = state.store.users_by_email(email).await.map_err(|error| {
        tracing::error!(%error, "looking up the local account by email failed");
        lookup_failed()
    })?;
    if existing.len() > 1 {
        return Err(auth::error_response(
            StatusCode::CONFLICT,
            "oidc_email_ambiguous",
            "有多個帳號使用這個電子郵件，請由系統管理員整理後再登入",
        ));
    }
    if let Some(user) = existing.into_iter().next() {
        if user.status != "active" {
            return Err(suspended_account());
        }
        state
            .store
            .link_identity(user.id, oidc.provider(), identity.subject, email)
            .await
            .map_err(|error| {
                tracing::error!(%error, "linking the external identity to an existing account failed");
                lookup_failed()
            })?;
        tracing::info!(username = %user.username, "external login linked to an existing account");
        return Ok(user);
    }

    if !valid_login_email(email) {
        return Err(auth::error_response(
            StatusCode::FORBIDDEN,
            "oidc_username_unusable",
            "這個電子郵件不能當作登入帳號，請由系統管理員建立帳號",
        ));
    }
    let username = email;
    // Cannot actually collide today: reaching here already means
    // `users_by_email(email)` found nothing, and the login name an account
    // gets at creation is always its email. Kept as the same defence
    // `create_user_account` has, in case a later edit ever points two
    // accounts' contact addresses at the same string.
    let taken = state
        .store
        .user_by_username(username)
        .await
        .map_err(|error| {
            tracing::error!(%error, "checking whether the username is taken failed");
            lookup_failed()
        })?
        .is_some();
    if taken {
        return Err(auth::error_response(
            StatusCode::CONFLICT,
            "oidc_username_taken",
            "登入名稱已被其他帳號使用，請由系統管理員建立帳號",
        ));
    }

    let display_name = identity
        .display_name
        .clone()
        .unwrap_or_else(|| username.to_string());
    let provisioned = state
        .store
        .provision_external_user(laysi_store::ProvisionExternalUser {
            username,
            display_name: &display_name,
            email,
            role_code: &provision.role_code,
            provider: oidc.provider(),
            subject: identity.subject,
            hired_on: crate::today(),
            created_by: "外部登入",
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "creating an account from the external identity failed");
            auth::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oidc_provision_failed",
                "目前無法建立登入帳號，請稍後再試",
            )
        })?;
    tracing::info!(
        username = %provisioned.user.username,
        employee_no = %provisioned.employee_no,
        role = %provision.role_name,
        "外部登入建立了新帳號與員工主檔"
    );
    Ok(provisioned.user)
}

/// Exchange the code on the backend, validate the id_token and nonce, then
/// resolve the local account from the issuer and `sub`.
pub async fn callback(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(oidc) = current(&state).await else {
        return disabled();
    };
    if let Some(error) = query.error {
        tracing::warn!(%error, "the IdP refused or cancelled the OIDC login");
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
            tracing::error!(%error, "the external login provider offers no usable token endpoint");
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
            tracing::warn!("OIDC authorization code exchange failed");
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
            tracing::warn!("OIDC id_token or nonce verification failed");
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

    if let Some(required) = oidc.allowed_hd.as_deref() {
        if !hd_matches(required, claims.additional_claims().hd.as_deref()) {
            tracing::warn!("the external account is outside the company hosted domain");
            return auth::error_response(
                StatusCode::FORBIDDEN,
                "oidc_domain_not_allowed",
                &format!("這個帳號不屬於 {required}，請改用公司帳號登入"),
            );
        }
    }

    let identity = ExternalIdentity {
        subject: claims.subject().as_str(),
        email: claims
            .email()
            .map(|email| email.trim().to_ascii_lowercase())
            .unwrap_or_default(),
        email_verified: claims.email_verified().unwrap_or(false),
        display_name: claims
            .name()
            .and_then(|name| name.get(None))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
    };
    let user = match resolve_account(&state, &oidc, &identity).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if user.status != "active" {
        return suspended_account();
    }
    // An account an administrator created carries a generated password and the
    // flag that forces changing it. Someone arriving through the IdP was never
    // given that password and needs none, so leaving the flag set puts a
    // password form in front of a login that just succeeded. Only the flag is
    // cleared: the hash still answers `reset_user_password` if they ever want
    // a password after all.
    if user.must_change_password {
        if let Err(error) = state.store.clear_must_change_password(user.id).await {
            tracing::error!(%error, "clearing the forced password change flag after external login failed");
        }
    }
    let secret = match auth::issue_session(&state, &user, &headers).await {
        Ok(secret) => secret,
        Err(response) => return response,
    };
    let mut response = redirect(pending.return_to.as_deref().unwrap_or("/"));
    auth::attach_session_cookie(&mut response, &secret, &headers);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The target survives a round trip through the IdP, so anything it
    /// accepts is somewhere an attacker can land a signed-in browser.
    #[test]
    fn only_the_consent_screen_is_a_return_target() {
        assert_eq!(
            return_target(Some("/oauth/authorize?client_id=lys_c_1&state=x")).as_deref(),
            Some("/oauth/authorize?client_id=lys_c_1&state=x")
        );
        assert_eq!(return_target(None), None);
        assert_eq!(return_target(Some("/")), None);
        // A protocol-relative address is another site, and an absolute one
        // says so outright.
        assert_eq!(return_target(Some("//evil.example/")), None);
        assert_eq!(return_target(Some("https://evil.example/")), None);
        // Without the query there is no authorization request to resume, so
        // this is only the prefix being borrowed.
        assert_eq!(return_target(Some("/oauth/authorize")), None);
        assert_eq!(return_target(Some("/oauth/authorize?a=1\r\nX: y")), None);
    }

    #[test]
    fn provider_key_is_the_discovered_issuer_not_email() {
        assert_eq!(
            provider_key("https://login.example.test/tenant/"),
            "https://login.example.test/tenant"
        );
    }

    #[test]
    fn a_domain_is_compared_without_case_or_at_sign() {
        assert_eq!(normalize_domain(" @Example.TEST "), "example.test");
        assert!(hd_matches("example.test", Some("Example.test")));
    }

    #[test]
    fn a_missing_hosted_domain_never_passes() {
        // A personal account carries no `hd` at all, and that is exactly the
        // account this check exists to keep out.
        assert!(!hd_matches("example.test", None));
        assert!(!hd_matches("example.test", Some("other.test")));
    }

    #[test]
    fn an_unverified_address_never_creates_an_account() {
        assert_eq!(
            usable_email("chen.mei@example.test", true),
            Some("chen.mei@example.test")
        );
        assert_eq!(usable_email("chen.mei@example.test", false), None);
        assert_eq!(usable_email("", true), None);
    }

    #[test]
    fn the_login_name_is_the_address_itself() {
        assert!(valid_login_email("chen.mei@example.test"));
        assert!(!valid_login_email("not-an-address"));
        assert!(!valid_login_email(""));
        assert!(!valid_login_email("a@b"));
    }

    #[tokio::test]
    async fn unlinked_identity_returns_the_required_message() {
        let response = unlinked_identity();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("應可讀取未連結身分回應");
        assert!(std::str::from_utf8(&body)
            .expect("JSON 必須是 UTF-8")
            .contains("本帳號尚未連結至任何使用者，請先由管理員完成連結"));
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
