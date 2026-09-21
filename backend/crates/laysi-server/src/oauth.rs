//! The OAuth 2.1 authorization server that fronts `/mcp`.
//!
//! A remote AI connector cannot be handed a secret to paste: the person
//! setting it up is on someone else's web page, and that page has no field
//! for one. It instead discovers this endpoint, registers itself, sends the
//! person here to sign in, and exchanges the resulting code for a token.
//!
//! The access token is an ordinary `api_token` row with `kind = 'mcp'`, so
//! [`crate::mcp`] authenticates it with no change and 「安全與營運」 lists and
//! revokes it like any other. A second token store would be a second place
//! to revoke, and one of them would be missed.
//!
//! Four properties hold the flow together:
//!
//! 1. Clients are public and hold no secret. A connector running in someone
//!    else's cloud cannot keep one, so possession is proved with PKCE S256
//!    instead, and `plain` is refused.
//! 2. The authorization code is single use because redeeming it deletes the
//!    row in the same statement.
//! 3. The redirect target must match a registered URI exactly, both when the
//!    code is issued and when it is redeemed.
//! 4. The identity comes from the browser session alone. Nothing in the
//!    request says who the token acts as, and the role is read again when
//!    the token is minted rather than carried in the code.
//!
//! The consent screen is the SPA at `/oauth/authorize`; this module only
//! answers the two `/v1/oauth/authorize` calls behind it. Rendering HTML here
//! would be a second UI with its own copy of the visual language.

use crate::Shared;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// How long an authorization code stays redeemable. RFC 6749 puts the
/// ceiling at ten minutes; the exchange happens within seconds of the
/// redirect, so this is generous already.
const CODE_LIFETIME_MINUTES: i64 = 5;

/// Registrations per minute from one address. Registration is open by
/// design -- that is what lets a connector arrive without an administrator
/// preparing anything -- so the bound on it is a rate, not a credential.
pub const REGISTRATIONS_PER_MINUTE: u32 = 5;

const MAX_REDIRECT_URIS: usize = 8;
const MAX_URI_CHARS: usize = 512;

/// The public origin this installation is reached at, derived from the
/// request rather than configured.
///
/// Same reasoning as the `Secure` flag on the session cookie: a value
/// somebody has to remember to update when the site moves is a value that
/// ends up pointing at the wrong host. The metadata documents and the
/// `WWW-Authenticate` challenge all have to carry absolute URLs, and the
/// only thing that knows the public name is the request that just arrived
/// under it.
///
/// The host is caller-supplied when no proxy rewrites it. A forged one only
/// changes what that same caller is told, so it grants nothing; it is still
/// checked against a conservative character set, because the value goes into
/// a response header.
pub fn public_base(headers: &HeaderMap) -> String {
    let first = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let scheme = match first("x-forwarded-proto") {
        Some(v) if v.eq_ignore_ascii_case("https") => "https",
        _ => "http",
    };
    // Each candidate is checked on its own, so a malformed forwarded header
    // falls through to `Host` rather than discarding both.
    let usable = |h: &&str| {
        h.len() <= 255
            && h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b":.-[]_".contains(&b))
    };
    let host = first("x-forwarded-host")
        .filter(usable)
        .or_else(|| first("host").filter(usable))
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

/// The `WWW-Authenticate` value `/mcp` answers an unauthenticated call with.
/// Without it a client has no way to find the authorization server and
/// reports the endpoint as unreachable rather than as needing a sign-in.
pub fn challenge(headers: &HeaderMap) -> String {
    format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
        public_base(headers)
    )
}

/// RFC 9728. Names the resource and points at the server that guards it.
pub async fn protected_resource(headers: HeaderMap) -> Response {
    let base = public_base(&headers);
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": ["mcp"],
    }))
    .into_response()
}

/// RFC 8414. The authorization endpoint is the SPA route; the other two are
/// REST and reach the `api` service through the same `/v1` prefix as
/// everything else.
pub async fn authorization_server(headers: HeaderMap) -> Response {
    let base = public_base(&headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/v1/oauth/token"),
        "registration_endpoint": format!("{base}/v1/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp"],
    }))
    .into_response()
}

// Dynamic client registration

#[derive(Deserialize)]
pub struct RegisterInput {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: String,
}

/// An OAuth error object (RFC 6749 section 5.2). The `error` code is what a
/// client reads; `error_description` is for whoever is looking at a log.
fn oauth_error(status: StatusCode, code: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": code, "error_description": description })),
    )
        .into_response()
}

/// Accepted redirect targets: HTTPS anywhere, plain HTTP only on the loopback
/// host a desktop client listens on. A fragment is refused because the
/// authorization response is appended as a query and a fragment would move
/// the parameters out of the part the client parses.
fn redirect_uri_usable(uri: &str) -> bool {
    let loopback = uri.starts_with("http://localhost") || uri.starts_with("http://127.0.0.1");
    (uri.starts_with("https://") || loopback)
        && uri.len() <= MAX_URI_CHARS
        && !uri.contains('#')
        && !uri.contains(char::is_whitespace)
}

pub async fn register(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<RegisterInput>,
) -> Response {
    let bucket = crate::auth::client_ip(&headers).unwrap_or_else(|| "unknown".into());
    if !s.oauth_register_rate_limiter.allow(&bucket) {
        return oauth_error(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "registration rate limit reached",
        );
    }
    if input.redirect_uris.is_empty() || input.redirect_uris.len() > MAX_REDIRECT_URIS {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "redirect_uris must hold between one and eight entries",
        );
    }
    if !input.redirect_uris.iter().all(|u| redirect_uri_usable(u)) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "each redirect_uri must be https, or http on the loopback host, and carry no fragment",
        );
    }

    let client_id = format!("lys_c_{}", random_hex());
    let client_name: String = input.client_name.trim().chars().take(120).collect();
    if let Err(error) = s
        .store
        .register_oauth_client(&client_id, &client_name, &input.redirect_uris)
        .await
    {
        tracing::error!(%error, "storing the OAuth client registration failed");
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "the registration could not be stored",
        );
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "client_name": client_name,
            "redirect_uris": input.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "client_id_issued_at": Utc::now().timestamp(),
        })),
    )
        .into_response()
}

// Authorization

/// The authorization request, in the shape the consent screen hands back.
/// Every field is optional here so that a missing one produces a sentence
/// naming it rather than a 422 with no body.
#[derive(Deserialize)]
pub struct AuthorizeInput {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    /// Present only on the POST. `false` is the person pressing 拒絕.
    approve: Option<bool>,
}

#[derive(Serialize)]
struct AuthorizeView {
    client_name: String,
    redirect_uri: String,
    actor: String,
}

/// Everything the request has to satisfy before a person is asked to approve
/// it. Nothing here depends on who is signed in, so both the GET and the POST
/// run it and neither can be reached without it.
// The error is the response to send, which is how an axum handler carries a
// refusal; 128 bytes on a consent path is not worth boxing and unboxing.
#[allow(clippy::result_large_err)]
async fn checked(
    s: &Shared,
    input: &AuthorizeInput,
) -> Result<(laysi_store::OauthClient, String), Response> {
    let bad = |message: &str| {
        Err(crate::auth::error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            message,
        ))
    };

    if input.response_type.as_deref() != Some("code") {
        return bad("這個連接器要求的授權方式本系統不支援，請改用支援授權碼流程的版本。");
    }
    // PKCE is the only thing standing in for a client secret, so a request
    // without it, or with the `plain` method, is refused rather than
    // downgraded.
    if input.code_challenge_method.as_deref() != Some("S256") {
        return bad("這個連接器沒有使用 S256 保護授權碼，請改用支援的版本。");
    }
    let challenge = input.code_challenge.as_deref().unwrap_or("").trim();
    if challenge.is_empty() {
        return bad("這個連接器沒有送出授權碼保護值，請改用支援的版本。");
    }
    let Some(client_id) = input.client_id.as_deref().filter(|v| !v.is_empty()) else {
        return bad("這個連接器沒有表明身分，無法授權。");
    };
    let Some(redirect_uri) = input.redirect_uri.as_deref().filter(|v| !v.is_empty()) else {
        return bad("這個連接器沒有指定接收授權碼的網址，無法授權。");
    };

    let client = match s.store.oauth_client(client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => return bad("這個連接器尚未在本系統註冊，請在連接器端重新加入一次。"),
        Err(error) => {
            tracing::error!(%error, "the OAuth client lookup failed");
            return Err(crate::auth::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oauth_store_error",
                "目前無法處理授權，請稍後再試",
            ));
        }
    };
    // Exact match, not a prefix: a prefix match lets a registered origin
    // carry the code to any path under it.
    if !client.redirect_uris.iter().any(|u| u == redirect_uri) {
        return bad("這個連接器要求把授權碼送到未登記的網址，已經擋下。");
    }
    Ok((client, redirect_uri.to_string()))
}

/// What the consent screen needs to name who is granting what to whom.
pub async fn authorize_view(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(input): Query<AuthorizeInput>,
) -> Response {
    let session = match crate::auth::authenticate_session(&s, &headers).await {
        Ok(session) => session,
        Err(error) => return error.into_response(),
    };
    let (client, redirect_uri) = match checked(&s, &input).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    Json(json!({
        "ok": true,
        "data": AuthorizeView {
            client_name: client.client_name,
            redirect_uri,
            actor: session.session.user.display_name.clone(),
        },
    }))
    .into_response()
}

/// Approve or refuse. Either way the answer is a URL for the browser to
/// follow, because a refusal the connector never hears about leaves it
/// waiting.
pub async fn authorize_decide(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<AuthorizeInput>,
) -> Response {
    let session = match crate::auth::authenticate_session(&s, &headers).await {
        Ok(session) => session,
        Err(error) => return error.into_response(),
    };
    let user = &session.session.user;
    // A first sign-in that still owes a password change is not a session
    // that may hand out a long-lived credential.
    if user.must_change_password {
        return crate::auth::error_response(
            StatusCode::FORBIDDEN,
            "must_change_password",
            "請先變更密碼，再授權外部連接器。",
        );
    }
    let (_, redirect_uri) = match checked(&s, &input).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let state = input.state.as_deref().unwrap_or("");

    if input.approve != Some(true) {
        return Json(json!({
            "ok": true,
            "data": { "redirect_to": redirect_with(&redirect_uri, &[("error", "access_denied")], state) },
        }))
        .into_response();
    }

    let code = format!("lys_a_{}", random_hex());
    let stored = s
        .store
        .create_oauth_code(laysi_store::NewOauthCode {
            code_hash: &laysi_store::token_hash(&code),
            client_id: input.client_id.as_deref().unwrap_or(""),
            user_id: user.id,
            redirect_uri: &redirect_uri,
            code_challenge: input.code_challenge.as_deref().unwrap_or("").trim(),
            expires_at: Utc::now() + Duration::minutes(CODE_LIFETIME_MINUTES),
        })
        .await;
    if let Err(error) = stored {
        tracing::error!(%error, "storing the authorization code failed");
        return crate::auth::error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oauth_store_error",
            "目前無法完成授權，請稍後再試",
        );
    }

    Json(json!({
        "ok": true,
        "data": { "redirect_to": redirect_with(&redirect_uri, &[("code", &code)], state) },
    }))
    .into_response()
}

// Token exchange

#[derive(Deserialize)]
pub struct TokenInput {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
}

pub async fn token(
    State(s): State<Shared>,
    headers: HeaderMap,
    Form(input): Form<TokenInput>,
) -> Response {
    let invalid_grant = |reason: &str| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            &format!("the authorization code is not usable: {reason}"),
        )
    };

    if input.grant_type.as_deref() != Some("authorization_code") {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code is supported",
        );
    }
    let (Some(code), Some(verifier)) = (input.code.as_deref(), input.code_verifier.as_deref())
    else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "code and code_verifier are both required",
        );
    };
    // Checked before the code is redeemed, so a client sending a malformed
    // verifier does not burn a usable grant. It is also load bearing:
    // `from_code_verifier_sha256` asserts this range rather than returning an
    // error, so an unchecked value panics the worker instead of answering.
    if !verifier_well_formed(verifier) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "code_verifier must be 43 to 128 unreserved characters",
        );
    }

    // Redeeming deletes the row, so everything below runs at most once per
    // code even when two exchanges arrive together.
    let grant = match s
        .store
        .take_oauth_code(&laysi_store::token_hash(code))
        .await
    {
        Ok(Some(grant)) => grant,
        Ok(None) => return invalid_grant("unknown or already redeemed"),
        Err(error) => {
            tracing::error!(%error, "redeeming the authorization code failed");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "the authorization code could not be read",
            );
        }
    };

    if grant.expires_at <= Utc::now() {
        return invalid_grant("expired");
    }
    if input.client_id.as_deref() != Some(grant.client_id.as_str()) {
        return invalid_grant("issued to a different client");
    }
    if input.redirect_uri.as_deref() != Some(grant.redirect_uri.as_str()) {
        return invalid_grant("redirect_uri differs from the one it was issued for");
    }
    // The verifier is the secret; the challenge stored beside the code is
    // its SHA-256. Deriving one from the other is what proves this exchange
    // comes from whoever started the flow.
    let derived = openidconnect::PkceCodeChallenge::from_code_verifier_sha256(
        &openidconnect::PkceCodeVerifier::new(verifier.to_string()),
    );
    if derived.as_str() != grant.code_challenge {
        return invalid_grant("code_verifier does not match the challenge");
    }

    let user = match s.store.user_by_id(grant.user_id).await {
        Ok(Some(user)) if user.status == "active" => user,
        Ok(_) => return invalid_grant("the account is no longer active"),
        Err(error) => {
            tracing::error!(%error, "the account lookup during the token exchange failed");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "the account could not be read",
            );
        }
    };

    let client_name = match s.store.oauth_client(&grant.client_id).await {
        Ok(Some(client)) if !client.client_name.is_empty() => client.client_name,
        _ => "外部連接器".to_string(),
    };
    // The role is read here rather than carried in the code: a role changed
    // between the consent and the exchange is the one that counts.
    let issued = s
        .store
        .issue_api_token(laysi_store::IssueApiToken {
            name: &client_name,
            kind: "mcp",
            scope: "與本人相同",
            actor: &user.display_name,
            actor_role: &user.role_code,
            created_by: &user.display_name,
            rotate_days: None,
            expiry_note: "未設定輪替週期",
            owner_employee_id: user.employee_id,
            // The account, not only the employee: the bootstrap `admin` has
            // no roster row, and without this its connector would outlive it.
            owner_user_id: Some(user.id),
        })
        .await;
    let issued = match issued {
        Ok(issued) => issued,
        Err(error) => {
            tracing::error!(%error, "issuing the MCP token for the OAuth grant failed");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "the token could not be issued",
            );
        }
    };

    let summary = format!(
        "{} 授權外部連接器「{}」取得 MCP Token · {}（{}…）· 權限與本人相同",
        user.display_name, client_name, issued.token.name, issued.token.prefix
    );
    if let Err(error) = s
        .store
        .write_audit(
            &user.display_name,
            &user.role_code,
            "oauth_token",
            "authentication",
            None,
            "ui",
            &json!({ "client_id": grant.client_id, "token_id": issued.token.id }),
            &summary,
            "ok",
            None,
            crate::auth::client_ip(&headers).as_deref(),
        )
        .await
    {
        tracing::error!(%error, "the audit write for the OAuth token issue failed");
    }

    // No `expires_in` and no refresh token: the token is revoked on the
    // 「安全與營運」 screen like every other one, and a lifetime announced
    // here would be a second answer to a question that screen already
    // answers.
    (
        StatusCode::OK,
        [("cache-control", "no-store")],
        Json(json!({
            "access_token": issued.secret,
            "token_type": "Bearer",
            "scope": "mcp",
        })),
    )
        .into_response()
}

// Helpers

/// The shape RFC 7636 gives a `code_verifier`: 43 to 128 characters from the
/// unreserved set.
fn verifier_well_formed(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

fn random_hex() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).expect("OS entropy is unavailable");
    hex::encode(bytes)
}

/// Append the authorization response to a redirect target. `state` is echoed
/// only when the client sent one, which is what the spec requires.
fn redirect_with(uri: &str, pairs: &[(&str, &str)], state: &str) -> String {
    let separator = if uri.contains('?') { '&' } else { '?' };
    let mut out = format!("{uri}{separator}");
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&format!("{key}={}", form_encoded(value)));
    }
    if !state.is_empty() {
        out.push_str(&format!("&state={}", form_encoded(state)));
    }
    out
}

/// Percent-encode everything outside the unreserved set. The value goes into
/// a query string that the client parses, and `state` is whatever the client
/// chose to put there.
fn form_encoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn the_public_base_follows_the_proxy_headers() {
        assert_eq!(
            public_base(&headers(&[
                ("host", "one.laysi.io"),
                ("x-forwarded-proto", "https")
            ])),
            "https://one.laysi.io"
        );
        // Plain HTTP on the office LAN, the same case the session cookie's
        // Secure flag has to get right.
        assert_eq!(
            public_base(&headers(&[("host", "192.168.1.10:8088")])),
            "http://192.168.1.10:8088"
        );
        // A header that could break out into a second header line never
        // reaches the response.
        assert_eq!(
            public_base(&headers(&[
                ("host", "one.laysi.io"),
                ("x-forwarded-host", "a b")
            ])),
            "http://one.laysi.io"
        );
    }

    #[test]
    fn only_https_and_the_loopback_host_are_accepted_as_redirects() {
        assert!(redirect_uri_usable(
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(redirect_uri_usable("http://localhost:33418/callback"));
        assert!(!redirect_uri_usable("http://example.com/callback"));
        // A fragment would move the response out of the query the client
        // reads.
        assert!(!redirect_uri_usable("https://example.com/cb#frag"));
        assert!(!redirect_uri_usable("https://example.com/c b"));
    }

    #[test]
    fn the_redirect_carries_the_state_back_verbatim() {
        assert_eq!(
            redirect_with("https://claude.ai/cb", &[("code", "lys_a_1")], "a b&c=d"),
            "https://claude.ai/cb?code=lys_a_1&state=a%20b%26c%3Dd"
        );
        // An existing query is kept, and a refusal is reported rather than
        // left as silence.
        assert_eq!(
            redirect_with(
                "https://claude.ai/cb?x=1",
                &[("error", "access_denied")],
                ""
            ),
            "https://claude.ai/cb?x=1&error=access_denied"
        );
    }

    #[test]
    fn the_challenge_names_the_metadata_document() {
        assert_eq!(
            challenge(&headers(&[
                ("host", "one.laysi.io"),
                ("x-forwarded-proto", "https")
            ])),
            "Bearer resource_metadata=\"https://one.laysi.io/.well-known/oauth-protected-resource\""
        );
    }

    /// `from_code_verifier_sha256` asserts the length rather than returning
    /// an error, so anything reaching it unchecked takes the worker thread
    /// down and the caller sees an empty reply.
    #[test]
    fn a_verifier_outside_the_rfc_shape_never_reaches_the_hash() {
        assert!(!verifier_well_formed("too-short"));
        assert!(!verifier_well_formed(&"a".repeat(129)));
        assert!(!verifier_well_formed(&format!("{}/", "a".repeat(42))));
        assert!(verifier_well_formed(&"a".repeat(43)));
        assert!(verifier_well_formed(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        ));
    }

    /// The verifier is what the client keeps; the challenge is what this
    /// server stored. If these two ever stop agreeing, every exchange fails
    /// with a message about PKCE and nothing says which side moved.
    #[test]
    fn the_stored_challenge_is_the_sha256_of_the_verifier() {
        // The example pair from RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let derived = openidconnect::PkceCodeChallenge::from_code_verifier_sha256(
            &openidconnect::PkceCodeVerifier::new(verifier.to_string()),
        );
        assert_eq!(
            derived.as_str(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
