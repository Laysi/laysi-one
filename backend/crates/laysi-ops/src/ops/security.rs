//! API and MCP tokens, system health.
//!
//! `api_token` stores only a BLAKE3 hash and the first eight characters: the
//! prefix lets a person recognise which token a row is, the hash lets the
//! system compare. The cost is that the plaintext [`IssueApiToken`] returns is
//! visible exactly once and a forgotten token can only be reissued, which the
//! screen has to say.
//!
//! Bearer auth on `/mcp` reads this table directly: hash the plaintext with
//! BLAKE3, find the unrevoked row with `kind = mcp`, take the identity solely
//! from that row's actor and actor_role, and update `last_used_at`. Request
//! headers cannot override the identity bound to the token.

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use laysi_store::IssueApiToken as IssueReq;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListApiTokens),
        Box::new(IssueApiToken),
        Box::new(RevokeApiToken),
        Box::new(GetSystemHealth),
    ]
}

// Tokens

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListApiTokensInput {
    /// Include revoked tokens; excluded by default.
    #[serde(default)]
    pub include_revoked: bool,
}

struct ListApiTokens;

#[async_trait]
impl Operation for ListApiTokens {
    fn name(&self) -> &'static str {
        "list_api_tokens"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀 API 與 MCP Token 清單：名稱、用途範圍、代表對象、最後使用時間、輪替期限。只保留前八碼與雜湊。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListApiTokensInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListApiTokensInput = parse(input)?;
        let tokens = ctx.store.api_tokens(i.include_revoked).await?;
        let now = chrono::Utc::now();

        let rows: Vec<Value> = tokens
            .iter()
            .map(|t| {
                // Days until rotation. Tokens with no fixed cycle ("expires at
                // period close") have none, and inventing a number would imply
                // it means something. This is a reminder estimate and separate
                // from whether the token is actually expired below: one reads
                // the last use, the other the creation time.
                let due = t.rotate_days.and_then(|d| {
                    let base = t.last_used_at.unwrap_or(t.created_at).date_naive();
                    base.checked_add_signed(chrono::Duration::days(i64::from(d)))
                });
                // This is the expiry that actually blocks /mcp auth:
                // created_at + rotate_days.
                let expired = t.is_expired(now);
                json!({
                    "id": t.id,
                    "name": t.name,
                    "kind": t.kind,
                    "kind_label": if t.kind == "mcp" { "MCP Token" } else { "API Token" },
                    "scope": t.scope,
                    "prefix": t.prefix,
                    "actor": t.actor,
                    "actor_role": t.actor_role,
                    "actor_role_name": t.actor_role_name,
                    "created_at": t.created_at,
                    "created_by": t.created_by,
                    "last_used_at": t.last_used_at,
                    "rotate_days": t.rotate_days,
                    "rotate_due_on": due,
                    "overdue": due.is_some_and(|d| d < ctx.today),
                    "expires_at": t.expires_at(),
                    "expired": expired,
                    "expiry_note": t.expiry_note,
                    "revoked_at": t.revoked_at,
                    "revoked_by": t.revoked_by,
                    // Revocation and expiry both stop a token connecting, so the
                    // "live" column has to consider both or the screen would call
                    // an expired token live.
                    "live": t.revoked_at.is_none() && !expired,
                })
            })
            .collect();

        let live = tokens
            .iter()
            .filter(|t| t.revoked_at.is_none() && !t.is_expired(now))
            .count();
        let summary = format!("讀取 Token 清單 · {} 組有效、共 {} 筆", live, rows.len());
        Ok(OpOutcome::new(
            json!({
                "tokens": rows,
                "live_count": live,
                "wiring_note": "設定輪替天數後，超過期限就不能連線。",
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct IssueApiTokenInput {
    /// Name of this token, e.g. "website orders".
    pub name: String,
    /// `api` or `mcp`.
    pub kind: String,
    /// Scope, as one human-readable line, e.g. "read-only plus drafts".
    pub scope: String,
    /// Who this token acts as. AI connecting with it is never more privileged
    /// than that person.
    pub actor: String,
    /// That person's role code, e.g. `acct`.
    pub actor_role: String,
    /// Rotation period in days. Omit for no fixed cycle and explain in
    /// expiry_note.
    #[serde(default)]
    pub rotate_days: Option<i32>,
    /// Description of the expiry condition, e.g. "expires at period close".
    #[serde(default)]
    pub expiry_note: Option<String>,
}

struct IssueApiToken;

#[async_trait]
impl Operation for IssueApiToken {
    fn name(&self) -> &'static str {
        "issue_api_token"
    }
    fn risk(&self) -> Risk {
        // Issuing a token issues an identity. Owner only.
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "核發一組新的 API 或 MCP Token。明文僅出現於本次回應。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<IssueApiTokenInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: IssueApiTokenInput = parse(input)?;
        if !matches!(i.kind.as_str(), "api" | "mcp") {
            return Err(OpError::BadInput(format!(
                "kind 只能是 api 或 mcp，收到的是「{}」",
                i.kind
            )));
        }
        if i.name.trim().is_empty() {
            return Err(OpError::BadInput(
                "Token 要有名稱，否則清單上無法分辨是哪一組".into(),
            ));
        }
        if let Some(d) = i.rotate_days {
            if d <= 0 {
                return Err(OpError::BadInput("輪替天數要大於零".into()));
            }
        }
        let expiry_note = i
            .expiry_note
            .clone()
            .unwrap_or_else(|| match i.rotate_days {
                Some(d) => format!("{d} 天後輪替"),
                None => "未設定輪替週期".into(),
            });

        let issued = ctx
            .store
            .issue_api_token(IssueReq {
                name: i.name.trim(),
                kind: &i.kind,
                scope: &i.scope,
                actor: &i.actor,
                actor_role: &i.actor_role,
                created_by: &ctx.actor,
                rotate_days: i.rotate_days,
                expiry_note: &expiry_note,
                // A token the owner issued on the installation's behalf
                // belongs to no one person, even when it acts as one. Only
                // `issue_my_api_token` and OAuth set an owner.
                owner_employee_id: None,
                owner_user_id: None,
            })
            .await?;

        // The audit summary must not contain the plaintext. Audit records are
        // meant to be read, and putting the credential there would hand the
        // token to everyone who can read them.
        let summary = format!(
            "發出 Token · {}（{}…）· 代表 {} · {}",
            issued.token.name, issued.token.prefix, issued.token.actor, expiry_note
        );
        Ok(OpOutcome::new(
            json!({
                "token": issued.token,
                "secret": issued.secret,
                "secret_note":
                    "明文只顯示一次，請立即複製保存。",
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct RevokeApiTokenInput {
    /// Token id, from `list_api_tokens`.
    pub id: i32,
}

struct RevokeApiToken;

#[async_trait]
impl Operation for RevokeApiToken {
    fn name(&self) -> &'static str {
        "revoke_api_token"
    }
    fn risk(&self) -> Risk {
        // Deliberately not BossOnly: revoking moves in the safe direction. A
        // leaked token must not wait for the owner to be available. Issuing is
        // the action that needs the owner.
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "撤銷一組 Token。撤銷後不能復原，需要時重新核發。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RevokeApiTokenInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RevokeApiTokenInput = parse(input)?;
        let t = ctx.store.revoke_api_token(i.id, &ctx.actor).await?;
        let summary = format!(
            "撤銷 Token · {}（{}…）· {} 之後無法連線",
            t.name, t.prefix, t.actor
        );
        Ok(OpOutcome::new(json!({ "token": t }), summary))
    }
}

// System health

struct GetSystemHealth;

#[async_trait]
impl Operation for GetSystemHealth {
    fn name(&self) -> &'static str {
        "get_system_health"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀系統現況：migration 版本、傳票與稽核筆數、Token 現況、目前生效的法規版本。物件儲存連線由 /healthz 回答。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let migration = ctx.store.migration_version().await?;
        let stats = ctx.store.system_stats().await?;

        // Ruleset versions currently in force. This section is itself a health
        // check: a domain with no applicable version today means the rule
        // library needs updating, and the related calculations will error.
        let rulesets: Vec<Value> = laysi_regulation::DOMAINS
            .iter()
            .map(|domain| {
                let rs = laysi_regulation::resolve(domain, ctx.today).ok();
                json!({
                    "domain": domain,
                    "version": rs.map(|r| r.version.clone()),
                    "effective_from": rs.map(|r| r.effective_from),
                    "applicable": rs.is_some(),
                })
            })
            .collect();
        let stale = rulesets
            .iter()
            .filter(|r| r["applicable"] == json!(false))
            .count();

        let summary = format!(
            "讀取系統現況 · migration {} · 已過帳 {} 張 · 稽核 {} 筆 · Token {} 組",
            migration
                .map(|v| v.to_string())
                .unwrap_or_else(|| "未知".into()),
            stats.posted_vouchers,
            stats.audit_entries,
            stats.tokens_live
        );
        Ok(OpOutcome::new(
            json!({
                "migration": migration,
                "stats": stats,
                "rulesets": rulesets,
                "rulesets_missing": stale,
                "as_of": ctx.today,
                "note":
                    "物件儲存連線狀態由服務健康檢查回報。",
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Revoking must not require the administrator: every minute spent waiting
    /// for a person while a token is leaked is a loss. Issuing is the action
    /// that needs their approval.
    #[test]
    fn revoking_is_easier_than_issuing_on_purpose() {
        let by = |name: &str| {
            ops()
                .into_iter()
                .find(|o| o.name() == name)
                .map(|o| o.risk())
        };
        assert_eq!(by("issue_api_token"), Some(Risk::AdminOnly));
        assert_eq!(by("revoke_api_token"), Some(Risk::HighRisk));
        assert_ne!(
            by("revoke_api_token"),
            Some(Risk::ReadOnly),
            "撤銷會改狀態，不是唯讀"
        );
    }

    /// Listing tokens is read-only, but only because the list holds no
    /// plaintext. Adding the secret to the output would invalidate that tier.
    #[test]
    fn listing_tokens_is_read_only_because_it_carries_no_secret() {
        let op = ops()
            .into_iter()
            .find(|o| o.name() == "list_api_tokens")
            .unwrap();
        assert_eq!(op.risk(), Risk::ReadOnly);
        assert!(op.summary().contains("只保留前八碼與雜湊"));
    }

    /// A sentinel: figures on the security screen may come only from the
    /// database or the rule library. Writing a rate or threshold into this
    /// module to make the screen look better fails here.
    #[test]
    fn the_module_never_states_a_rate_of_its_own() {
        for op in ops() {
            assert!(
                !op.summary().contains('%'),
                "{} 的說明裡不該有費率",
                op.name()
            );
        }
    }
}
