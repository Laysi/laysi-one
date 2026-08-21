//! Operation registry: the mechanism that makes "every operation is available
//! over MCP" a structural guarantee rather than a discipline.
//!
//! Each operation is defined exactly once by implementing [`Operation`]. The
//! registry derives the REST routes, the MCP `tools/list` response and the
//! TypeScript types from that single definition, so all three share one JSON
//! Schema and one `call`. There is no second place to forget.
//!
//! `laysi-server` carries a parity test asserting that the REST and MCP tool
//! name sets are identical, which fails if anyone hand-writes a route that
//! bypasses the registry.
//!
//! [`Registry::dispatch`] runs a fixed sequence: tool grant, risk, role
//! permission, execute, audit. Blocked calls are audited too — a denied
//! attempt is the entry most worth keeping.

use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_store::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod ops;

// Risk levels

/// Risk level of an operation, shown as four tiers on the MCP screen.
/// The role that owns the business: payroll, budget, approval thresholds.
pub const ROLE_BOSS: &str = "boss";
/// The role that owns the system: permissions, tokens, settings, logins.
pub const ROLE_ADMIN: &str = "admin";
/// Login name of the account the browser creates at first run. It stands for
/// the installation rather than a person, so it is the one login that is not
/// an email address and the one that keeps `email` empty: an address would
/// let an external identity resolve onto it by `users_by_email`, which is the
/// account that must stay reachable by password alone.
pub const ADMIN_USERNAME: &str = "admin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Reads data, changes no state.
    ReadOnly,
    /// Produces a draft that a human must confirm before it becomes real.
    NeedsConfirm,
    /// Changes the books directly. Off by default.
    HighRisk,
    /// Touches payroll or similar sensitive data; owner only.
    BossOnly,
    /// Controls the system itself: role permissions, API tokens, company
    /// settings and logins. Separate from `BossOnly` so that whoever runs the
    /// installation does not thereby read everyone's salary.
    AdminOnly,
}

impl Risk {
    /// Every tier, so generators and migrations cannot drift from the enum.
    pub const ALL: [Risk; 5] = [
        Risk::ReadOnly,
        Risk::NeedsConfirm,
        Risk::HighRisk,
        Risk::BossOnly,
        Risk::AdminOnly,
    ];

    pub fn label_zh(&self) -> &'static str {
        match self {
            Risk::ReadOnly => "唯讀",
            Risk::NeedsConfirm => "需人確認",
            Risk::HighRisk => "高風險",
            Risk::BossOnly => "限負責人",
            Risk::AdminOnly => "限系統管理員",
        }
    }

    pub fn as_db(&self) -> &'static str {
        match self {
            Risk::ReadOnly => "read_only",
            Risk::NeedsConfirm => "needs_confirm",
            Risk::HighRisk => "high_risk",
            Risk::BossOnly => "boss_only",
            Risk::AdminOnly => "admin_only",
        }
    }
}

// Execution context and errors

/// Where a call came from.
///
/// The distinction matters: `mcp_tool_grant` governs what AI may do, not
/// what people may do. An accountant pressing "post" on screen and a model
/// deciding on its own to call `post_voucher` are different events, so
/// disabling `post_voucher` for AI must not stop the accountant from
/// posting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// A signed-in person acting on screen.
    Ui,
    /// An AI client over MCP, subject to `mcp_tool_grant`.
    Mcp,
}

impl Channel {
    pub fn as_db(&self) -> &'static str {
        match self {
            Channel::Ui => "ui",
            Channel::Mcp => "mcp",
        }
    }
}

/// Execution context for a single call.
pub struct OpCtx {
    pub store: Store,
    /// RustFS object storage, `None` when the configuration is incomplete.
    /// The document operations then report that object storage is not
    /// connected rather than quietly storing files in the database.
    pub objects: Option<laysi_store::ObjectStore>,
    /// The caller. For MCP calls this is the identity bound to the token, not
    /// something the AI asserted.
    pub actor: String,
    pub actor_role: String,
    /// Login name of the caller, `None` for an MCP token, which is bound to a
    /// person rather than to an account. Operations that must tell the caller
    /// apart from the account they are editing need a key `actor` cannot be:
    /// display names collide.
    pub username: Option<String>,
    /// Employee primary key linked to the login. External accountants and MCP
    /// tokens may have none.
    pub employee_id: Option<i32>,
    /// What the user actually said to the AI. Without it the audit trail
    /// records the action but not the intent.
    pub prompt: Option<String>,
    pub channel: Channel,
    pub today: NaiveDate,
}

impl OpCtx {
    /// Object storage, or a clear error when it is not configured.
    pub fn objects(&self) -> Result<&laysi_store::ObjectStore, OpError> {
        self.objects.as_ref().ok_or_else(|| {
            OpError::BadInput(
                "尚未接上物件儲存，文件功能無法使用；請洽系統管理員完成設定。"
                    .into(),
            )
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("查無此操作：{0}")]
    UnknownTool(String),

    #[error("工具「{tool}」目前為關閉狀態，請先至「MCP 與 AI」啟用。")]
    ToolDisabled { tool: String },

    #[error("「{tool}」限負責人執行，目前身分為「{role}」")]
    NeedsBoss { tool: String, role: String },

    #[error("「{tool}」限系統管理員執行，目前身分為「{role}」")]
    NeedsAdmin { tool: String, role: String },

    #[error("{role}未獲授權執行「{tool}」，請洽負責人於「角色與核准」開啟")]
    NotPermitted { tool: String, role: String },

    /// The message stands alone. A 「參數不正確：」 prefix appears in front of
    /// sentences the user never typed a parameter for — an empty budget, a
    /// payroll run with nothing to run — where it names a cause that is not
    /// the reason.
    #[error("{0}")]
    BadInput(String),

    #[error("{0}")]
    Store(#[from] laysi_store::StoreError),

    #[error("{0}")]
    Storage(#[from] laysi_store::StorageError),

    #[error("{0}")]
    Regulation(#[from] laysi_regulation::RegulationError),
}

impl OpError {
    /// Machine-readable code for the API response.
    pub fn code(&self) -> &'static str {
        match self {
            OpError::UnknownTool(_) => "unknown_tool",
            OpError::ToolDisabled { .. } => "tool_disabled",
            OpError::NeedsBoss { .. } => "needs_boss",
            OpError::NeedsAdmin { .. } => "needs_admin",
            OpError::NotPermitted { .. } => "not_permitted",
            OpError::BadInput(_) => "bad_input",
            OpError::Store(_) => "store_error",
            OpError::Storage(_) => "storage_error",
            OpError::Regulation(_) => "regulation_error",
        }
    }
}

/// The result of one successful execution.
///
/// `summary` is written to the audit trail, which has to be readable, so
/// every operation states what it did rather than leaving a bare
/// `get_cash_position() -> Ok`.
pub struct OpOutcome {
    pub data: Value,
    pub summary: String,
    pub voucher_no: Option<String>,
}

impl OpOutcome {
    pub(crate) fn new(data: Value, summary: impl Into<String>) -> Self {
        Self { data, summary: summary.into(), voucher_no: None }
    }
    pub(crate) fn with_voucher(mut self, no: impl Into<String>) -> Self {
        self.voucher_no = Some(no.into());
        self
    }
}

// Operation

#[async_trait]
pub trait Operation: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn risk(&self) -> Risk;
    /// Description shown to the model in MCP `tools/list`; it decides whether
    /// the tool gets used correctly.
    fn summary(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError>;
}

pub(crate) fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({ "type": "object" }))
}

pub(crate) fn parse<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, OpError> {
    // Empty input means all defaults: MCP clients often send null, or omit
    // arguments entirely.
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| OpError::BadInput(e.to_string()))
}

/// Thousands-separated string for display.
pub(crate) fn money(v: Money) -> String {
    let s = v.round_dp(0).abs().to_string();
    let mut rev = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            rev.push(',');
        }
        rev.push(c);
    }
    let body: String = rev.chars().rev().collect();
    if v.is_sign_negative() && !v.is_zero() {
        format!("-{body}")
    } else {
        body
    }
}


// Shared input types
// Parameter shapes used by more than one module live here so each module
// does not define its own.

/// An operation that takes no parameters.
#[derive(Deserialize, JsonSchema, Default)]
pub struct Empty {}

/// Query as of a date. Needed by the regulation and balance operations.
#[derive(Deserialize, JsonSchema, Default)]
pub struct AsOfInput {
    /// Cut-off date (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub as_of: Option<NaiveDate>,
}

/// Limit the number of rows returned.
#[derive(Deserialize, JsonSchema, Default)]
pub struct LimitInput {
    /// Maximum rows to return, default 50.
    #[serde(default)]
    pub limit: Option<i64>,
}

// Registry

pub struct Registry {
    ops: Vec<Box<dyn Operation>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Longest string kept verbatim in the audit trail. Longer ones are recorded
/// by length instead of by value.
const MAX_AUDITED_STRING: usize = 512;

/// Fields recorded as present rather than by value, whatever their length.
///
/// A national ID is ten characters, so the length rule never reaches it, and
/// it is personal data for life. Which employee a call was about is still
/// answerable: `emp_no` travels in the same parameters. A bank account number
/// is the same case, and nothing about a past call needs it either.
const REDACTED_FIELDS: [&str; 3] = ["id_no", "client_secret", "account_no"];

/// Parameters as the audit trail should hold them.
///
/// File contents travel through the parameters as base64, so without this
/// every upload would leave a second copy of the file in `ai_audit_log`,
/// megabytes at a time, in a table meant to be read by people. Which file it
/// was is still answerable: `document_version` keeps the BLAKE3, and that hash
/// is over the bytes that landed rather than over the bytes that were sent.
///
/// For bytes the rule is the length of the value, not the name of the field,
/// so a future operation that carries bytes is covered without being listed
/// here. [`REDACTED_FIELDS`] is the exception, and says why.
fn redact_for_audit(input: &Value) -> Value {
    match input {
        Value::String(text) if text.len() > MAX_AUDITED_STRING => {
            Value::String(format!("[內容 {} 位元組，未存入稽核紀錄]", text.len()))
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_for_audit).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    if REDACTED_FIELDS.contains(&key.as_str()) && value.is_string() {
                        return (key.clone(), Value::String("[已遮蔽]".into()));
                    }
                    (key.clone(), redact_for_audit(value))
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Screen-facing name for an operation, taken from the first clause of its
/// summary.
///
/// Refusals and the audit trail are read by people, so they must not carry
/// the registry key: 「update_company_setting」 names nothing the reader has
/// seen. Deriving the name from the summary keeps one copy of the wording
/// rather than a second field that drifts away from it.
/// The actor's role as the screens name it. Falls back to the code only when
/// the role row has gone, which is a database inconsistency rather than
/// something to render.
pub(crate) async fn role_name(ctx: &OpCtx) -> String {
    ctx.store
        .role_name(&ctx.actor_role)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| ctx.actor_role.clone())
}

pub fn op_label(summary: &str) -> &str {
    let label = summary.split(['：', '。', '，', '（']).next().unwrap_or(summary);
    if label.is_empty() {
        summary
    } else {
        label
    }
}

impl Registry {
    pub fn new() -> Self {
        Self { ops: ops::all() }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Operation> {
        self.ops.iter().find(|o| o.name() == name).map(|b| b.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Operation> {
        self.ops.iter().map(|b| b.as_ref())
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The single entry point for every call, REST and MCP alike.
    pub async fn dispatch(&self, ctx: &OpCtx, name: &str, input: Value) -> Result<Value, OpError> {
        let op = self.get(name).ok_or_else(|| OpError::UnknownTool(name.to_string()))?;

        // 1. Tool grant: a disabled tool is invisible to AI, even for reading.
        //    MCP only; people on screen are unaffected (see [`Channel`]).
        if ctx.channel == Channel::Mcp && ctx.store.tool_enabled(name).await? != Some(true) {
            self.audit(ctx, op, &input, "被拒絕：工具未開啟", "blocked").await;
            return Err(OpError::ToolDisabled { tool: name.to_string() });
        }

        // 2. Risk floor: two operations are pinned to one role each, outside
        //    the permission matrix. BossOnly guards business confidentiality
        //    (payroll, withholding, budget); AdminOnly guards the system's own
        //    controls (permissions, tokens, settings, logins). Neither role can
        //    be given the other's floor through `set_role_permission`, and no
        //    operation changes `user_account.role_code`, so the split holds.
        match op.risk() {
            Risk::BossOnly if ctx.actor_role != ROLE_BOSS => {
                self.audit(ctx, op, &input, "被拒絕：需負責人身分", "blocked").await;
                return Err(OpError::NeedsBoss {
                    tool: op_label(op.summary()).to_string(),
                    role: role_name(ctx).await,
                });
            }
            Risk::AdminOnly if ctx.actor_role != ROLE_ADMIN => {
                self.audit(ctx, op, &input, "被拒絕：需系統管理員身分", "blocked").await;
                return Err(OpError::NeedsAdmin {
                    tool: op_label(op.summary()).to_string(),
                    role: role_name(ctx).await,
                });
            }
            _ => {}
        }

        // 3. Role permission, deny by default: not in the set means refused.
        //
        // Looked up on every call and never cached. That is one extra small
        // query, but it makes set_role_permission take effect immediately,
        // leaving no window in which a revoked permission still works.
        let permitted = ctx.store.permitted_ops(&ctx.actor_role).await?;
        if !permitted.contains(name) {
            let role = role_name(ctx).await;
            let label = op_label(op.summary());
            let summary = format!("被拒絕：{role}未獲授權執行「{label}」");
            self.audit(ctx, op, &input, &summary, "blocked").await;
            return Err(OpError::NotPermitted { tool: label.to_string(), role });
        }

        // 4. Execute.
        let result = op.call(ctx, input.clone()).await;

        // 5. Audit. Read-only calls are recorded too: "the AI read my books"
        //    is itself a fact worth seeing.
        match &result {
            Ok(out) => {
                let _ = ctx
                    .store
                    .write_audit(
                        &ctx.actor,
                        &ctx.actor_role,
                        name,
                        op.risk().as_db(),
                        ctx.prompt.as_deref(),
                        ctx.channel.as_db(),
                        &redact_for_audit(&input),
                        &out.summary,
                        "ok",
                        out.voucher_no.as_deref(),
                    )
                    .await;
            }
            Err(e) => self.audit(ctx, op, &input, &e.to_string(), "error").await,
        }

        result.map(|o| o.data)
    }

    /// Write the audit entry. A failure to audit must not change the result
    /// of the operation, but it does belong in the server log.
    async fn audit(
        &self,
        ctx: &OpCtx,
        op: &dyn Operation,
        input: &Value,
        summary: &str,
        outcome: &str,
    ) {
        if let Err(e) = ctx
            .store
            .write_audit(
                &ctx.actor,
                &ctx.actor_role,
                op.name(),
                op.risk().as_db(),
                ctx.prompt.as_deref(),
                ctx.channel.as_db(),
                &redact_for_audit(input),
                summary,
                outcome,
                None,
            )
            .await
        {
            tracing::error!(tool = op.name(), error = %e, "稽核紀錄寫入失敗");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An uploaded file must not land in the audit table a second time, and
    /// everything a person reads the audit for must survive that.
    #[test]
    fn audited_parameters_keep_the_short_fields_and_drop_the_file() {
        let content = "A".repeat(MAX_AUDITED_STRING + 1);
        let redacted = redact_for_audit(&serde_json::json!({
            "claim_no": "EX2608-004",
            "file_name": "高鐵票.pdf",
            "content_base64": content,
            "nested": [{ "content_base64": content }],
            "emp_no": "E010",
            "id_no": "A123456789",
        }));

        assert_eq!(redacted["claim_no"], serde_json::json!("EX2608-004"));
        assert_eq!(redacted["file_name"], serde_json::json!("高鐵票.pdf"));
        let marker = format!("[內容 {} 位元組，未存入稽核紀錄]", content.len());
        assert_eq!(redacted["content_base64"], serde_json::json!(marker));
        assert_eq!(redacted["nested"][0]["content_base64"], serde_json::json!(marker));

        // Short enough to survive the length rule, personal data for life.
        // The employee is still identified by emp_no.
        assert_eq!(redacted["id_no"], serde_json::json!("[已遮蔽]"));
        assert_eq!(redacted["emp_no"], serde_json::json!("E010"));
    }

    #[test]
    fn registry_holds_every_operation_exactly_once() {
        let r = Registry::new();
        let mut names: Vec<&str> = r.iter().map(|o| o.name()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "有重複註冊的操作名稱");

        // Every module's operations must be reachable through the registry.
        // Forgetting a line in all() in ops/mod.rs fails here rather than in
        // the frontend when a screen finds nothing to call.
        let modules = [
            ops::ledger::ops(),
            ops::receivables::ops(),
            ops::payables::ops(),
            ops::banking::ops(),
            ops::tax::ops(),
            ops::assets::ops(),
            ops::expenses::ops(),
            ops::budget::ops(),
            ops::documents::ops(),
            ops::imports::ops(),
            ops::reports::ops(),
            ops::masters::ops(),
            ops::employees::ops(),
            ops::attendance::ops(),
            ops::insurance::ops(),
            ops::payroll::ops(),
            ops::hr_compliance::ops(),
            ops::selfservice::ops(),
            ops::settings::ops(),
            ops::security::ops(),
        ];
        let mut declared = 0;
        for module in modules {
            declared += module.len();
            for op in module {
                assert!(r.get(op.name()).is_some(), "{} 沒有註冊進 Registry", op.name());
            }
        }
        assert_eq!(before, declared, "註冊表裡有不屬於任何模組的操作");
    }

    fn grant_names(migration: &'static str) -> std::collections::BTreeSet<&'static str> {
        migration
            .split_once("INSERT INTO mcp_tool_grant")
            .expect("migration 沒有 mcp_tool_grant INSERT")
            .1
            .split_once(';')
            .expect("migration 的 mcp_tool_grant INSERT 沒有分號結尾")
            .0
            .lines()
            .filter_map(|line| line.trim().strip_prefix("('"))
            .filter_map(|line| line.split_once('\'').map(|(name, _)| name))
            .collect()
    }

    /// 0008, 0009, 0015, 0016, 0017 and 0020 seed the HR and self-service
    /// grants. One
    /// operation short and AI sees a switch it cannot call; one too many and
    /// there is no default MCP policy for it. The two sets must match exactly.
    #[test]
    fn hr_and_self_service_registry_matches_the_migration_grants() {
        use std::collections::BTreeSet;

        let migration_names: BTreeSet<&str> = [
            include_str!("../../../migrations/0008_hr_employees_attendance_payroll.sql"),
            include_str!("../../../migrations/0009_staff_self_service.sql"),
            include_str!("../../../migrations/0015_self_service_expense_claim.sql"),
            include_str!("../../../migrations/0016_attendance_punch.sql"),
            include_str!("../../../migrations/0017_self_service_mcp.sql"),
            include_str!("../../../migrations/0020_expense_claim_receipt.sql"),
            include_str!("../../../migrations/0045_employees_ask_to_fix_their_own_punches.sql"),
        ]
        .into_iter()
        .flat_map(grant_names)
        .collect();
        assert!(
            !migration_names.is_empty(),
            "沒有從 0008／0009／0015／0016／0017／0020 migration 抓到任何人資或自助工具"
        );

        let hr_modules = [
            ops::employees::ops(),
            ops::attendance::ops(),
            ops::insurance::ops(),
            ops::payroll::ops(),
            ops::hr_compliance::ops(),
            ops::selfservice::ops(),
        ];
        let declared_names: BTreeSet<&str> =
            hr_modules.iter().flatten().map(|op| op.name()).collect();

        let registry = Registry::new();
        let registry_names: BTreeSet<&str> = registry
            .iter()
            .map(|op| op.name())
            .filter(|name| declared_names.contains(name))
            .collect();
        assert_eq!(
            registry_names, declared_names,
            "人資模組宣告的操作沒有完整註冊進 Registry"
        );
        assert_eq!(
            registry_names, migration_names,
            "Registry 的人資／自助操作與 0008／0009／0015／0016／0017 migration 種下的 mcp_tool_grant 不一致"
        );
    }

    /// The nine grants in 0012 must match the seven structural master data
    /// operations plus creating a receivable and a payable, exactly.
    #[test]
    fn master_data_registry_matches_the_migration_grants() {
        use std::collections::BTreeSet;

        let migration_names =
            grant_names(include_str!("../../../migrations/0012_master_data_ops.sql"));
        let mut declared_names: BTreeSet<&str> =
            ops::masters::ops().iter().map(|op| op.name()).collect();
        declared_names.insert("create_ap_bill");
        declared_names.insert("create_ar_invoice");

        let registry_names: BTreeSet<&str> = Registry::new()
            .iter()
            .map(|op| op.name())
            .filter(|name| declared_names.contains(name))
            .collect();
        assert_eq!(
            registry_names, declared_names,
            "主檔、應收與應付建立操作沒有完整註冊進 Registry"
        );
        assert_eq!(
            registry_names, migration_names,
            "Registry 的主檔建立操作與 0012 migration 種下的 mcp_tool_grant 不一致"
        );
    }

    /// The two correction tools in 0014 must exist in both the registry and
    /// the default MCP policy.
    #[test]
    fn voucher_correction_registry_matches_the_migration_grants() {
        use std::collections::BTreeSet;

        let migration_names =
            grant_names(include_str!("../../../migrations/0014_voucher_correction.sql"));
        let declared_names = BTreeSet::from(["void_voucher", "reverse_voucher"]);
        let registry_names: BTreeSet<&str> = Registry::new()
            .iter()
            .map(|op| op.name())
            .filter(|name| declared_names.contains(name))
            .collect();

        assert_eq!(
            registry_names, declared_names,
            "作廢與沖銷操作沒有完整註冊進 Registry"
        );
        assert_eq!(
            registry_names, migration_names,
            "Registry 的作廢與沖銷操作和 0014 migration 的 MCP grant 不一致"
        );
    }

    /// Replay the permission INSERTs and UPDATEs in migration order, keeping
    /// only the last op_names set for each permission. This matches what
    /// PostgreSQL ends up with, and avoids reading an array that 0008 wrote
    /// and 0011 later overwrote as a duplicate grant.
    fn permission_assignments(migration: &str) -> Vec<(String, Vec<String>)> {
        fn array_at(statement: &str, start: usize) -> Vec<String> {
            let rest = &statement[start + 2..];
            let body = rest
                .split_once("}'")
                .expect("op_names 陣列字面沒有結尾")
                .0;
            body.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        }

        let mut assignments = Vec::new();
        for statement in migration.split(';') {
            if statement.contains("INSERT INTO permission (") {
                for (array_start, _) in statement.match_indices("'{") {
                    let before = &statement[..array_start];
                    let row_start = before
                        .rfind("('")
                        .expect("permission INSERT 的資料列沒有代碼");
                    let code = before[row_start + 2..]
                        .split_once('\'')
                        .expect("permission INSERT 的代碼沒有結尾")
                        .0;
                    assignments.push((code.to_string(), array_at(statement, array_start)));
                }
            } else if statement.contains("UPDATE permission")
                && statement.contains("SET op_names")
            {
                let array_start = statement
                    .find("'{")
                    .expect("permission UPDATE 沒有 op_names 陣列");
                let code = statement
                    .split_once("WHERE code = '")
                    .expect("permission UPDATE 沒有 WHERE code")
                    .1
                    .split_once('\'')
                    .expect("permission UPDATE 的 code 沒有結尾")
                    .0;
                assignments.push((code.to_string(), array_at(statement, array_start)));
            }
        }
        assignments
    }

    /// Every registry operation must be governed by exactly one permission.
    ///
    /// One missing means deny-by-default makes it permanently unusable; a
    /// duplicate means revoking one permission still leaves a way back in.
    /// This parses the migration text only and needs no database.
    #[test]
    fn every_registry_operation_has_exactly_one_permission() {
        use std::collections::{BTreeMap, BTreeSet};

        let migrations = [
            include_str!("../../../migrations/0006_reports_roles_security.sql"),
            include_str!("../../../migrations/0008_hr_employees_attendance_payroll.sql"),
            include_str!("../../../migrations/0009_staff_self_service.sql"),
            include_str!("../../../migrations/0011_permission_coverage.sql"),
            include_str!("../../../migrations/0012_master_data_ops.sql"),
            include_str!("../../../migrations/0014_voucher_correction.sql"),
            // 0015 onwards rewrite permissions earlier migrations wrote, so
            // they have to be replayed in order for the same reason
            // PostgreSQL applies them in order.
            include_str!("../../../migrations/0015_self_service_expense_claim.sql"),
            include_str!("../../../migrations/0016_attendance_punch.sql"),
            include_str!("../../../migrations/0017_self_service_mcp.sql"),
            include_str!("../../../migrations/0020_expense_claim_receipt.sql"),
            include_str!("../../../migrations/0023_system_administrator.sql"),
            include_str!("../../../migrations/0025_user_account_roster.sql"),
            include_str!("../../../migrations/0026_user_account_edit.sql"),
            include_str!("../../../migrations/0027_user_account_delete.sql"),
            include_str!("../../../migrations/0029_user_password_reset.sql"),
            include_str!("../../../migrations/0031_drop_einvoicing.sql"),
            include_str!("../../../migrations/0034_first_budget_version.sql"),
            include_str!("../../../migrations/0039_register_fixed_assets.sql"),
            include_str!("../../../migrations/0044_role_can_change_after_creation.sql"),
            include_str!("../../../migrations/0045_employees_ask_to_fix_their_own_punches.sql"),
            include_str!("../../../migrations/0046_external_identity_can_be_unlinked.sql"),
            include_str!("../../../migrations/0047_external_login_is_configured_on_screen.sql"),
        ];
        let mut permissions = BTreeMap::new();
        for migration in migrations {
            let assignments = permission_assignments(migration);
            assert!(
                !assignments.is_empty(),
                "migration 沒有解析到任何 permission op_names"
            );
            for (code, op_names) in assignments {
                permissions.insert(code, op_names);
            }
        }

        let empty: Vec<&String> = permissions
            .iter()
            .filter_map(|(code, op_names)| op_names.is_empty().then_some(code))
            .collect();
        assert!(empty.is_empty(), "仍有 permission 沒有對應操作：{empty:?}");

        let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (permission, op_names) in &permissions {
            for op_name in op_names {
                owners.entry(op_name.clone()).or_default().push(permission.clone());
            }
        }

        let registry: BTreeSet<String> =
            Registry::new().iter().map(|op| op.name().to_string()).collect();
        let mapped: BTreeSet<String> = owners.keys().cloned().collect();
        let missing: Vec<&String> = registry.difference(&mapped).collect();
        let unknown: Vec<&String> = mapped.difference(&registry).collect();
        let duplicated: Vec<(&String, &Vec<String>)> =
            owners.iter().filter(|(_, permission)| permission.len() != 1).collect();

        assert!(missing.is_empty(), "Registry 操作沒有 permission：{missing:?}");
        assert!(unknown.is_empty(), "permission 指到不存在的操作：{unknown:?}");
        assert!(
            duplicated.is_empty(),
            "操作出現在不只一條 permission：{duplicated:?}"
        );
        assert_eq!(registry.len(), owners.len(), "Registry 與 permission 的操作數量不同");
    }

    /// The UI channel must not be bound by AI tool grants; otherwise
    /// disabling post_voucher for AI would stop the accountant posting too.
    #[test]
    fn channels_are_distinguishable() {
        assert_eq!(Channel::Ui.as_db(), "ui");
        assert_eq!(Channel::Mcp.as_db(), "mcp");
        assert_ne!(Channel::Ui, Channel::Mcp);
    }

    #[test]
    fn not_permitted_error_names_the_role_and_operation() {
        let error = OpError::NotPermitted {
            tool: "將傳票過帳".into(),
            role: "出納".into(),
        };
        assert_eq!(error.code(), "not_permitted");
        assert_eq!(
            error.to_string(),
            "出納未獲授權執行「將傳票過帳」，請洽負責人於「角色與核准」開啟"
        );
    }

    /// Refusals name the operation the way the screens do. A reader who has
    /// never seen `update_company_setting` cannot act on a sentence built
    /// around it, and every operation already carries reviewed wording in its
    /// summary.
    #[test]
    fn every_operation_yields_a_refusal_label_without_its_registry_key() {
        for op in Registry::new().iter() {
            let label = op_label(op.summary());
            assert!(!label.is_empty(), "{} 的操作名是空的", op.name());
            assert!(
                !label.contains(op.name()),
                "{} 的操作名帶著註冊表鍵值：{label}",
                op.name()
            );
            assert!(
                !label.contains('_') && label.is_char_boundary(label.len()),
                "{} 的操作名看起來像識別字：{label}",
                op.name()
            );
        }
    }

    /// Running the system and owning the business are different jobs, so the
    /// two floors hold different operations. Moving anything across this line
    /// either hands the installer everyone's salary or hands the owner the
    /// permission matrix; both undo the point of 0023.
    #[test]
    fn business_confidentiality_and_system_control_are_different_floors() {
        use std::collections::BTreeSet;

        let names = |want: Risk| -> BTreeSet<&'static str> {
            Registry::new().iter().filter(|o| o.risk() == want).map(|o| o.name()).collect()
        };
        let boss = names(Risk::BossOnly);
        let admin = names(Risk::AdminOnly);

        assert_eq!(
            boss,
            BTreeSet::from([
                "create_budget_version",
                "list_payslips",
                "list_withholding_statements",
                "set_approval_rule",
                "set_budget",
            ]),
            "限負責人的操作集合變了"
        );
        assert_eq!(
            admin,
            BTreeSet::from([
                "change_user_role",
                "create_user_account",
                "delete_user_account",
                "get_oidc_setting",
                "issue_api_token",
                "list_user_accounts",
                "reset_user_password",
                "set_role_permission",
                "unlink_user_identity",
                "update_company_setting",
                "update_oidc_setting",
                "update_user_account",
            ]),
            "限系統管理員的操作集合變了"
        );
        assert!(boss.is_disjoint(&admin), "一個操作不能同時掛在兩道底線上");
        assert_ne!(ROLE_BOSS, ROLE_ADMIN);
    }

    #[test]
    fn needs_admin_error_names_the_role_and_operation() {
        let error = OpError::NeedsAdmin {
            tool: "變更一項公司設定".into(),
            role: "負責人".into(),
        };
        assert_eq!(error.code(), "needs_admin");
        assert_eq!(
            error.to_string(),
            "「變更一項公司設定」限系統管理員執行，目前身分為「負責人」"
        );
    }

    /// The owner must not keep the permission matrix after 0023: holding both
    /// would make the split cosmetic.
    #[test]
    fn the_owner_no_longer_holds_manage_access() {
        let migration = include_str!("../../../migrations/0023_system_administrator.sql");
        assert!(
            migration.contains("DELETE FROM role_permission")
                && migration.contains("permission_code = 'manage_access'"),
            "0023 必須收回 boss 的 manage_access"
        );
        assert!(
            migration.contains("('admin', 'manage_access', 'full'"),
            "0023 必須把 manage_access 給 admin"
        );
        // Balances are business information. The administrator reads the
        // chart's structure through list_accounts and nothing more.
        assert!(
            !migration.contains("'{list_accounts,get_company_settings,list_chart_of_accounts"),
            "list_chart_of_accounts 帶餘額，不能留在 view_masters"
        );
    }

    #[test]
    fn every_operation_has_a_chinese_summary_and_a_schema() {
        for op in Registry::new().iter() {
            assert!(!op.summary().is_empty(), "{} 沒有說明", op.name());
            assert!(
                op.summary().chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{} 的說明不是中文，MCP 客戶端會看不懂",
                op.name()
            );
            assert!(op.input_schema().is_object(), "{} 的 input schema 不是物件", op.name());
        }
    }

    /// A writing operation must never be marked read-only: that would tell
    /// the user the AI is only looking.
    #[test]
    fn mutating_operations_are_not_marked_read_only() {
        let r = Registry::new();
        for name in [
            "draft_voucher",
            "post_voucher",
            "void_voucher",
            "reverse_voucher",
            "create_account",
            "update_account",
            "create_employee",
            "update_employee",
            "create_bank_account",
            "create_dimension_value",
            "set_approval_rule",
            "create_ap_bill",
            "create_ar_invoice",
        ] {
            let op = r.get(name).expect(name);
            assert_ne!(op.risk(), Risk::ReadOnly, "{name} 會改資料，不能標成唯讀");
        }
        assert_eq!(r.get("post_voucher").unwrap().risk(), Risk::HighRisk);
        assert_eq!(r.get("void_voucher").unwrap().risk(), Risk::HighRisk);
        assert_eq!(r.get("reverse_voucher").unwrap().risk(), Risk::HighRisk);
        assert_eq!(r.get("draft_voucher").unwrap().risk(), Risk::NeedsConfirm);
    }

    #[test]
    fn money_formats_with_thousands_separators() {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        let d = |s: &str| Decimal::from_str(s).unwrap();
        assert_eq!(money(d("3842610")), "3,842,610");
        assert_eq!(money(d("842")), "842");
        assert_eq!(money(d("-156300")), "-156,300");
        assert_eq!(money(d("8420.4")), "8,420");
        assert_eq!(money(d("0")), "0");
    }
}
