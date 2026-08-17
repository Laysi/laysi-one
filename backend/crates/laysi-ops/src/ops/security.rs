//! Security policy, API and MCP tokens, system health.
//!
//! A security policy must not be prose. Every rule here either carries an
//! `enforced` flag ([`GetSecurityPolicy`] states whether it is actually
//! applied) or is not stored at all and derived on each call:
//!
//! - What AI may not do is derived from `mcp_tool_grant`, and high-risk tools
//!   that are still enabled are listed alongside.
//! - Amount thresholds are derived from `approval_rule`, so editing the table
//!   changes the screen.
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
        Box::new(GetSecurityPolicy),
        Box::new(ListApiTokens),
        Box::new(IssueApiToken),
        Box::new(RevokeApiToken),
        Box::new(GetSystemHealth),
    ]
}

/// Join a few names into one sentence, eliding the rest. Display only.
fn short_list(names: &[String]) -> String {
    match names.len() {
        0 => "（無）".into(),
        1..=3 => names.join("、"),
        n => format!("{}、{}、{} 等 {n} 項", names[0], names[1], names[2]),
    }
}

// Control rules

struct GetSecurityPolicy;

#[async_trait]
impl Operation for GetSecurityPolicy {
    fn name(&self) -> &'static str {
        "get_security_policy"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀控制規則：職責分離、金額門檻、雙因素、AI 禁止事項、稽核保留。逐條標示是否實際執行；AI 禁止事項依目前的工具開關即時計算，非固定文字。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let stored = ctx.store.security_policies().await?;
        let grants = ctx.store.tool_grant_details().await?;
        let rules = ctx.store.approval_rules().await?;

        // "What AI may not do" is the set of tools currently disabled. The
        // sentence changes with the switches because it is the switches, not
        // a description of them.
        let blocked: Vec<String> =
            grants.iter().filter(|g| !g.enabled).map(|g| g.tool_name.clone()).collect();
        // Tools still enabled that move money or change the books. This matters
        // more than the line above: what the user needs to know is not which
        // tools are off, but which are still on.
        let risky_on: Vec<String> = grants
            .iter()
            .filter(|g| g.enabled && matches!(g.risk.as_str(), "high_risk" | "boss_only"))
            .map(|g| g.tool_name.clone())
            .collect();

        let mut rows = vec![json!({
            "code": "ai_limits",
            "label": "AI 禁止事項",
            "value": format!("AI 目前不能呼叫：{}", short_list(&blocked)),
            "detail": if risky_on.is_empty() {
                "會動到錢或帳的工具目前全部對 AI 關閉。".to_string()
            } else {
                format!(
                    "注意：{} 仍然對 AI 開著，它會動到錢或帳。要關請到「MCP 與 AI」。",
                    short_list(&risky_on)
                )
            },
            // This one really does stop a call: the first step of dispatch is the
            // mcp_tool_grant lookup.
            "enforced": true,
            "source": "mcp_tool_grant",
            "sort_order": 10,
            "blocked_tools": blocked,
            "risky_enabled_tools": risky_on,
        })];

        // The amount thresholds are the ones in the approval flow, not a copy.
        let thresholds: Vec<String> = rules
            .iter()
            .filter_map(|r| {
                r.min_amount.map(|a| {
                    format!("{} 以上{}", crate::money(a), r.role_name.as_deref().unwrap_or("核准"))
                })
            })
            .collect();
        rows.push(json!({
            "code": "amount_threshold",
            "label": "金額門檻",
            "value": if thresholds.is_empty() { "未設定門檻".into() } else { thresholds.join("　") },
            "detail": "門檻是公司政策，在「角色與簽核」改動後立即生效。費用報支送出時即依金額決定要經過幾關，簽核時逐關檢查身分。",
            "enforced": true,
            "source": "approval_rule",
            "sort_order": 30,
        }));

        rows.extend(stored.iter().map(|p| {
            json!({
                "code": p.code,
                "label": p.label,
                "value": p.value,
                "detail": p.detail,
                "enforced": p.enforced,
                "source": "policy",
                "sort_order": p.sort_order,
            })
        }));
        rows.sort_by_key(|r| r["sort_order"].as_i64().unwrap_or(0));

        let live = rows.iter().filter(|r| r["enforced"] == json!(true)).count();
        let summary = format!(
            "讀取控制規則 · {} 條，其中 {live} 條執行中 · AI 關閉 {} 個工具",
            rows.len(),
            blocked.len()
        );
        Ok(OpOutcome::new(
            json!({ "rules": rows, "enforced_count": live, "total": rows.len() }),
            summary,
        ))
    }
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
        "讀 API 與 MCP Token 清單：名稱、用途範圍、代表對象、最後使用時間、輪替期限。僅保留前八碼與雜湊，不存明文，系統也無法回查。"
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

        let live = tokens.iter().filter(|t| t.revoked_at.is_none() && !t.is_expired(now)).count();
        let summary = format!("讀取 Token 清單 · {} 把有效、共 {} 筆", live, rows.len());
        Ok(OpOutcome::new(
            json!({
                "tokens": rows,
                "live_count": live,
                "storage_note":
                    "系統只存不可還原的雜湊與前八碼，明文只在發出來的那一次回傳。",
                "wiring_note":
                    "MCP 連線以雜湊比對驗證，身分取自該把 Token 的登記資料，\
                     每次成功驗證都會更新「最後使用」。設定輪替天數後，\
                     期限一到即強制拒絕，不必等人手動撤銷。",
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
        Risk::BossOnly
    }
    fn summary(&self) -> &'static str {
        "核發一組新的 API 或 MCP Token。明文僅出現於本次回應，之後系統無法回查；系統僅保存雜湊與前八碼。"
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
            return Err(OpError::BadInput("Token 要有名字，否則清單上認不出是哪一把".into()));
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
                // `issue_my_api_token` sets an owner.
                owner_employee_id: None,
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
                    "明文僅顯示這一次，請立即複製保存；系統僅保留雜湊，無法回查。",
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
        "撤銷一把 Token。撤銷之後不能復原，要用就重發一把。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RevokeApiTokenInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RevokeApiTokenInput = parse(input)?;
        let t = ctx.store.revoke_api_token(i.id, &ctx.actor).await?;
        let summary = format!("撤銷 Token · {}（{}…）· {} 就連不進來了", t.name, t.prefix, t.actor);
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
        "讀系統現況：migration 版本、傳票與稽核筆數、Token 現況、目前生效的法規版本。品牌指紋與物件儲存連線由 /healthz 回答。"
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
        let stale = rulesets.iter().filter(|r| r["applicable"] == json!(false)).count();

        let summary = format!(
            "讀取系統現況 · migration {} · 已過帳 {} 張 · 稽核 {} 筆 · Token {} 把",
            migration.map(|v| v.to_string()).unwrap_or_else(|| "未知".into()),
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
                    "品牌完整性指紋與物件儲存連線狀態不在此處，兩者由 GET /healthz 回答。",
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Revoking must not require the owner: every minute spent waiting for a
    /// person while a token is leaked is a loss. Issuing is the action that
    /// needs their approval.
    #[test]
    fn revoking_is_easier_than_issuing_on_purpose() {
        let by = |name: &str| ops().into_iter().find(|o| o.name() == name).map(|o| o.risk());
        assert_eq!(by("issue_api_token"), Some(Risk::BossOnly));
        assert_eq!(by("revoke_api_token"), Some(Risk::HighRisk));
        assert_ne!(by("revoke_api_token"), Some(Risk::ReadOnly), "撤銷會改狀態，不是唯讀");
    }

    /// Listing tokens is read-only, but only because the list holds no
    /// plaintext. Adding the secret to the output would invalidate that tier.
    #[test]
    fn listing_tokens_is_read_only_because_it_carries_no_secret() {
        let op = ops().into_iter().find(|o| o.name() == "list_api_tokens").unwrap();
        assert_eq!(op.risk(), Risk::ReadOnly);
        assert!(op.summary().contains("不存明文"));
    }

    #[test]
    fn short_list_stays_readable_when_there_are_many() {
        let names = |n: usize| -> Vec<String> { (1..=n).map(|i| format!("t{i}")).collect() };
        assert_eq!(short_list(&names(0)), "（無）");
        assert_eq!(short_list(&names(1)), "t1");
        assert_eq!(short_list(&names(3)), "t1、t2、t3");
        assert_eq!(short_list(&names(7)), "t1、t2、t3 等 7 項");
    }

    /// A sentinel: figures on the security screen may come only from the
    /// database or the rule library. Writing a rate or threshold into this
    /// module to make the screen look better fails here.
    #[test]
    fn the_module_never_states_a_rate_of_its_own() {
        for op in ops() {
            assert!(!op.summary().contains('%'), "{} 的說明裡不該有費率", op.name());
        }
    }
}
