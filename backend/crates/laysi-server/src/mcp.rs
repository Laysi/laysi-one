//! The MCP (Model Context Protocol) interface.
//!
//! Streamable HTTP: a single `POST /mcp` endpoint carrying JSON-RPC 2.0. The
//! tool list is not hand-written; it is generated directly from
//! [`laysi_ops::Registry`], so a new operation automatically becomes an MCP
//! tool and cannot be missed.
//!
//! Authentication uses `Authorization: Bearer <secret>`. The server hashes
//! it with BLAKE3 and looks up `api_token`; the identity comes solely from
//! that row's `actor` and `actor_role`, never from anything the request body
//! or headers claim, or the owner-only risk tier would mean nothing.
//!
//! With the endpoint exposed publicly there are three layers between
//! authentication and JSON-RPC:
//!
//! 1. Origin validation (required by the MCP spec, guarding against DNS
//!    rebinding): see [`origin_allowed`].
//! 2. Token expiry: with `rotate_days` set, the token stops working after
//!    `created_at + rotate_days`. Expiry and revocation write different audit
//!    summaries but return the same 401 externally, see [`denial_reason`], so
//!    a caller cannot infer which one it was.
//! 3. Per-token rate limiting: see [`crate::ratelimit`], counted in process
//!    with no redis dependency.

use crate::{Shared, MCP_PROTOCOL_VERSION};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

/// JSON-RPC error codes.
mod code {
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// -32000 to -32099 is the range the spec reserves for implementation
    /// defined server errors.
    pub const RATE_LIMITED: i32 = -32029;
    /// The server could not answer, as opposed to answering "nothing".
    pub const INTERNAL: i32 = -32603;
}

fn ok(id: Value, result: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn err(id: Value, code: i32, message: impl Into<String>) -> Response {
    Json(json!({
        "jsonrpc": "2.0", "id": id,
        "error": { "code": code, "message": message.into() }
    }))
    .into_response()
}

#[derive(Debug)]
enum McpAuthError {
    Unauthorized,
    Store(laysi_store::StoreError),
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let (scheme, secret) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !secret.is_empty()).then_some(secret)
}

/// The streamable HTTP spec requires validating `Origin` to guard against
/// DNS rebinding. Non-browser clients (normal AI connectors) do not send the
/// header, so the rule is: if present it must be in the allow list, if
/// absent it passes. The list is empty by default, which accepts only calls
/// that send no Origin.
fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(origin) => allowed.iter().any(|a| a == origin),
    }
}

fn token_identity(token: &laysi_store::ApiToken) -> crate::auth::VerifiedIdentity {
    crate::auth::VerifiedIdentity {
        actor: token.actor.clone(),
        actor_role: token.actor_role.clone(),
        // A token is not an account, so there is no login name to act as.
        username: None,
        // Only from the column an employee set on their own token through
        // `issue_my_api_token`. A token the owner issued on the
        // installation's behalf leaves it NULL and stays unlinked: the
        // employee is still never guessed from `actor`, which is a display
        // name two people can share.
        employee_id: token.owner_employee_id,
    }
}

/// One successful MCP authentication: the identity plus the token id, which
/// the rate limiter uses as its bucket key.
struct AuthenticatedMcp {
    identity: crate::auth::VerifiedIdentity,
    token_id: i32,
}

/// Why a token is unusable. Revocation and expiry both stop it, but the
/// reason has to be distinguishable to write a meaningful audit summary. It
/// must never leak into the HTTP response, which is always the same 401.
enum Denial {
    Revoked,
    Expired,
}

fn denial_reason(token: &laysi_store::ApiToken, at: DateTime<Utc>) -> Option<Denial> {
    if token.revoked_at.is_some() {
        Some(Denial::Revoked)
    } else if token.is_expired(at) {
        Some(Denial::Expired)
    } else {
        None
    }
}

/// Rejections whose reason is worth recording in the audit (revoked,
/// expired). A missing or mismatched hash is a random probe with no
/// trustworthy identity to attribute, and is not recorded.
async fn audit_denied_token(
    store: &laysi_store::Store,
    token: &laysi_store::ApiToken,
    denial: Denial,
) {
    let reason = match denial {
        Denial::Revoked => "已撤銷",
        Denial::Expired => "已過期",
    };
    let summary = format!(
        "MCP 認證被拒絕 · Token「{}」（{}…）{reason}",
        token.name, token.prefix
    );
    if let Err(error) = store
        .write_audit(
            &token.actor,
            &token.actor_role,
            "mcp_auth",
            "authentication",
            None,
            "mcp",
            &json!({ "token_id": token.id }),
            &summary,
            "blocked",
            None,
        )
        .await
    {
        tracing::error!(%error, token_id = token.id, "MCP 認證拒絕的稽核寫入失敗");
    }
}

async fn authenticate_mcp(
    store: &laysi_store::Store,
    headers: &HeaderMap,
) -> Result<AuthenticatedMcp, McpAuthError> {
    let secret = bearer(headers).ok_or(McpAuthError::Unauthorized)?;
    let token_hash = laysi_store::token_hash(secret);
    let token = store
        .token_by_hash(&token_hash)
        .await
        .map_err(McpAuthError::Store)?
        .filter(|token| token.kind == "mcp")
        .ok_or(McpAuthError::Unauthorized)?;

    if let Some(denial) = denial_reason(&token, Utc::now()) {
        audit_denied_token(store, &token, denial).await;
        return Err(McpAuthError::Unauthorized);
    }

    // If the token is revoked immediately after being read back, the update
    // returns false and this call must be rejected as well.
    if !store
        .touch_api_token_used(token.id)
        .await
        .map_err(McpAuthError::Store)?
    {
        return Err(McpAuthError::Unauthorized);
    }
    Ok(AuthenticatedMcp {
        identity: token_identity(&token),
        token_id: token.id,
    })
}

fn auth_error(error: McpAuthError) -> Response {
    let (status, message) = match error {
        McpAuthError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            "MCP Bearer token 無效、類型不符、已撤銷或已過期",
        ),
        McpAuthError::Store(error) => {
            tracing::error!(%error, "MCP token 查詢失敗");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "目前無法驗證 MCP token，請稍後再試",
            )
        }
    };
    (
        status,
        Json(json!({
            "jsonrpc": "2.0", "id": Value::Null,
            "error": { "code": code::INVALID_REQUEST, "message": message }
        })),
    )
        .into_response()
}

fn rate_limited_error() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "jsonrpc": "2.0", "id": Value::Null,
            "error": { "code": code::RATE_LIMITED, "message": "已超過速率限制，請稍後再試" }
        })),
    )
        .into_response()
}

pub async fn handle(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    // Origin validation is independent of whether authentication succeeds; it
    // is the DNS rebinding guard the streamable HTTP spec requires, so it
    // comes first.
    if !origin_allowed(&headers, &s.mcp_allowed_origins) {
        tracing::warn!("MCP 請求的 Origin 不在允許清單內，已拒絕");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "jsonrpc": "2.0", "id": Value::Null,
                "error": { "code": code::INVALID_REQUEST, "message": "Origin 不在允許清單內" }
            })),
        )
            .into_response();
    }

    // Authentication precedes JSON parsing, so a request without valid
    // credentials cannot obtain a different status from a different body.
    let auth = match authenticate_mcp(&s.store, &headers).await {
        Ok(auth) => auth,
        Err(error) => return auth_error(error),
    };

    // Per-token rate limiting: a leaked token cannot be used without bound,
    // even while it remains valid.
    if !s.mcp_rate_limiter.allow(&auth.token_id.to_string()) {
        let summary = format!(
            "MCP 呼叫被擋下 · {}（token 編號 {}）超過每分鐘速率限制",
            auth.identity.actor, auth.token_id
        );
        if let Err(error) = s
            .store
            .write_audit(
                &auth.identity.actor,
                &auth.identity.actor_role,
                "mcp_rate_limit",
                "authentication",
                None,
                "mcp",
                &json!({ "token_id": auth.token_id }),
                &summary,
                "blocked",
                None,
            )
            .await
        {
            tracing::error!(%error, "MCP 速率限制的稽核寫入失敗");
        }
        return rate_limited_error();
    }

    let identity = auth.identity;
    let req: Value = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "jsonrpc": "2.0", "id": Value::Null,
                    "error": { "code": code::INVALID_REQUEST, "message": "請求 body 不是有效的 JSON" }
                })),
            )
                .into_response();
        }
    };

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = req.get("method").and_then(|m| m.as_str()) else {
        return err(id, code::INVALID_REQUEST, "缺少 method");
    };

    match method {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "laysi-one",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions":
                    "Laysi One 是台灣中小企業的財務與人資系統。所有寫入型操作都會留下\
                     稽核紀錄；高風險工具預設關閉，被擋下時請把原因如實告訴使用者，\
                     不要嘗試繞過。金額一律是字串形式的十進位數，不要轉成浮點數。",
            }),
        ),

        // A notification has no id and must have no response body.
        m if m.starts_with("notifications/") => StatusCode::ACCEPTED.into_response(),

        "ping" => ok(id, json!({})),

        "tools/list" => {
            // What this token can actually call, which is the same three
            // questions `dispatch` asks: the tool switch, the risk floor and
            // the role's permissions. Listing a tool the model will be refused
            // wastes a round trip and turns the conversation into an argument
            // with the permission system — and a staff token would otherwise
            // see sixty tools it cannot use.
            //
            // A database failure is reported as one. Answering with an empty
            // list would say "you are granted nothing", which is a valid
            // answer to a different question, and the operator looking for the
            // missing tools has nothing to go on.
            let (Ok(grants), Ok(permitted)) = (
                s.store.tool_grants().await,
                s.store.permitted_ops(&identity.actor_role).await,
            ) else {
                return err(id, code::INTERNAL, "讀不到工具授權，請稍後再試");
            };
            let tools: Vec<Value> = s
                .registry
                .iter()
                .filter(|op| {
                    grants
                        .iter()
                        .find(|(n, _)| n == op.name())
                        .map(|(_, e)| *e)
                        .unwrap_or(false)
                })
                .filter(|op| {
                    op.risk().allows_role(&identity.actor_role) && permitted.contains(op.name())
                })
                .map(|op| {
                    json!({
                        "name": op.name(),
                        "description": format!("[{}] {}", op.risk().label_zh(), op.summary()),
                        "inputSchema": op.input_schema(),
                    })
                })
                .collect();
            ok(id, json!({ "tools": tools }))
        }

        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let Some(name) = params.get("name").and_then(|n| n.as_str()) else {
                return err(id, code::INVALID_REQUEST, "缺少工具名稱");
            };
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            // What the user actually said: MCP clients may pass it in _meta so
            // the audit can record intent.
            let prompt = params
                .get("_meta")
                .and_then(|m| m.get("prompt"))
                .and_then(|p| p.as_str())
                .map(str::to_string);

            let ctx = crate::ctx_from(&s, &identity, prompt, laysi_ops::Channel::Mcp);
            match s.registry.dispatch(&ctx, name, args).await {
                Ok(data) => ok(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&data).unwrap_or_default()
                        }],
                        "isError": false,
                    }),
                ),
                // A failed tool returns isError rather than a JSON-RPC error, so
                // the model can see the reason and relay it, instead of getting a
                // protocol-level failure.
                Err(e) => ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": e.to_string() }],
                        "isError": true,
                    }),
                ),
            }
        }

        other => err(
            id,
            code::METHOD_NOT_FOUND,
            format!("不支援的 method：{other}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::Row;

    fn lazy_store() -> laysi_store::Store {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgres@127.0.0.1/laysi_test")
            .expect("測試用延遲連線池必須可建立");
        laysi_store::Store::from_pool(pool)
    }

    #[tokio::test]
    async fn forged_identity_headers_without_bearer_token_are_unauthorized() {
        let mut headers = HeaderMap::new();
        headers.insert(
            concat!("x-laysi-", "actor"),
            "forged-user".parse().expect("測試標頭必須有效"),
        );
        headers.insert(
            concat!("x-laysi-", "role"),
            "boss".parse().expect("測試標頭必須有效"),
        );

        assert!(matches!(
            authenticate_mcp(&lazy_store(), &headers).await,
            Err(McpAuthError::Unauthorized)
        ));
        assert_eq!(
            auth_error(McpAuthError::Unauthorized).status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
    /// mcp_identity_is_bound_to_token_row_and_revocation_is_immediate -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn mcp_identity_is_bound_to_token_row_and_revocation_is_immediate() {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = laysi_store::Store::connect(&database_url, 2)
            .await
            .expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");
        let issued = store
            .issue_api_token(laysi_store::IssueApiToken {
                name: "MCP 身分綁定測試",
                kind: "mcp",
                scope: "測試",
                actor: "Token A 綁定身分",
                actor_role: "staff",
                created_by: "整合測試",
                rotate_days: None,
                expiry_note: "測試結束即撤銷",
                owner_employee_id: None,
            })
            .await
            .expect("應可簽發測試 MCP token");
        let expected_hash = laysi_store::token_hash(&issued.secret);
        let stored_hash: String = sqlx::query("SELECT token_hash FROM api_token WHERE id = $1")
            .bind(issued.token.id)
            .fetch_one(store.pool())
            .await
            .expect("應可讀回測試 token 雜湊")
            .get("token_hash");
        assert_eq!(stored_hash, expected_hash);
        assert_ne!(stored_hash, issued.secret);

        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", issued.secret)
                .parse()
                .expect("Bearer 標頭必須有效"),
        );
        headers.insert(
            concat!("x-laysi-", "actor"),
            "forged-boss".parse().expect("測試標頭必須有效"),
        );
        headers.insert(
            concat!("x-laysi-", "role"),
            "boss".parse().expect("測試標頭必須有效"),
        );
        let auth = authenticate_mcp(&store, &headers)
            .await
            .expect("有效 MCP token 應通過");
        assert_eq!(auth.identity.actor, "Token A 綁定身分");
        assert_eq!(auth.identity.actor_role, "staff");
        assert_eq!(auth.identity.employee_id, None);
        assert_eq!(auth.token_id, issued.token.id);

        store
            .revoke_api_token(issued.token.id, "整合測試")
            .await
            .expect("應可撤銷測試 token");
        assert!(matches!(
            authenticate_mcp(&store, &headers).await,
            Err(McpAuthError::Unauthorized)
        ));
        sqlx::query("DELETE FROM api_token WHERE id = $1")
            .bind(issued.token.id)
            .execute(store.pool())
            .await
            .expect("應可清除測試 token");
    }

    /// Expiry and revocation must both be rejected and must both be
    /// distinguishable in the audit. This checks `denial_reason` directly (a
    /// pure function needing no database); the database-dependent flow is in
    /// the preceding `#[ignore]` test and in
    /// `mcp_rejects_an_expired_token_even_though_it_was_never_revoked`.
    ///
    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
    /// mcp_rejects_an_expired_token_even_though_it_was_never_revoked -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn mcp_rejects_an_expired_token_even_though_it_was_never_revoked() {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = laysi_store::Store::connect(&database_url, 2)
            .await
            .expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");
        let issued = store
            .issue_api_token(laysi_store::IssueApiToken {
                name: "MCP 過期測試",
                kind: "mcp",
                scope: "測試",
                actor: "Token 過期測試身分",
                actor_role: "staff",
                created_by: "整合測試",
                rotate_days: Some(1),
                expiry_note: "測試用：1 天後過期",
                owner_employee_id: None,
            })
            .await
            .expect("應可簽發測試 MCP token");

        // rotate_days = 1, with created_at moved back two days to simulate
        // expiry without waiting.
        sqlx::query("UPDATE api_token SET created_at = now() - interval '2 days' WHERE id = $1")
            .bind(issued.token.id)
            .execute(store.pool())
            .await
            .expect("應可竄改測試 token 的建立時間");

        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", issued.secret)
                .parse()
                .expect("Bearer 標頭必須有效"),
        );

        // Never revoked but expired: still rejected, and externally the same
        // 401.
        assert!(matches!(
            authenticate_mcp(&store, &headers).await,
            Err(McpAuthError::Unauthorized)
        ));
        let stored: Option<DateTime<Utc>> =
            sqlx::query("SELECT revoked_at FROM api_token WHERE id = $1")
                .bind(issued.token.id)
                .fetch_one(store.pool())
                .await
                .expect("應可讀回測試 token")
                .get("revoked_at");
        assert_eq!(stored, None, "過期不等於撤銷，revoked_at 應維持 NULL");

        sqlx::query("DELETE FROM api_token WHERE id = $1")
            .bind(issued.token.id)
            .execute(store.pool())
            .await
            .expect("應可清除測試 token");
    }

    fn sample_token(
        revoked: bool,
        rotate_days: Option<i32>,
        created_days_ago: i64,
    ) -> laysi_store::ApiToken {
        laysi_store::ApiToken {
            id: 1,
            name: "測試 Token".into(),
            kind: "mcp".into(),
            scope: "測試".into(),
            prefix: "lys_test".into(),
            actor: "測試員".into(),
            actor_role: "staff".into(),
            actor_role_name: None,
            created_at: Utc::now() - chrono::Duration::days(created_days_ago),
            created_by: "測試".into(),
            last_used_at: None,
            rotate_days,
            expiry_note: String::new(),
            revoked_at: revoked.then(Utc::now),
            revoked_by: String::new(),
            owner_employee_id: None,
        }
    }

    #[test]
    fn denial_reason_distinguishes_revoked_from_expired() {
        let live = sample_token(false, None, 0);
        assert!(denial_reason(&live, Utc::now()).is_none());

        let revoked = sample_token(true, None, 0);
        assert!(matches!(
            denial_reason(&revoked, Utc::now()),
            Some(Denial::Revoked)
        ));

        let expired = sample_token(false, Some(1), 3);
        assert!(matches!(
            denial_reason(&expired, Utc::now()),
            Some(Denial::Expired)
        ));

        // Revocation is checked before expiry: when both apply the audit
        // summary should say revoked, because revocation is an explicit human
        // decision while expiry is only a rotation deadline passing.
        let both = sample_token(true, Some(1), 3);
        assert!(matches!(
            denial_reason(&both, Utc::now()),
            Some(Denial::Revoked)
        ));
    }

    #[test]
    fn origin_header_absent_is_allowed_but_present_must_match_allowlist() {
        let allowed = vec!["https://claude.ai".to_string()];

        let mut no_origin = HeaderMap::new();
        assert!(origin_allowed(&no_origin, &allowed));

        let mut good = HeaderMap::new();
        good.insert(header::ORIGIN, "https://claude.ai".parse().unwrap());
        assert!(origin_allowed(&good, &allowed));

        let mut bad = HeaderMap::new();
        bad.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!origin_allowed(&bad, &allowed));

        // With an empty allow list (the default) any request carrying an Origin
        // must be rejected, which accepts only normal AI connectors that send
        // none.
        no_origin.insert(header::ORIGIN, "https://anything.example".parse().unwrap());
        assert!(!origin_allowed(&no_origin, &[]));
    }
}
