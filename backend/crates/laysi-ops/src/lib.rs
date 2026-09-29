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
use chrono::{FixedOffset, NaiveDate};
use laysi_domain::Money;
use laysi_store::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

pub mod ops;

// Risk levels

/// Risk level of an operation, shown as four tiers on the MCP screen.
/// The role that owns the business: payroll, budget, approval thresholds.
pub const ROLE_BOSS: &str = "boss";
/// The role that owns the system: permissions, tokens, settings, logins.
pub const ROLE_ADMIN: &str = "admin";
/// The role every new hire starts on: their own attendance, leave and claims,
/// and nothing else. HR issues this one; every other role is the
/// administrator's to grant.
pub const ROLE_STAFF: &str = "staff";
/// Login name of the account the browser creates at first run. It stands for
/// the installation rather than a person, so it is the one login that is not
/// an email address and the one that keeps `email` empty: an address would
/// let an external identity resolve onto it by `users_by_email`, which is the
/// account that must stay reachable by password alone.
pub const ADMIN_USERNAME: &str = "admin";

/// Code of the `company_setting` row holding the business time zone.
pub use laysi_store::TIMEZONE_SETTING;

/// Business time zone of this install, in minutes east of UTC.
///
/// Single-tenant and effectively fixed once attendance exists, so it is read
/// once at start-up rather than on every request. `set_company_timezone`
/// writes the setting and then updates this, so REST and MCP both follow the
/// change without a restart. The initial value is the one the CHECK
/// constraints ship with, which is also what an install that predates the
/// setting is on.
static BUSINESS_OFFSET_MINUTES: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(laysi_domain::BUSINESS_UTC_OFFSET_HOURS * 60);

/// The business time zone as a chrono offset.
pub fn business_offset() -> FixedOffset {
    let minutes = BUSINESS_OFFSET_MINUTES.load(std::sync::atomic::Ordering::Relaxed);
    FixedOffset::east_opt(minutes * 60).unwrap_or_else(laysi_domain::default_business_offset)
}

/// Policy values that live in `company_setting` but are read on paths that
/// have no `Store` at hand: the cookie a login writes, the check a request
/// makes, the counter `/mcp` consults. Same shape as the business offset
/// above -- read once at start-up, republished by `update_company_setting`,
/// so REST and MCP both follow a change without a restart.
///
/// The fallbacks are what 0014 seeds, so an install that predates the
/// settings behaves as it did.
static SESSION_IDLE_MINUTES: AtomicI64 = AtomicI64::new(60);
static SESSION_MAX_HOURS: AtomicI64 = AtomicI64::new(168);
static MCP_RATE_LIMIT_PER_MINUTE: AtomicU32 = AtomicU32::new(60);

/// Codes of the `company_setting` rows the three atomics above mirror.
pub const SESSION_IDLE_SETTING: &str = "session_idle_minutes";
pub const SESSION_MAX_SETTING: &str = "session_max_hours";
pub const MCP_RATE_LIMIT_SETTING: &str = "mcp_rate_limit_per_minute";

/// How long `ai_audit_log` rows are kept. Read by the daily purge, which has
/// a `Store` at hand, so it stays a row rather than joining the atomics.
pub const AUDIT_KEEP_SETTING: &str = "audit_keep_days";

pub fn session_idle_minutes() -> i64 {
    SESSION_IDLE_MINUTES.load(Ordering::Relaxed)
}

pub fn session_max_hours() -> i64 {
    SESSION_MAX_HOURS.load(Ordering::Relaxed)
}

/// Calls per token per minute; 0 means unlimited.
pub fn mcp_rate_limit_per_minute() -> u32 {
    MCP_RATE_LIMIT_PER_MINUTE.load(Ordering::Relaxed)
}

/// Read the three settings and publish them to the process.
///
/// A row that is missing or does not parse leaves the current value alone: a
/// typo in one setting must not silently reset a session policy, and the
/// write path validates before storing anyway.
pub async fn load_policy_settings(store: &Store) {
    let Ok(settings) = store.company_settings().await else {
        return;
    };
    let value = |code: &str| {
        settings
            .iter()
            .find(|s| s.code == code)
            .map(|s| s.value.trim().to_string())
    };
    if let Some(v) = value(SESSION_IDLE_SETTING).and_then(|v| v.parse::<i64>().ok()) {
        if v > 0 {
            SESSION_IDLE_MINUTES.store(v, Ordering::Relaxed);
        }
    }
    if let Some(v) = value(SESSION_MAX_SETTING).and_then(|v| v.parse::<i64>().ok()) {
        if v > 0 {
            SESSION_MAX_HOURS.store(v, Ordering::Relaxed);
        }
    }
    if let Some(v) = value(MCP_RATE_LIMIT_SETTING).and_then(|v| v.parse::<u32>().ok()) {
        MCP_RATE_LIMIT_PER_MINUTE.store(v, Ordering::Relaxed);
    }
}

/// Publish a newly resolved business time zone to the whole process.
pub fn set_business_offset(offset: FixedOffset) {
    BUSINESS_OFFSET_MINUTES.store(
        offset.local_minus_utc() / 60,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Read the business time zone out of the settings table and publish it.
///
/// Called once at start-up. An unreadable or malformed value leaves the
/// default in place rather than failing the boot: the offset the CHECK
/// constraints hold is always a usable answer.
pub async fn load_business_offset(store: &Store) -> FixedOffset {
    if let Ok(Some(v)) = store.company_setting(TIMEZONE_SETTING).await {
        if let Some(offset) = laysi_domain::parse_business_offset(&v) {
            set_business_offset(offset);
            return offset;
        }
    }
    business_offset()
}

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

    /// Whether this tier lets `role` through at all.
    ///
    /// The two floors sit outside the permission matrix, so every place that
    /// decides what a role may reach — `dispatch`, the permission list on
    /// `/v1/auth/me`, the MCP tool list — has to apply them, and has to apply
    /// the same ones. A second copy of this match is a copy that drifts.
    pub fn allows_role(&self, role: &str) -> bool {
        match self {
            Risk::BossOnly => role == ROLE_BOSS,
            Risk::AdminOnly => role == ROLE_ADMIN,
            _ => true,
        }
    }

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
    /// Source address of the request, already validated as one. Recorded on
    /// every audit row: who and what without where answers "was this the
    /// account holder or someone holding their credential" only halfway.
    pub client_ip: Option<String>,
    pub today: NaiveDate,
    /// Business time zone in force for this call. Carried beside `today`
    /// because it is what `today` was derived from: an operation that turns
    /// an instant into a wall-clock time has to use the same one.
    pub offset: FixedOffset,
}

impl OpCtx {
    /// Object storage, or a clear error when it is not configured.
    pub fn objects(&self) -> Result<&laysi_store::ObjectStore, OpError> {
        self.objects.as_ref().ok_or_else(|| {
            OpError::BadInput("尚未接上物件儲存，文件功能無法使用；請洽系統管理員完成設定。".into())
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

    #[error("{reason}系統目前為唯讀，「{tool}」無法執行；查詢、報表與匯出不受影響。")]
    Unlicensed { tool: String, reason: String },

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
            OpError::Unlicensed { .. } => "unlicensed",
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
        Self {
            data,
            summary: summary.into(),
            voucher_no: None,
        }
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

    /// Whether this operation changes stored state.
    ///
    /// A different question from `Risk::ReadOnly`, which is about who may
    /// call it: a payslip listing carries the owner floor and still writes
    /// nothing. Only the licence floor asks this one, because an
    /// installation whose licence has lapsed has to stay readable — the
    /// books, the payslips and the exports are the customer's.
    ///
    /// The default follows the risk tier, so the handful of reads that carry
    /// an identity floor are the only overrides; `reads_are_not_writes`
    /// fails if a new one forgets.
    fn writes(&self) -> bool {
        !matches!(self.risk(), Risk::ReadOnly)
    }
}

pub(crate) fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({ "type": "object" }))
}

pub(crate) fn parse<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, OpError> {
    // Empty input means all defaults: MCP clients often send null, or omit
    // arguments entirely.
    let v = if v.is_null() { json!({}) } else { v };
    serde_path_to_error::deserialize(v).map_err(|e| {
        let path = e.path().to_string();
        let message = e.into_inner().to_string();
        tracing::debug!(%path, %message, "input parse failed");
        OpError::BadInput(describe_input_error(&path, &message))
    })
}

/// The text between `open` and the next `close`, if both are there.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    text.split_once(open)?
        .1
        .split_once(close)
        .map(|(inside, _)| inside)
}

/// A serde type name as the screens would say it.
fn type_zh(raw: &str) -> &'static str {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix("a ")
        .or_else(|| raw.strip_prefix("an "))
        .unwrap_or(raw);
    match raw.split([' ', ',']).next().unwrap_or(raw) {
        "string" | "str" | "&str" | "character" | "char" => "文字",
        "integer" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32"
        | "u64" | "u128" | "usize" => "整數",
        "float" | "floating" | "f32" | "f64" => "數字",
        "boolean" | "bool" => "是或否",
        "map" | "struct" => "物件",
        "sequence" | "seq" | "array" | "tuple" => "陣列",
        "null" | "unit" | "none" => "空值",
        other => {
            tracing::debug!(type_name = other, "type has no display name");
            "其他型別"
        }
    }
}

/// A parameter error in the language the rest of the product speaks.
///
/// serde's own messages are English and name Rust types (`expected i64`),
/// which is the wrong register for something an MCP client relays to a person
/// and a screen prints verbatim. The shapes handled here are the ones
/// `serde_json` and the field types in use actually produce; anything else
/// still says which field is wrong, because that is what the reader needs in
/// order to fix it.
fn describe_input_error(path: &str, message: &str) -> String {
    if let Some(name) = between(message, "missing field `", "`") {
        return format!("缺少必填欄位「{name}」");
    }
    if let Some(name) = between(message, "unknown field `", "`") {
        return format!("沒有「{name}」這個欄位");
    }

    // Reads as a noun in every sentence below, so a missing path does not
    // turn 「的內容不正確」 into 「的內容的內容不正確」.
    let subject = if path.is_empty() || path == "." {
        "輸入".to_string()
    } else {
        format!("欄位「{path}」")
    };

    if let Some((got, want)) = message
        .strip_prefix("invalid type: ")
        .and_then(|rest| rest.split_once(", expected "))
    {
        return format!(
            "{subject}的型別不符：預期{}，收到{}",
            type_zh(want),
            type_zh(got)
        );
    }
    if message.to_ascii_lowercase().contains("decimal") {
        return format!("{subject}不是有效的金額，請填數字，例如 10500 或 10500.00");
    }
    if message.contains("premature end of input") {
        return format!("{subject}不完整");
    }
    if message.contains("input contains invalid characters") {
        return format!("{subject}含有不接受的字元");
    }
    if message.contains("out of range") {
        return format!("{subject}超出可接受的範圍");
    }
    format!("{subject}的內容不正確")
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
const REDACTED_FIELDS: [&str; 4] = ["id_no", "client_secret", "account_no", "secret_key"];

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
    let label = summary
        .split(['：', '。', '，', '（'])
        .next()
        .unwrap_or(summary);
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
        self.ops
            .iter()
            .find(|o| o.name() == name)
            .map(|b| b.as_ref())
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
        let op = self
            .get(name)
            .ok_or_else(|| OpError::UnknownTool(name.to_string()))?;

        // 0. Licence floor: an installation whose licence has lapsed keeps
        //    every read, so the books stay readable, printable and
        //    exportable, and refuses everything that writes. Read from a
        //    process atomic rather than a table, so the common case costs no
        //    query; `ops::license::refresh` republishes it.
        //
        //    `activate_license` is the one write that has to stay reachable:
        //    it is the way out of this state.
        if op.writes() && name != "activate_license" && !ops::license::writes_allowed() {
            let reason = ops::license::lapse_summary(ctx).await;
            let label = op_label(op.summary()).to_string();
            self.audit(ctx, op, &input, "被拒絕：授權已失效", "blocked")
                .await;
            return Err(OpError::Unlicensed {
                tool: label,
                reason,
            });
        }

        // 1. Tool grant: a disabled tool is invisible to AI, even for reading.
        //    MCP only; people on screen are unaffected (see [`Channel`]).
        if ctx.channel == Channel::Mcp && ctx.store.tool_enabled(name).await? != Some(true) {
            self.audit(ctx, op, &input, "被拒絕：工具未開啟", "blocked")
                .await;
            return Err(OpError::ToolDisabled {
                tool: name.to_string(),
            });
        }

        // 2. Risk floor: two operations are pinned to one role each, outside
        //    the permission matrix. BossOnly guards business confidentiality
        //    (payroll, withholding, budget); AdminOnly guards the system's own
        //    controls (permissions, tokens, settings, logins). Neither role can
        //    be given the other's floor through `set_role_permission`, and no
        //    operation changes `user_account.role_code`, so the split holds.
        match op.risk() {
            Risk::BossOnly if ctx.actor_role != ROLE_BOSS => {
                self.audit(ctx, op, &input, "被拒絕：需負責人身分", "blocked")
                    .await;
                return Err(OpError::NeedsBoss {
                    tool: op_label(op.summary()).to_string(),
                    role: role_name(ctx).await,
                });
            }
            Risk::AdminOnly if ctx.actor_role != ROLE_ADMIN => {
                self.audit(ctx, op, &input, "被拒絕：需系統管理員身分", "blocked")
                    .await;
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
            return Err(OpError::NotPermitted {
                tool: label.to_string(),
                role,
            });
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
                        ctx.client_ip.as_deref(),
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
                ctx.client_ip.as_deref(),
            )
            .await
        {
            tracing::error!(tool = op.name(), error = %e, "audit log write failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parameter errors are read by a person, through the screen or through
    /// whatever the MCP client says next. None of them may arrive in English
    /// or name a Rust type.
    #[test]
    fn input_errors_are_written_in_the_language_of_the_screens() {
        #[derive(Debug, serde::Deserialize)]
        struct Sample {
            #[allow(dead_code)]
            name: String,
            #[serde(default)]
            #[allow(dead_code)]
            limit: Option<i64>,
        }

        let missing = parse::<Sample>(serde_json::json!({}))
            .unwrap_err()
            .to_string();
        assert_eq!(missing, "缺少必填欄位「name」");

        let wrong_type = parse::<Sample>(serde_json::json!({ "name": "甲", "limit": "很多" }))
            .unwrap_err()
            .to_string();
        assert_eq!(wrong_type, "欄位「limit」的型別不符：預期整數，收到文字");

        for message in [&missing, &wrong_type] {
            assert!(!message.is_ascii(), "訊息應為中文，收到「{message}」");
            assert!(
                !message.contains("i64"),
                "訊息不該出現 Rust 型別：{message}"
            );
        }
    }

    #[test]
    fn the_field_that_is_wrong_is_named_even_when_the_reason_is_not_recognised() {
        assert_eq!(
            describe_input_error("due_on", "input contains invalid characters"),
            "欄位「due_on」含有不接受的字元"
        );
        assert_eq!(
            describe_input_error("amount", "Invalid decimal: unknown character"),
            "欄位「amount」不是有效的金額，請填數字，例如 10500 或 10500.00"
        );
        assert_eq!(
            describe_input_error("lines[0].debit", "something new"),
            "欄位「lines[0].debit」的內容不正確"
        );
        // No path at all still has to produce a sentence, not an empty one.
        assert_eq!(
            describe_input_error("", "something new"),
            "輸入的內容不正確"
        );
    }

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
        assert_eq!(
            redacted["nested"][0]["content_base64"],
            serde_json::json!(marker)
        );

        // Short enough to survive the length rule, personal data for life.
        // The employee is still identified by emp_no.
        assert_eq!(redacted["id_no"], serde_json::json!("[已遮蔽]"));
        assert_eq!(redacted["emp_no"], serde_json::json!("E010"));
    }

    /// A dependant's id number sits one array down in `update_employee`'s
    /// input, and is kept out of the audit table all the same.
    #[test]
    fn a_dependants_id_number_is_redacted_inside_the_list() {
        let redacted = redact_for_audit(&serde_json::json!({
            "emp_no": "E010",
            "dependents": [{ "name": "王小華", "relation": "child", "id_no": "A123456789" }],
        }));
        assert_eq!(
            redacted["dependents"][0]["id_no"],
            serde_json::json!("[已遮蔽]")
        );
        assert_eq!(
            redacted["dependents"][0]["name"],
            serde_json::json!("王小華")
        );
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
            ops::license::ops(),
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
            ops::notices::ops(),
            ops::selfservice::ops(),
            ops::settings::ops(),
            ops::regulations::ops(),
            ops::calendar::ops(),
            ops::security::ops(),
            ops::backup::ops(),
            ops::webhook::ops(),
        ];
        let mut declared = 0;
        for module in modules {
            declared += module.len();
            for op in module {
                assert!(
                    r.get(op.name()).is_some(),
                    "{} 沒有註冊進 Registry",
                    op.name()
                );
            }
        }
        assert_eq!(before, declared, "註冊表裡有不屬於任何模組的操作");
    }

    /// The schema, as text. The assertions below read the seed data straight
    /// out of it, so a switch or a permission that the schema does not actually
    /// seed cannot pass by being described in a test.
    ///
    /// A later migration that seeds `permission` or `mcp_tool_grant` has to be
    /// concatenated after this one: the later rows win, the same way the later
    /// statement wins in the database.
    const SCHEMA: &str = concat!(
        include_str!("../../../migrations/0001_initial.sql"),
        include_str!("../../../migrations/0002_employee_records.sql"),
        include_str!("../../../migrations/0006_regulation_feed.sql"),
        include_str!("../../../migrations/0007_license.sql"),
        include_str!("../../../migrations/0008_update_voucher.sql"),
        include_str!("../../../migrations/0010_notice_push_test.sql"),
        include_str!("../../../migrations/0014_preview_leave.sql"),
    );

    /// The rows of one multi-row INSERT, one string per row, without the
    /// leading `('`.
    fn inserted_rows(table: &str) -> Vec<&'static str> {
        let head = format!("INSERT INTO {table} (");
        let rows: Vec<&str> = SCHEMA
            .split(head.as_str())
            .skip(1)
            .flat_map(|block| {
                block
                    .split_once(";\n")
                    .unwrap_or_else(|| panic!("{table} 的 INSERT 沒有分號結尾"))
                    .0
                    .lines()
            })
            .filter_map(|line| line.trim().strip_prefix("('"))
            .collect();
        assert!(!rows.is_empty(), "migrations 裡沒有 {table} 的 INSERT");
        rows
    }

    /// Whether the schema contains this literal.
    fn seeded(needle: &str) -> bool {
        SCHEMA.contains(needle)
    }

    /// Values of the first column of every row of one multi-row INSERT.
    fn first_column_of(table: &str) -> std::collections::BTreeSet<&'static str> {
        inserted_rows(table)
            .into_iter()
            .filter_map(|row| row.split_once('\'').map(|(name, _)| name))
            .collect()
    }

    /// Every operation needs exactly one MCP switch, and every switch needs an
    /// operation. One short and AI sees a tool it cannot call; one too many and
    /// there is no default policy for it.
    #[test]
    fn every_registry_operation_has_an_mcp_tool_grant() {
        use std::collections::BTreeSet;

        let seeded = first_column_of("mcp_tool_grant");
        let registry: BTreeSet<&str> = Registry::new().iter().map(|op| op.name()).collect();

        assert_eq!(
            registry.difference(&seeded).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "Registry 操作沒有 mcp_tool_grant"
        );
        assert_eq!(
            seeded.difference(&registry).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "mcp_tool_grant 指到不存在的操作"
        );
    }

    /// The seeded risk must be the operation's own risk. `mcp-role-matrix.ts`
    /// reads this column to assert that nothing carrying an identity floor is
    /// open to AI, so a row that understates its tier makes that check pass
    /// over the very tool it exists to catch.
    #[test]
    fn every_mcp_tool_grant_carries_the_operation_risk() {
        let registry = Registry::new();
        for row in inserted_rows("mcp_tool_grant") {
            let mut parts = row.split('\'');
            let name = parts.next().expect("grant 沒有工具名");
            let seeded = parts.nth(1).expect("grant 沒有風險分級");
            let op = registry.get(name).expect("grant 指到不存在的操作");
            assert_eq!(
                seeded,
                op.risk().as_db(),
                "{name} 的 mcp_tool_grant.risk 與操作本身不符"
            );
        }
    }

    /// Every registry operation must be governed by exactly one permission.
    ///
    /// One missing means deny-by-default makes it permanently unusable; a
    /// duplicate means revoking one permission still leaves a way back in.
    /// Reads the seeded rows out of 0001 and needs no database.
    #[test]
    fn every_registry_operation_has_exactly_one_permission() {
        use std::collections::{BTreeMap, BTreeSet};

        fn op_names_of(row: &str) -> &str {
            row.split_once("'{")
                .expect("permission 沒有 op_names 陣列")
                .1
                .split_once("}'")
                .expect("op_names 陣列字面沒有結尾")
                .0
        }

        let mut granted: BTreeMap<&str, &str> = BTreeMap::new();
        for row in inserted_rows("permission") {
            let code = row.split_once('\'').expect("permission 的代碼沒有結尾").0;
            granted.insert(code, op_names_of(row));
        }
        assert!(!granted.is_empty(), "permission 沒有解析到任何資料列");

        let mut owners: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (code, op_names) in &granted {
            assert!(!op_names.is_empty(), "permission 沒有對應操作：{code}");
            for op_name in op_names.split(',').map(str::trim) {
                owners.entry(op_name).or_default().push(code);
            }
        }

        let registry: BTreeSet<&str> = Registry::new().iter().map(|op| op.name()).collect();
        let mapped: BTreeSet<&str> = owners.keys().copied().collect();
        let duplicated: Vec<_> = owners
            .iter()
            .filter(|(_, holders)| holders.len() != 1)
            .collect();

        assert_eq!(
            registry.difference(&mapped).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "Registry 操作沒有 permission"
        );
        assert_eq!(
            mapped.difference(&registry).collect::<Vec<_>>(),
            Vec::<&&str>::new(),
            "permission 指到不存在的操作"
        );
        assert!(
            duplicated.is_empty(),
            "操作出現在不只一條 permission：{duplicated:?}"
        );
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

    /// The licence floor asks `writes()`, not `risk()`, and the default
    /// derives one from the other. A read that carries an identity floor has
    /// to say so, or a lapsed installation stops handing the customer their
    /// own payslips; a write that names itself like a read would slip through
    /// the floor entirely.
    ///
    /// Checked by the naming convention rather than by pinning seventy names:
    /// the mistake this catches is a new operation added without the
    /// override, and the name is what makes it recognisable.
    #[test]
    fn reads_are_not_writes_whatever_floor_they_carry() {
        const READ_VERBS: [&str; 7] = [
            "list_", "get_", "search_", "suggest_", "compute_", "match_", "preview_",
        ];
        // Named like a read, and does write: each one says why on its own
        // definition, and adding to this list is a decision rather than an
        // oversight.
        const EXCEPTIONS: [&str; 2] = ["match_bank_transaction", "preview_import"];

        for op in Registry::new().iter() {
            let name = op.name();
            let named_read = READ_VERBS.iter().any(|verb| name.starts_with(verb));
            if EXCEPTIONS.contains(&name) {
                assert!(op.writes(), "{name} 列在例外裡卻不寫入，該從清單移除");
                continue;
            }
            assert_eq!(
                op.writes(),
                !named_read,
                "{name} 的 writes() 與命名不符；讀取要覆寫 writes()，寫入要改名或列入例外"
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
            Registry::new()
                .iter()
                .filter(|o| o.risk() == want)
                .map(|o| o.name())
                .collect()
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
                "activate_license",
                "change_user_role",
                "confirm_backup_key",
                "create_user_account",
                "delete_regulation",
                "delete_user_account",
                "get_backup_setting",
                "get_license",
                "get_oidc_setting",
                "issue_api_token",
                "list_user_accounts",
                "refresh_regulations",
                "reset_backup_key",
                "reset_user_password",
                "save_regulation",
                "set_company_timezone",
                "set_mcp_tool_grant",
                "set_role_permission",
                "test_notice_push",
                "unlink_user_identity",
                "update_backup_setting",
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

    /// The owner must not hold the permission matrix: holding both it and the
    /// payroll floor would make the split between the two roles cosmetic.
    #[test]
    fn the_owner_no_longer_holds_manage_access() {
        assert!(
            seeded("('admin', 'manage_access'"),
            "manage_access 必須給 admin"
        );
        assert!(
            !seeded("('boss', 'manage_access'"),
            "boss 不得持有 manage_access"
        );
        // Balances are business information. The administrator reads the
        // chart's structure through list_accounts and nothing more.
        assert!(
            !seeded("'{list_accounts,get_company_settings,list_chart_of_accounts"),
            "list_chart_of_accounts 帶餘額，不能留在 view_masters"
        );
    }

    #[test]
    fn every_operation_has_a_chinese_summary_and_a_schema() {
        for op in Registry::new().iter() {
            assert!(!op.summary().is_empty(), "{} 沒有說明", op.name());
            assert!(
                op.summary()
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{} 的說明不是中文，MCP 客戶端會看不懂",
                op.name()
            );
            assert!(
                op.input_schema().is_object(),
                "{} 的 input schema 不是物件",
                op.name()
            );
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
