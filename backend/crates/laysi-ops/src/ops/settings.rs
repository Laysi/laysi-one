//! Company settings, chart of accounts, dimensions, role permissions and
//! approval flows.
//!
//! A setting and a regulation value are different kinds of thing. The company
//! name is a setting and the owner may change it. "VAT 5%, filed every two
//! months" looks like a setting but is fixed by the VAT Act, so it is not in
//! `company_setting`; [`GetCompanySettings`] derives that row from
//! `laysi-regulation` on every call and marks it as sourced from the ruleset
//! and not editable.
//!
//! The distinction only becomes visible when the law changes: a settings table
//! holds one current value with no effective date, so filings for earlier
//! periods would silently move to the new rate. The ruleset library has
//! effective dates and does not.
//!
//! `role_permission` is readable, editable and records who changed it. Every
//! call goes through [`Registry::dispatch`](crate::Registry::dispatch), which
//! looks up the `permission.op_names` granted to the role; no row, or an
//! operation outside the set, means denial.
//!
//! That lookup is deliberately not cached so [`SetRolePermission`] takes
//! effect on the next call. The two risk floors remain additional hard limits
//! that a permission row cannot lift: `Risk::BossOnly` for payroll and other
//! business confidentiality, `Risk::AdminOnly` for the system's own controls.
//! [`CreateUserAccount`] is on the admin floor, and it is the only way to
//! create a login without shell access; nothing anywhere changes an existing
//! account's role, so an administrator cannot promote themselves to owner.
//! `missing_ops` on [`ListRoles`] is post-deployment diagnostics — a correct
//! build keeps it empty, which the migration coverage test enforces.

use crate::ops::employees::{BASIS_MONTHLY, INSURED_EMPLOYEE};
use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use base64::Engine;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetCompanySettings),
        Box::new(UpdateCompanySetting),
        Box::new(SetCompanyTimezone),
        Box::new(ListChartOfAccounts),
        Box::new(ListDimensions),
        Box::new(ListRoles),
        Box::new(SetRolePermission),
        Box::new(SetMcpToolGrant),
        Box::new(CreateUserAccount),
        Box::new(UpdateUserAccount),
        Box::new(ChangeUserRole),
        Box::new(ResetUserPassword),
        Box::new(UnlinkUserIdentity),
        Box::new(DeleteUserAccount),
        Box::new(ListUserAccounts),
        Box::new(GetOidcSetting),
        Box::new(UpdateOidcSetting),
        Box::new(ListApprovalRules),
    ]
}

/// Display label for an account kind. Presentation only.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "asset" => "資產",
        "liability" => "負債",
        "equity" => "權益",
        "revenue" => "收益",
        "expense" => "費用",
        _ => "其他",
    }
}

/// Display label for a report group.
fn group_label(g: &str) -> &'static str {
    match g {
        "current_asset" => "流動資產",
        "non_current_asset" => "非流動資產",
        "current_liability" => "流動負債",
        "non_current_liability" => "非流動負債",
        "equity" => "權益",
        "revenue" => "營業收入",
        "cogs" => "銷貨成本",
        "operating_expense" => "營業費用",
        "other_income" => "營業外收入",
        "other_expense" => "營業外支出",
        _ => "未分組",
    }
}

/// Display label for a cash flow activity. `None` is a P&L account,
/// represented by the profit line.
fn section_label(s: Option<&str>) -> &'static str {
    match s {
        Some("cash") => "現金本身",
        Some("operating") => "營業活動",
        Some("investing") => "投資活動",
        Some("financing") => "籌資活動",
        None => "併入本期損益",
        _ => "未分類",
    }
}

// Company and accounting settings

struct GetCompanySettings;

#[async_trait]
impl Operation for GetCompanySettings {
    fn name(&self) -> &'static str {
        "get_company_settings"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀公司與帳務設定：公司名稱、統一編號、會計年度、本位幣、本期會計期間、單據編號規則與營業稅率。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let stored = ctx.store.company_settings().await?;
        let mut rows: Vec<Value> = stored
            .iter()
            .map(|s| {
                json!({
                    "code": s.code,
                    "label": s.label,
                    "value": s.value,
                    "note": s.note,
                    "editable": s.editable,
                    "options": setting_options(&s.code),
                    "source": "setting",
                    "updated_at": s.updated_at,
                    "updated_by": s.updated_by,
                })
            })
            .collect();

        // Derived rows. The current period follows the calendar and the
        // numbering rule follows the document numberer, so neither can be
        // stored without becoming a second copy that drifts. 0033 removed
        // their `company_setting` rows for that reason.
        let start_month = ctx.store.fiscal_year_start_month().await?;
        rows.push(json!({
            "code": "period",
            "label": "本期會計期間",
            "value": ctx.today.format("%Y / %m").to_string(),
            "note": format!("會計年度自 {start_month} 月起。"),
            "editable": false,
            "source": "derived",
        }));
        rows.push(json!({
            "code": "doc_number",
            "label": "單據編號規則",
            "value": format!("V{}-001 起，每月從 001 重新編號", ctx.today.format("%y%m")),
            "note": "",
            "editable": false,
            "source": "derived",
        }));

        // VAT is not a setting. It has an effective date and settings do not;
        // the difference only appears the day the law changes, by which time
        // the structure is too late to fix.
        let vat = laysi_regulation::resolve("vat", ctx.today)?;
        let rate = vat.decimal("rate")?;
        let months = vat.decimal("period_months")?;
        rows.push(json!({
            "code": "vat",
            "label": "營業稅",
            "value": format!(
                "一般稅額 {}%　{} 個月一期",
                crate::ops::tax::percent(rate),
                months.normalize()
            ),
            "note": format!("{}（{} 起適用）", vat.source_note, vat.effective_from),
            "editable": false,
            "source": "regulation",
            "ruleset": {
                "domain": vat.domain,
                "version": vat.version,
                "effective_from": vat.effective_from,
                "source_note": vat.source_note,
            },
        }));

        let summary = format!("讀取公司設定 · {} 項", rows.len());
        Ok(OpOutcome::new(
            json!({
                "settings": rows,
                "editable_count": stored.iter().filter(|s| s.editable).count(),
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateCompanySettingInput {
    /// Setting key, e.g. `company_name`.
    pub code: String,
    /// New value.
    pub value: String,
}

/// Check digit of a business administration number (統一編號).
///
/// Eight digits weighted 1,2,1,2,1,2,4,1; each product is reduced to its digit
/// sum and the total must be divisible by five. A seventh digit of 7 has
/// historically been allowed to satisfy the test either way, and numbers
/// issued under that rule are still in use, so both are accepted.
fn is_valid_tax_id(value: &str) -> bool {
    const WEIGHTS: [u32; 8] = [1, 2, 1, 2, 1, 2, 4, 1];
    let digits: Vec<u32> = value.chars().filter_map(|c| c.to_digit(10)).collect();
    if value.chars().count() != 8 || digits.len() != 8 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .zip(WEIGHTS)
        .map(|(d, w)| {
            let p = d * w;
            p / 10 + p % 10
        })
        .sum();
    sum % 5 == 0 || (digits[6] == 7 && (sum + 1) % 5 == 0)
}

/// The values a setting accepts, when it accepts a fixed few. The screen
/// offers exactly these and nothing else, so a policy that has two answers
/// cannot be stored as a third thing that reads like neither.
/// Months a fiscal year can start in. Offering them as options makes the
/// screen a picker, so the range no longer has to be written out beside it.
const FISCAL_MONTHS: [&str; 12] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12",
];

fn setting_options(code: &str) -> Vec<&'static str> {
    match code {
        "punch_correction_policy" => crate::ops::attendance::CORRECTION_POLICIES.to_vec(),
        "fiscal_year" => FISCAL_MONTHS.to_vec(),
        BRAND_DISPLAY_SETTING => BRAND_DISPLAYS.to_vec(),
        _ => Vec::new(),
    }
}

/// The company's own logo, and where the two places that show a company
/// identity render it.
pub const BRAND_LOGO_SETTING: &str = "brand_logo";
pub const BRAND_DISPLAY_SETTING: &str = "brand_display";

/// What the sign-in screen and the sidebar show. With no logo uploaded only
/// the first is reachable, so the reader of this value has to fall back to it
/// rather than draw an empty box.
pub const BRAND_DISPLAYS: [&str; 3] = ["名稱", "標誌", "標誌與名稱"];

/// The largest logo that may be stored. It travels inside `/v1/auth/me` and
/// `/v1/meta` on every load, so its size is page weight rather than a file
/// sitting in a bucket.
pub const MAX_BRAND_LOGO_BYTES: usize = 512 * 1024;

const BRAND_LOGO_TYPES: [&str; 4] = ["png", "jpeg", "webp", "svg+xml"];

/// `data:image/<type>;base64,<payload>` and nothing else.
///
/// The stored string goes straight into an `<img src>` on a page served to
/// signed-out visitors, so the prefix is checked here rather than trusted from
/// whatever produced it.
fn validate_brand_logo(value: &str) -> Result<(), OpError> {
    let bad = || OpError::BadInput("標誌要是 PNG、JPEG、WebP 或 SVG 圖檔".into());
    let (mime, payload) = value
        .strip_prefix("data:image/")
        .and_then(|rest| rest.split_once(";base64,"))
        .ok_or_else(bad)?;
    if !BRAND_LOGO_TYPES.contains(&mime) {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| bad())?;
    if bytes.len() > MAX_BRAND_LOGO_BYTES {
        return Err(OpError::BadInput(format!(
            "標誌檔案要小於 {} KB",
            MAX_BRAND_LOGO_BYTES / 1024
        )));
    }
    Ok(())
}

/// Settings for which no value is itself an answer.
///
/// The working day is the case: `attendance::late()` returns `Option<bool>`,
/// where `None` means nobody has said when work starts, which is different
/// from nobody being late. A company that has not decided has to be able to
/// say so, and to take it back after having set one.
const CLEARABLE_SETTINGS: [&str; 8] = [
    // Cleared means the company has no logo of its own, which is where every
    // installation starts and where one that drops its logo returns.
    BRAND_LOGO_SETTING,
    "work_start",
    "work_end",
    "break_start",
    "break_end",
    "late_grace_minutes",
    "flex_window_minutes",
    // Cleared means kept forever, which a company that has to produce records
    // years later has to be able to say.
    crate::AUDIT_KEEP_SETTING,
];

/// Settings whose shape is a fact about the outside world rather than a
/// preference. Anything not listed here is stored as typed.
fn validate_setting(code: &str, value: &str) -> Result<(), OpError> {
    match code {
        "tax_id" if !is_valid_tax_id(value) => Err(OpError::BadInput(
            "統一編號是 8 位數字，且檢查碼要相符".into(),
        )),
        "fiscal_year" if !matches!(value.parse::<u32>(), Ok(1..=12)) => {
            Err(OpError::BadInput("會計年度起始月份要填 1 到 12".into()))
        }
        "work_start" | "work_end" | "break_start" | "break_end" if !is_clock_time(value) => Err(
            OpError::BadInput(format!("「{value}」不是時刻，格式是 HH:MM，例如 09:00")),
        ),
        "late_grace_minutes" | "flex_window_minutes" if value.parse::<u32>().is_err() => {
            Err(OpError::BadInput(format!("「{value}」不是分鐘數")))
        }
        // Zero would log everyone out on their next click, and there would be
        // no way back in to undo it.
        crate::SESSION_IDLE_SETTING | crate::SESSION_MAX_SETTING
            if !matches!(value.parse::<i64>(), Ok(1..)) =>
        {
            Err(OpError::BadInput(format!("「{value}」不是大於 0 的數字")))
        }
        crate::MCP_RATE_LIMIT_SETTING if value.parse::<u32>().is_err() => Err(OpError::BadInput(
            format!("「{value}」不是次數，不限就填 0"),
        )),
        // Zero would delete the record of the call that set it to zero.
        crate::AUDIT_KEEP_SETTING if !matches!(value.parse::<i64>(), Ok(1..)) => Err(
            OpError::BadInput(format!("「{value}」不是大於 0 的天數，永久保存就留空")),
        ),
        BRAND_LOGO_SETTING => validate_brand_logo(value),
        BRAND_DISPLAY_SETTING if !BRAND_DISPLAYS.contains(&value) => {
            Err(OpError::BadInput(format!(
                "公司識別顯示方式填「{}」、「{}」或「{}」，收到「{value}」",
                BRAND_DISPLAYS[0], BRAND_DISPLAYS[1], BRAND_DISPLAYS[2]
            )))
        }
        "punch_correction_policy"
            if !crate::ops::attendance::CORRECTION_POLICIES.contains(&value) =>
        {
            Err(OpError::BadInput(format!(
                "補登申請的處理方式填「{}」或「{}」，收到「{value}」",
                crate::ops::attendance::CORRECTION_REVIEW,
                crate::ops::attendance::CORRECTION_AUTO,
            )))
        }
        _ => Ok(()),
    }
}

/// `HH:MM` on a 24-hour clock.
fn is_clock_time(value: &str) -> bool {
    let Some((h, m)) = value.split_once(':') else {
        return false;
    };
    if h.len() != 2 || m.len() != 2 || !h.chars().chain(m.chars()).all(|c| c.is_ascii_digit()) {
        return false;
    }
    matches!((h.parse::<u32>(), m.parse::<u32>()), (Ok(h), Ok(m)) if h < 24 && m < 60)
}

struct UpdateCompanySetting;

#[async_trait]
impl Operation for UpdateCompanySetting {
    fn name(&self) -> &'static str {
        "update_company_setting"
    }
    fn risk(&self) -> Risk {
        // Changing this affects the whole system, so it is owner only.
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "變更一項公司設定。只有可編輯的項目改得動：統一編號驗檢查碼，會計年度收 1 到 12 的起始月份，上下班與休息時間收 HH:MM，留空表示不判定遲到。法規決定的數值不在這裡變更。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UpdateCompanySettingInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UpdateCompanySettingInput = parse(input)?;
        let value = i.value.trim();
        if value.is_empty() && !CLEARABLE_SETTINGS.contains(&i.code.as_str()) {
            return Err(OpError::BadInput("設定值為必填".into()));
        }
        if !value.is_empty() {
            validate_setting(&i.code, value)?;
        }
        let s = ctx
            .store
            .update_company_setting(&i.code, value, &ctx.actor)
            .await?;
        // The three policy settings are also read on paths with no Store at
        // hand, so the process copy has to follow the row.
        if matches!(
            i.code.as_str(),
            crate::SESSION_IDLE_SETTING
                | crate::SESSION_MAX_SETTING
                | crate::MCP_RATE_LIMIT_SETTING
        ) {
            crate::load_policy_settings(&ctx.store).await;
        }
        // The logo is hundreds of kilobytes of base64. `redact_for_audit`
        // only reaches the input, so the summary has to name it instead of
        // repeating it.
        let summary = match (s.value.is_empty(), s.code.as_str()) {
            (true, _) => format!("改設定 · {} 清為尚未設定", s.label),
            (false, BRAND_LOGO_SETTING) => format!("改設定 · {} 已更換", s.label),
            (false, _) => format!("改設定 · {} 改成「{}」", s.label, s.value),
        };
        Ok(OpOutcome::new(json!({ "setting": s }), summary))
    }
}

// Business time zone

#[derive(Deserialize, JsonSchema)]
pub struct SetCompanyTimezoneInput {
    /// Hours and minutes east of UTC, as `+08:00` or `-05:30`.
    pub offset: String,
}

struct SetCompanyTimezone;

#[async_trait]
impl Operation for SetCompanyTimezone {
    fn name(&self) -> &'static str {
        "set_company_timezone"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "變更記帳時區。出勤日期與單據日期依這個時區判定，格式 +08:00。已經有打卡或補登申請時不能變更。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SetCompanyTimezoneInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SetCompanyTimezoneInput = parse(input)?;
        let raw = i.offset.trim();
        let Some(offset) = laysi_domain::parse_business_offset(raw) else {
            return Err(OpError::BadInput(format!(
                "時區格式為 +08:00 或 -05:30，收到「{raw}」"
            )));
        };
        let stored = laysi_domain::format_business_offset(offset);
        ctx.store.set_business_timezone(&stored, &ctx.actor).await?;
        crate::set_business_offset(offset);

        Ok(OpOutcome::new(
            json!({
                "offset": stored,
                "today": laysi_domain::business_today(offset),
            }),
            format!("改設定 · 記帳時區改成 UTC{stored}"),
        ))
    }
}

// Chart of accounts

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListChartOfAccountsInput {
    /// List only accounts actually used in this period. All by default.
    #[serde(default)]
    pub only_used: bool,
}

struct ListChartOfAccounts;

#[async_trait]
impl Operation for ListChartOfAccounts {
    fn name(&self) -> &'static str {
        "list_chart_of_accounts"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀會計科目表：代號、名稱、類別、正常餘額方向，以及所屬報表分組、現金流量活動別與目前餘額"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListChartOfAccountsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListChartOfAccountsInput = parse(input)?;
        // The same query the three statements use: the balance shown on the
        // chart of accounts is the balance on the statements.
        let rows = ctx.store.ledger_movements(None, ctx.today).await?;

        let accounts: Vec<Value> = rows
            .iter()
            .filter(|m| !i.only_used || m.line_count > 0)
            .map(|m| {
                json!({
                    "code": m.code,
                    "name": m.name,
                    "kind": m.kind,
                    "kind_label": kind_label(&m.kind),
                    "normal_side": m.normal_side,
                    "normal_side_label": if m.normal_side == "debit" { "借方" } else { "貸方" },
                    "is_postable": m.is_postable,
                    "is_active": m.is_active,
                    "report_group": m.report_group,
                    "report_group_label": group_label(&m.report_group),
                    "cash_flow_section": m.cash_flow_section,
                    "cash_flow_label": section_label(m.cash_flow_section.as_deref()),
                    "balance": m.balance().to_string(),
                    "line_count": m.line_count,
                })
            })
            .collect();

        let used = rows.iter().filter(|m| m.line_count > 0).count();
        let summary = format!("讀取科目表 · {} 個科目，其中 {used} 個有分錄", rows.len());
        Ok(OpOutcome::new(
            json!({ "accounts": accounts, "total": rows.len(), "used": used, "as_of": ctx.today }),
            summary,
        ))
    }
}

struct ListDimensions;

#[async_trait]
impl Operation for ListDimensions {
    fn name(&self) -> &'static str {
        "list_dimensions"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀部門清單。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let dims = ctx.store.dimensions().await?;
        let values: usize = dims.iter().map(|d| d.values.len()).sum();
        let summary = format!("讀取部門 · {values} 個");
        Ok(OpOutcome::new(json!({ "dimensions": dims }), summary))
    }
}

// Roles and permissions

struct ListRoles;

#[async_trait]
impl Operation for ListRoles {
    fn name(&self) -> &'static str {
        "list_roles"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀角色權限對照表：哪個角色有哪些權限、完整還是有限。每次呼叫都會即時強制檢查。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let roles = ctx.store.roles().await?;
        let permissions = ctx.store.permissions().await?;
        let grants = ctx.store.role_permissions().await?;

        // Operations that actually exist in the registry. A permission pointing
        // at a missing operation means the migration and the code have drifted;
        // the coverage test blocks such a build, but the API still reports it.
        let known: HashSet<&'static str> = crate::ops::all().iter().map(|o| o.name()).collect();
        // The two risk floors sit outside the permission matrix; both lists are
        // kept so the screen can label them separately. Granting a permission
        // does not lift either floor.
        let with_risk = |want: Risk| -> HashSet<&'static str> {
            crate::ops::all()
                .iter()
                .filter(|o| o.risk() == want)
                .map(|o| o.name())
                .collect()
        };
        let boss_only = with_risk(Risk::BossOnly);
        let admin_only = with_risk(Risk::AdminOnly);

        let perms: Vec<Value> = permissions
            .iter()
            .map(|p| {
                let missing: Vec<&String> = p
                    .op_names
                    .iter()
                    .filter(|n| !known.contains(n.as_str()))
                    .collect();
                let boss_ops: Vec<&String> = p
                    .op_names
                    .iter()
                    .filter(|n| boss_only.contains(n.as_str()))
                    .collect();
                let admin_ops: Vec<&String> = p
                    .op_names
                    .iter()
                    .filter(|n| admin_only.contains(n.as_str()))
                    .collect();
                json!({
                    "code": p.code,
                    "label": p.label,
                    "note": p.note,
                    "op_names": p.op_names,
                    // A migration pointing at a missing operation is listed as found.
                    "missing_ops": missing,
                    // These operations carry a risk floor on top of the role
                    // permission; granting the row does not open them.
                    "boss_only_ops": boss_ops,
                    "admin_only_ops": admin_ops,
                })
            })
            .collect();

        let cells: Vec<Value> = grants
            .iter()
            .map(|g| {
                json!({
                    "role_code": g.role_code,
                    "permission_code": g.permission_code,
                    "level": g.level,
                    "mark": if g.level == "full" { "●" } else { "○" },
                    "note": g.note,
                    "updated_at": g.updated_at,
                    "updated_by": g.updated_by,
                })
            })
            .collect();

        let summary = format!(
            "讀取權限對照表 · {} 個角色 × {} 條權限 · {} 格有授權",
            roles.len(),
            permissions.len(),
            cells.len()
        );
        Ok(OpOutcome::new(
            json!({
                "roles": roles,
                "permissions": perms,
                "grants": cells,
                "actor_role": ctx.actor_role,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct SetRolePermissionInput {
    /// Role code, e.g. `acct`.
    pub role_code: String,
    /// Permission code, e.g. `post_voucher`.
    pub permission_code: String,
    /// `full`, `limited`, or `none` to revoke this cell.
    pub level: String,
}

struct SetRolePermission;

#[async_trait]
impl Operation for SetRolePermission {
    fn name(&self) -> &'static str {
        "set_role_permission"
    }
    fn risk(&self) -> Risk {
        // Changing permissions and MCP settings is owner only.
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "設定或收回權限對照表的一格。權限層級用 full／limited／none；改動會從下一次操作呼叫起立即生效。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SetRolePermissionInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SetRolePermissionInput = parse(input)?;
        let level = match i.level.as_str() {
            "full" | "limited" => Some(i.level.as_str()),
            "none" => None,
            other => {
                return Err(OpError::BadInput(format!(
                    "level 只能是 full、limited 或 none，收到的是「{other}」"
                )))
            }
        };

        let cell = ctx
            .store
            .set_role_permission(&i.role_code, &i.permission_code, level, &ctx.actor)
            .await?;

        let what = match level {
            Some("full") => "完整",
            Some("limited") => "有限",
            _ => "收回",
        };
        let role_name = ctx
            .store
            .roles()
            .await?
            .into_iter()
            .find(|r| r.code == i.role_code)
            .map(|r| r.name)
            .unwrap_or_else(|| i.role_code.clone());
        let perm_label = ctx
            .store
            .permissions()
            .await?
            .into_iter()
            .find(|p| p.code == i.permission_code)
            .map(|p| p.label)
            .unwrap_or_else(|| i.permission_code.clone());
        let summary =
            format!("改權限 · {role_name}的「{perm_label}」設為{what}（下一次呼叫立即生效）");
        Ok(OpOutcome::new(json!({ "cell": cell }), summary))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct SetMcpToolGrantInput {
    /// Operation name, e.g. `post_voucher`.
    pub tool_name: String,
    /// Whether an AI client may call it.
    pub enabled: bool,
}

struct SetMcpToolGrant;

#[async_trait]
impl Operation for SetMcpToolGrant {
    fn name(&self) -> &'static str {
        "set_mcp_tool_grant"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "開啟或關閉一項 AI 工具。改動從下一次呼叫起立即生效。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SetMcpToolGrantInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SetMcpToolGrantInput = parse(input)?;

        // This switch decides what AI may call, so opening it would let an
        // administrator's MCP token open every other one. It is the single
        // name that has to stay closed to reach the rest.
        if i.enabled && i.tool_name == self.name() {
            return Err(OpError::BadInput(
                "這項工具無法對 AI 開啟。開啟之後 AI 就能自行開啟其餘工具。".into(),
            ));
        }

        let grant = ctx
            .store
            .set_tool_grant(&i.tool_name, i.enabled)
            .await?
            .ok_or_else(|| {
                OpError::BadInput(format!("沒有名為「{}」的工具，請確認名稱。", i.tool_name))
            })?;

        // The seeded note describes the tool in Chinese, so its first clause
        // names it the same way `dispatch` names an operation from its summary.
        let label = crate::op_label(&grant.note).to_string();
        let what = if grant.enabled { "開啟" } else { "關閉" };
        let summary = format!("{what} AI 工具 · {label}（下一次呼叫立即生效）");
        Ok(OpOutcome::new(json!({ "grant": grant }), summary))
    }
}

/// Resolve the employee an account points at, writing a roster entry when the
/// number names nobody yet.
///
/// An existing number links; anything else creates a roster entry carrying a
/// name and a hire date and nothing else. There is no flag to turn that off:
/// an account without an employee cannot punch in, take leave or claim an
/// expense, so the roster entry is part of what "create an account" means.
/// Linking is by number, not by name, because names repeat. The amounts stay
/// HR's to fill in, and payroll refuses an employee whose salary is still zero.
async fn link_employee(
    ctx: &OpCtx,
    employee_no: &str,
    name: &str,
) -> Result<laysi_store::Employee, OpError> {
    if !employee_no.is_empty() {
        if let Ok(found) = ctx.store.employee(employee_no).await {
            return Ok(found);
        }
    }

    let emp_no = if employee_no.is_empty() {
        ctx.store.next_employee_no().await?
    } else {
        employee_no.to_string()
    };

    let employee = ctx
        .store
        .create_employee(laysi_store::WriteEmployee {
            emp_no: &emp_no,
            name,
            dept_code: None,
            title: "",
            employment_type: "full_time",
            pay_basis: BASIS_MONTHLY,
            insured_as: INSURED_EMPLOYEE,
            birthday: None,
            hired_on: ctx.today,
            left_on: None,
            status: "active",
            base_salary: None,
            labor_insured_salary: None,
            health_insured_salary: None,
            pension_wage: None,
            id_no: "",
            bank_accounts: None,
            note: "",
            actor: &ctx.actor,
            keep_active_role: crate::ROLE_ADMIN,
        })
        .await?;
    Ok(employee)
}

/// Login name rules, shared with the web bootstrap endpoint so the two
/// entry points cannot drift.
const DISPLAY_NAME_MAX_CHARS: usize = 64;
const EMAIL_MAX_CHARS: usize = 254;

/// The address doubles as the login name, so it has to be one somebody can
/// actually be reached at, not just a string with an \`@\` in it: a login
/// nobody can receive a password reset or an OIDC match at is a login that
/// only \`laysi-admin\` can recover.
fn valid_login_email(value: &str) -> bool {
    let count = value.chars().count();
    if !(3..=EMAIL_MAX_CHARS).contains(&count) || value.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// What one new login needs, once the caller has settled who it belongs to.
pub(crate) struct NewLogin<'a> {
    /// Already trimmed and lowercased; it is also the login name.
    pub email: &'a str,
    pub display_name: &'a str,
    pub role_code: &'a str,
    pub employee_id: i32,
    /// No password at all, reachable only through the identity provider.
    pub external_login_only: bool,
}

/// The account as it was created, with the one-time password when there is one.
pub(crate) struct IssuedLogin {
    pub user: laysi_store::UserAccount,
    pub role_name: String,
    pub temporary_password: Option<String>,
}

/// Whether a first sign-in from the provider would resolve onto this address
/// rather than build a second account.
///
/// The three conditions are what makes a passwordless account reachable, so
/// the caller that offers one and the caller that guards against one ask the
/// same question.
pub(crate) async fn external_login_resolves(ctx: &OpCtx, email: &str) -> Result<bool, OpError> {
    let oidc = ctx.store.oidc_setting().await?;
    let domain = oidc.allowed_domain.trim().to_ascii_lowercase();
    Ok(oidc.enabled
        && !oidc.provision_role.trim().is_empty()
        && !domain.is_empty()
        && email.ends_with(&format!("@{domain}")))
}

/// Everything an entry point owes before it writes anything: the address is
/// one somebody can be reached at, the role exists, the name is free, and the
/// account will have a way in once it exists.
///
/// Separate from [`write_login`] because a caller may have to create a roster
/// entry in between, and a refusal after that would leave an employee record
/// nobody asked for.
pub(crate) async fn check_new_login(ctx: &OpCtx, req: &NewLogin<'_>) -> Result<String, OpError> {
    if !valid_login_email(req.email) {
        return Err(OpError::BadInput("電子郵件格式不正確".into()));
    }
    if req.display_name.is_empty() {
        return Err(OpError::BadInput("顯示名稱為必填".into()));
    }
    if req.display_name.chars().count() > DISPLAY_NAME_MAX_CHARS {
        return Err(OpError::BadInput("顯示名稱不可超過 64 個字元".into()));
    }

    let roles = ctx.store.roles().await?;
    let Some(role) = roles.iter().find(|r| r.code == req.role_code) else {
        let known: Vec<&str> = roles.iter().map(|r| r.code.as_str()).collect();
        return Err(OpError::BadInput(format!(
            "查無角色「{}」；可用的角色是 {}",
            req.role_code,
            known.join("、")
        )));
    };

    if ctx.store.user_by_username(req.email).await?.is_some() {
        return Err(OpError::BadInput(format!(
            "「{}」已經是登入帳號",
            req.email
        )));
    }

    // Refused before anything is written: an account with no password that the
    // provider cannot resolve onto is an account nobody can ever open, and no
    // operation here can give it one afterwards.
    if req.external_login_only {
        let oidc = ctx.store.oidc_setting().await?;
        if !oidc.enabled {
            return Err(OpError::BadInput(
                "請先設定外部登入，才能建立免密碼帳號".into(),
            ));
        }
        if oidc.provision_role.trim().is_empty() {
            return Err(OpError::BadInput(
                "外部登入目前設為不自動建立帳號，請改設一個角色".into(),
            ));
        }
        let domain = oidc.allowed_domain.trim().to_ascii_lowercase();
        if domain.is_empty() || !req.email.ends_with(&format!("@{domain}")) {
            return Err(OpError::BadInput(format!(
                "外部登入只接受 @{domain} 的地址，這個帳號要設定密碼"
            )));
        }
    } else if req.role_code != crate::ROLE_ADMIN {
        // The mirror image: with password sign-in turned off, a password is the
        // one thing this account will never be able to use. Only the system
        // administrator role is still let in that way.
        let oidc = ctx.store.oidc_setting().await?;
        if oidc.password_login_disabled {
            return Err(OpError::BadInput(format!(
                "本站已停用帳號密碼登入，請改用 @{} 的地址建立外部登入帳號",
                oidc.allowed_domain
            )));
        }
    }

    Ok(role.name.clone())
}

/// Write the account [`check_new_login`] has already cleared.
pub(crate) async fn write_login(
    ctx: &OpCtx,
    req: &NewLogin<'_>,
    role_name: String,
) -> Result<IssuedLogin, OpError> {
    // The creator never sets a lasting password. A generated one is returned
    // once and must be changed at first login.
    let temporary = (!req.external_login_only).then(laysi_store::password::temporary_password);
    let password_hash = match &temporary {
        Some(value) => Some(
            laysi_store::password::hash_password(value)
                .map_err(|e| OpError::BadInput(format!("無法建立密碼雜湊：{e}")))?,
        ),
        None => None,
    };

    let user = ctx
        .store
        .create_user(laysi_store::CreateUser {
            username: req.email,
            display_name: req.display_name,
            email: req.email,
            password_hash: password_hash.as_deref(),
            role_code: req.role_code,
            employee_id: Some(req.employee_id),
            status: "active",
            must_change_password: temporary.is_some(),
            created_by: &ctx.actor,
        })
        .await?;

    Ok(IssuedLogin {
        user,
        role_name,
        temporary_password: temporary,
    })
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateUserAccountInput {
    /// Also the login name: accounts sign in with their address, so there is
    /// no separate username to invent or remember.
    pub email: String,
    /// Name shown on screen and in the audit trail.
    pub display_name: String,
    /// Role code from the role table, e.g. `acct`.
    pub role_code: String,
    /// Employee number this login belongs to. An unknown number, or none at
    /// all, writes a roster entry; the number is allocated when empty.
    #[serde(default)]
    pub employee_no: String,
    /// Create the account with no password, to be reached only through the
    /// external identity provider. Accepted while external login is on, is
    /// set to create accounts, and the address is inside the allowed domain —
    /// the three conditions under which a first sign-in resolves onto this
    /// account instead of building a second one.
    #[serde(default)]
    pub external_login_only: bool,
}

struct CreateUserAccount;

#[async_trait]
impl Operation for CreateUserAccount {
    fn name(&self) -> &'static str {
        "create_user_account"
    }
    fn risk(&self) -> Risk {
        // Creating a login is a system control, not a business decision.
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "建立一個登入帳號並指定角色。密碼由系統產生，僅在本次回應出現一次，本人首次登入必須變更；改用外部登入時不產生密碼。員工編號找不到對應的人時會一併建立員工主檔，留空則自動配號。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateUserAccountInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CreateUserAccountInput = parse(input)?;

        let email = i.email.trim().to_ascii_lowercase();
        let mut login = NewLogin {
            email: &email,
            display_name: i.display_name.trim(),
            role_code: i.role_code.trim(),
            employee_id: 0,
            external_login_only: i.external_login_only,
        };
        // Checked before the roster entry: a refusal afterwards would leave an
        // employee nobody asked for.
        let role_name = check_new_login(ctx, &login).await?;
        let employee = link_employee(ctx, i.employee_no.trim(), login.display_name).await?;
        login.employee_id = employee.id;
        let issued = write_login(ctx, &login, role_name).await?;
        let user = issued.user;
        let temporary = issued.temporary_password;

        let summary = format!(
            "建立帳號 · {}（{}）角色{}，連結員工 {}{}",
            user.username,
            user.display_name,
            issued.role_name,
            employee.emp_no,
            if temporary.is_some() {
                ""
            } else {
                "，外部登入"
            }
        );
        Ok(OpOutcome::new(
            json!({
                "user": {
                    "id": user.id,
                    "username": user.username,
                    "display_name": user.display_name,
                    "email": user.email,
                    "role_code": user.role_code,
                    "role_name": issued.role_name,
                    "employee_no": employee.emp_no,
                    "employee_name": employee.name,
                },
                // Shown once and never retrievable: only the hash is stored.
                // Null when the account has no password at all.
                "temporary_password": temporary,
                "must_change_password": temporary.is_some(),
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateUserAccountInput {
    /// Which account to edit. Login names are unique and never change.
    pub username: String,
    /// Leave the field out to keep the current value.
    #[serde(default)]
    pub display_name: Option<String>,
    /// An empty string clears the address.
    #[serde(default)]
    pub email: Option<String>,
    /// `active` or `suspended`. A suspended account cannot sign in.
    #[serde(default)]
    pub status: Option<String>,
    /// Employee number this login belongs to. An unknown number writes a
    /// roster entry; an empty string removes the link; leaving the field out
    /// keeps the current one.
    #[serde(default)]
    pub employee_no: Option<String>,
}

struct UpdateUserAccount;

#[async_trait]
impl Operation for UpdateUserAccount {
    fn name(&self) -> &'static str {
        "update_user_account"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "改一個登入帳號的顯示名稱、電子郵件、啟用狀態與連結的員工。員工編號找不到對應的人時會一併建立員工主檔。角色請用變更角色的操作，密碼只能由本人變更。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UpdateUserAccountInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UpdateUserAccountInput = parse(input)?;

        let username = i.username.trim();
        let Some(current) = ctx.store.user_by_username(username).await? else {
            return Err(OpError::BadInput(format!("查無登入名稱「{username}」")));
        };

        let display_name = match &i.display_name {
            Some(name) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err(OpError::BadInput("顯示名稱為必填".into()));
                }
                if name.chars().count() > DISPLAY_NAME_MAX_CHARS {
                    return Err(OpError::BadInput("顯示名稱不可超過 64 個字元".into()));
                }
                Some(name)
            }
            None => None,
        };

        let email = match &i.email {
            Some(email) => {
                let email = email.trim();
                if !email.is_empty()
                    && (email.chars().count() > EMAIL_MAX_CHARS
                        || !email.contains('@')
                        || email.chars().any(char::is_whitespace))
                {
                    return Err(OpError::BadInput("電子郵件格式不正確；此欄可以留空".into()));
                }
                // An address on this account would let an external identity
                // resolve onto it by `users_by_email`, and it is the account
                // that has to stay reachable when the identity provider is
                // not.
                if !email.is_empty() && current.username == crate::ADMIN_USERNAME {
                    return Err(OpError::BadInput("系統管理員帳號不能設定電子郵件".into()));
                }
                Some(email)
            }
            None => None,
        };

        let status = match &i.status {
            Some(status) => match status.trim() {
                s @ ("active" | "suspended") => Some(s),
                other => {
                    return Err(OpError::BadInput(format!(
                        "狀態只能是 active 或 suspended，收到的是「{other}」"
                    )))
                }
            },
            None => None,
        };

        // Locking out the last administrator would leave nobody able to open
        // the accounts screen, and no operation restores a role. Refusing here
        // is the only guard, since dispatch checks the caller and not the
        // target.
        if status == Some("suspended") && current.role_code == crate::ROLE_ADMIN {
            let others = ctx
                .store
                .users()
                .await?
                .into_iter()
                .filter(|u| {
                    u.role_code == crate::ROLE_ADMIN
                        && u.status == "active"
                        && u.username != current.username
                })
                .count();
            if others == 0 {
                return Err(OpError::BadInput(
                    "至少要保留一個啟用中的系統管理員帳號，無法停用最後一個".into(),
                ));
            }
        }

        // `Some(None)` clears the link, `None` leaves it as it is.
        let employee = match i.employee_no.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(no) => Some(Some(
                link_employee(ctx, no, display_name.unwrap_or(&current.display_name)).await?,
            )),
        };

        let updated = ctx
            .store
            .update_user(laysi_store::UpdateUser {
                username: &current.username,
                display_name,
                email,
                status,
                employee_id: employee.as_ref().map(|e| e.as_ref().map(|e| e.id)),
            })
            .await?
            .ok_or_else(|| OpError::BadInput(format!("查無登入名稱「{username}」")))?;

        let roles = ctx.store.roles().await?;
        let role_name = roles
            .iter()
            .find(|r| r.code == updated.role_code)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| updated.role_code.clone());

        let mut changed: Vec<String> = Vec::new();
        if display_name.is_some() {
            changed.push(format!("顯示名稱改為 {}", updated.display_name));
        }
        if email.is_some() {
            changed.push(match updated.email.as_str() {
                "" => "清除電子郵件".to_string(),
                set => format!("電子郵件改為 {set}"),
            });
        }
        if let Some(status) = status {
            changed.push(if status == "active" {
                "啟用".into()
            } else {
                "停用".to_string()
            });
        }
        if let Some(employee) = &employee {
            changed.push(match employee {
                Some(e) => format!("連結員工 {}", e.emp_no),
                None => "解除員工連結".to_string(),
            });
        }
        if changed.is_empty() {
            changed.push("沒有欄位變更".to_string());
        }

        let summary = format!(
            "改帳號 · {}（{}）{}",
            updated.username,
            role_name,
            changed.join("、")
        );
        Ok(OpOutcome::new(
            json!({
                "user": {
                    "id": updated.id,
                    "username": updated.username,
                    "display_name": updated.display_name,
                    "email": updated.email,
                    "role_code": updated.role_code,
                    "role_name": role_name,
                    "status": updated.status,
                },
                "changed": changed,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct ChangeUserRoleInput {
    /// Login name of the account to move.
    pub username: String,
    /// Role code from the role table, e.g. `acct`.
    pub role_code: String,
    /// Must be `true`. The account's sessions and MCP tokens end at once.
    #[serde(default)]
    pub confirm: bool,
}

struct ChangeUserRole;

#[async_trait]
impl Operation for ChangeUserRole {
    fn name(&self) -> &'static str {
        "change_user_role"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "變更一個登入帳號的角色。該帳號目前的登入工作階段與本人核發的 MCP Token 立刻失效，本人重新登入後才有新的權限。不能變更自己的角色。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ChangeUserRoleInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ChangeUserRoleInput = parse(input)?;
        if !i.confirm {
            return Err(OpError::BadInput(
                "變更角色會讓該帳號登出並失去現有 Token，請帶入 confirm".into(),
            ));
        }

        let username = i.username.trim();
        let Some(current) = ctx.store.user_by_username(username).await? else {
            return Err(OpError::BadInput(format!("查無登入名稱「{username}」")));
        };

        // The only escalation this operation could add is upward on the
        // caller's own account: everything else an administrator can already
        // do by creating an account with that role. `actor` is a display name
        // and collides, so the login name is the key; an MCP token has none
        // and therefore cannot be told apart from the account it is editing.
        let Some(caller) = ctx.username.as_deref() else {
            return Err(OpError::BadInput("請以本人的登入帳號進行角色變更".into()));
        };
        if caller == current.username {
            return Err(OpError::BadInput(
                "不能變更自己的角色，請由另一個系統管理員帳號變更".into(),
            ));
        }

        let role_code = i.role_code.trim();
        let roles = ctx.store.roles().await?;
        let Some(role) = roles.iter().find(|r| r.code == role_code) else {
            let known: Vec<&str> = roles.iter().map(|r| r.code.as_str()).collect();
            return Err(OpError::BadInput(format!(
                "查無角色「{role_code}」；可用的角色是 {}",
                known.join("、")
            )));
        };
        if current.role_code == role.code {
            return Err(OpError::BadInput(format!(
                "「{}」已經是{}",
                current.username, role.name
            )));
        }

        // Same guard as suspending the last administrator: nobody left who can
        // open the accounts screen means nobody left who can undo it.
        if current.role_code == crate::ROLE_ADMIN {
            let others = ctx
                .store
                .users()
                .await?
                .into_iter()
                .filter(|u| {
                    u.role_code == crate::ROLE_ADMIN
                        && u.status == "active"
                        && u.username != current.username
                })
                .count();
            if others == 0 {
                return Err(OpError::BadInput(
                    "至少要保留一個啟用中的系統管理員帳號，請先建立另一個管理員帳號".into(),
                ));
            }
        }

        let was = roles
            .iter()
            .find(|r| r.code == current.role_code)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| current.role_code.clone());

        let changed = ctx
            .store
            .change_user_role(laysi_store::ChangeUserRole {
                username: &current.username,
                role_code: &role.code,
                actor: &ctx.actor,
            })
            .await?
            .ok_or_else(|| OpError::BadInput(format!("查無登入名稱「{username}」")))?;

        let summary = format!(
            "變更角色 · {}（{}）由{}改為{}，登出 {} 個工作階段，撤銷 {} 組 Token",
            changed.user.username,
            changed.user.display_name,
            was,
            role.name,
            changed.sessions,
            changed.tokens
        );
        Ok(OpOutcome::new(
            json!({
                "user": {
                    "id": changed.user.id,
                    "username": changed.user.username,
                    "display_name": changed.user.display_name,
                    "role_code": changed.user.role_code,
                    "role_name": role.name,
                    "previous_role_name": was,
                    "status": changed.user.status,
                },
                "revoked_sessions": changed.sessions,
                "revoked_tokens": changed.tokens,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct ResetUserPasswordInput {
    /// Login name of the account that needs a new password.
    pub username: String,
    /// Must be `true`. The current password stops working at once.
    #[serde(default)]
    pub confirm: bool,
}

struct ResetUserPassword;

#[async_trait]
impl Operation for ResetUserPassword {
    fn name(&self) -> &'static str {
        "reset_user_password"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "為忘記密碼的人重新產生一組臨時密碼。舊密碼與該帳號目前的登入工作階段立刻失效，新密碼僅在本次回應出現一次，本人首次登入必須變更。只用外部帳號登入的帳號不適用。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ResetUserPasswordInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ResetUserPasswordInput = parse(input)?;
        if !i.confirm {
            return Err(OpError::BadInput(
                "重設密碼會讓舊密碼立刻失效，請帶入 confirm".into(),
            ));
        }

        let username = i.username.trim();
        let Some(user) = ctx.store.user_by_username(username).await? else {
            return Err(OpError::BadInput(format!("查無登入名稱「{username}」")));
        };
        // Giving a password to an account that has none would turn an
        // identity-provider login into a password login, which is the one
        // thing clearing the hash was for.
        if user.password_hash.is_none() {
            return Err(OpError::BadInput(format!(
                "「{username}」只用外部帳號登入，密碼請在身分提供者重設"
            )));
        }

        let temporary = laysi_store::password::temporary_password();
        let password_hash = laysi_store::password::hash_password(&temporary)
            .map_err(|e| OpError::BadInput(format!("無法建立密碼雜湊：{e}")))?;
        let updated = ctx
            .store
            .set_password_hash(user.id, Some(&password_hash), true)
            .await?;
        let revoked = ctx.store.revoke_user_sessions(user.id, &ctx.actor).await?;

        let roles = ctx.store.roles().await?;
        let role_name = roles
            .iter()
            .find(|r| r.code == updated.role_code)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| updated.role_code.clone());

        let summary = format!(
            "重設密碼 · {}（{}）舊密碼失效，登出 {revoked} 個工作階段",
            updated.username, role_name
        );
        Ok(OpOutcome::new(
            json!({
                "user": {
                    "id": updated.id,
                    "username": updated.username,
                    "display_name": updated.display_name,
                    "role_code": updated.role_code,
                    "role_name": role_name,
                },
                // Shown once and never retrievable: only the hash is stored.
                "temporary_password": temporary,
                "must_change_password": true,
                "revoked_sessions": revoked,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct UnlinkUserIdentityInput {
    /// Login name of the account to unlink.
    pub username: String,
    /// Must be `true`. The account's sessions end at once.
    #[serde(default)]
    pub confirm: bool,
}

struct UnlinkUserIdentity;

#[async_trait]
impl Operation for UnlinkUserIdentity {
    fn name(&self) -> &'static str {
        "unlink_user_identity"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "解除一個登入帳號的外部帳號連結。該帳號的登入工作階段立刻失效，之後只能用密碼登入。沒有密碼的帳號無法解除連結，請改為停用。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UnlinkUserIdentityInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UnlinkUserIdentityInput = parse(input)?;
        if !i.confirm {
            return Err(OpError::BadInput(
                "解除連結會讓該帳號登出，請帶入 confirm".into(),
            ));
        }

        let username = i.username.trim();
        let Some(target) = ctx.store.user_by_username(username).await? else {
            return Err(OpError::BadInput(format!("查無登入名稱「{username}」")));
        };
        let identities = ctx.store.user_identities(target.id).await?;
        if identities.is_empty() {
            return Err(OpError::BadInput(format!("「{username}」沒有連結外部帳號")));
        }
        // Unlinking an account with no password hash leaves a login nobody
        // can use, and no operation on this screen can give it one back.
        if target.password_hash.is_none() {
            return Err(OpError::BadInput(format!(
                "「{username}」只能用外部帳號登入，解除後沒有人能登入；請改為停用這個帳號"
            )));
        }

        let removed = ctx.store.unlink_identities(target.id, &ctx.actor).await?;
        let labels: Vec<String> = identities
            .iter()
            .map(|identity| laysi_store::provider_label(&identity.provider))
            .collect();
        let summary = format!(
            "解除連結 · {}（{}）不再以{}登入，登出 {} 個工作階段",
            target.username,
            target.display_name,
            labels.join("、"),
            removed.revoked_sessions
        );
        Ok(OpOutcome::new(
            json!({
                "user": {
                    "username": target.username,
                    "display_name": target.display_name,
                },
                "removed": removed.removed,
                "revoked_sessions": removed.revoked_sessions,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteUserAccountInput {
    /// Login name of the account to remove.
    pub username: String,
    /// Must be `true`. Removing an account cannot be undone.
    #[serde(default)]
    pub confirm: bool,
}

struct DeleteUserAccount;

#[async_trait]
impl Operation for DeleteUserAccount {
    fn name(&self) -> &'static str {
        "delete_user_account"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "刪除一個登入帳號及其登入工作階段。無法復原，稽核紀錄上的既有紀錄不受影響。連著外部帳號的要先解除連結；最後一個帳號與最後一個啟用中的系統管理員不能刪除。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<DeleteUserAccountInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DeleteUserAccountInput = parse(input)?;
        if !i.confirm {
            return Err(OpError::BadInput("刪除帳號無法復原，請帶入 confirm".into()));
        }

        let username = i.username.trim();
        let accounts = ctx.store.users().await?;
        let Some(target) = accounts.iter().find(|u| u.username == username) else {
            return Err(OpError::BadInput(format!("查無登入名稱「{username}」")));
        };

        // The web bootstrap endpoint does not reopen once an installation has
        // been claimed, so an empty user_account table can only be repaired
        // from a shell.
        if accounts.len() == 1 {
            return Err(OpError::BadInput(
                "這是最後一個帳號，刪除後將無人能登入；請先建立另一個帳號".into(),
            ));
        }
        if target.role_code == crate::ROLE_ADMIN {
            let others = accounts
                .iter()
                .filter(|u| {
                    u.role_code == crate::ROLE_ADMIN
                        && u.status == "active"
                        && u.username != target.username
                })
                .count();
            if others == 0 {
                return Err(OpError::BadInput(
                    "至少要保留一個啟用中的系統管理員帳號；請先建立另一個管理員帳號".into(),
                ));
            }
        }

        // Where an external directory provisions accounts, the next
        // sign-in builds a deleted one again. Breaking the link is the
        // deliberate step that makes the deletion mean something.
        if !ctx.store.user_identities(target.id).await?.is_empty() {
            return Err(OpError::BadInput(format!(
                "「{username}」連著外部帳號，請先解除連結再刪除；只是要擋住登入的話，停用就夠了"
            )));
        }

        let removed = ctx
            .store
            .delete_user(username)
            .await?
            .ok_or_else(|| OpError::BadInput(format!("查無登入名稱「{username}」")))?;

        let roles = ctx.store.roles().await?;
        let role_name = roles
            .iter()
            .find(|r| r.code == removed.role_code)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| removed.role_code.clone());

        let summary = format!("刪帳號 · {}（{}）已移除", removed.username, role_name);
        Ok(OpOutcome::new(
            json!({
                "user": {
                    "username": removed.username,
                    "display_name": removed.display_name,
                    "role_code": removed.role_code,
                    "role_name": role_name,
                },
                "remaining": accounts.len() - 1,
            }),
            summary,
        ))
    }
}

struct ListUserAccounts;

#[async_trait]
impl Operation for ListUserAccounts {
    fn name(&self) -> &'static str {
        "list_user_accounts"
    }
    fn risk(&self) -> Risk {
        // Who can sign in is the same subject as creating a login, so it sits
        // on the same floor. The roster carries no amount and no payslip.
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "讀登入帳號清單：登入名稱、角色、連結的員工、外部帳號、狀態與最後登入時間。密碼與密碼雜湊不在回應內，也無法取回。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let accounts = ctx.store.user_account_roster().await?;
        let pending = accounts.iter().filter(|a| a.must_change_password).count();
        let summary = format!(
            "讀取登入帳號 · {} 個，其中 {pending} 個尚未變更初始密碼",
            accounts.len()
        );
        Ok(OpOutcome::new(json!({ "accounts": accounts }), summary))
    }
}

/// Settings the sign-in screen and the server both read. The client secret is
/// not among them: it goes in, it never comes back.
struct GetOidcSetting;

#[async_trait]
impl Operation for GetOidcSetting {
    fn name(&self) -> &'static str {
        "get_oidc_setting"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "讀外部登入設定：供應商、用戶端 ID、公司電子郵件網域、自動建立帳號的角色、是否停用帳號密碼登入與上次驗證結果。用戶端密鑰不在回應內，設定後也無法取回。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let setting = ctx.store.oidc_setting().await?;
        let roles = ctx.store.roles().await?;
        let role_name = roles
            .iter()
            .find(|role| role.code == setting.provision_role)
            .map(|role| role.name.clone());
        let summary = if setting.enabled {
            format!(
                "讀外部登入設定 · 已啟用{}",
                if setting.verify_error.is_empty() {
                    ""
                } else {
                    "，但設定有問題"
                }
            )
        } else {
            "讀外部登入設定 · 未啟用".to_string()
        };
        Ok(OpOutcome::new(
            json!({
                "enabled": setting.enabled,
                "issuer": setting.issuer,
                "provider_label": laysi_store::provider_label(&setting.issuer),
                "client_id": setting.client_id,
                "has_client_secret": !setting.client_secret.is_empty(),
                "redirect_url": setting.redirect_url,
                "allowed_domain": setting.allowed_domain,
                "provision_role": setting.provision_role,
                "provision_role_name": role_name,
                "password_login_disabled": setting.password_login_disabled,
                "updated_at": setting.updated_at,
                "updated_by": setting.updated_by,
                "verified_at": setting.verified_at,
                "verify_error": setting.verify_error,
                "roles": roles,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateOidcSettingInput {
    /// Whether the sign-in button appears at all. Turning it on needs the
    /// issuer, the client id, the redirect URL and a stored secret.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The IdP's issuer, e.g. `https://accounts.google.com` for Google
    /// Workspace and Cloud Identity.
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    /// Write only. Leave the field out to keep the stored secret; an empty
    /// string clears it.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Where the IdP returns to, ending in `/v1/auth/oauth/callback`. It has
    /// to be registered with the IdP exactly as written here.
    #[serde(default)]
    pub redirect_url: Option<String>,
    /// The domain accounts must belong to. An empty string accepts any
    /// account the IdP authenticates.
    #[serde(default)]
    pub allowed_domain: Option<String>,
    /// Role for accounts created on a first sign-in from that domain. An
    /// empty string creates none.
    #[serde(default)]
    pub provision_role: Option<String>,
    /// Turns off the password form for every role except the system
    /// administrator. Only settable while `enabled` is (or is becoming) true.
    #[serde(default)]
    pub password_login_disabled: Option<bool>,
}

struct UpdateOidcSetting;

/// The only path the server answers a sign-in on.
const OAUTH_CALLBACK_PATH: &str = "/v1/auth/oauth/callback";

fn is_web_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

/// Domains compare without case, and a leading `@` is what people type.
fn normalize_login_domain(value: &str) -> String {
    value.trim().trim_start_matches('@').to_ascii_lowercase()
}

#[async_trait]
impl Operation for UpdateOidcSetting {
    fn name(&self) -> &'static str {
        "update_oidc_setting"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "設定外部登入：供應商、用戶端 ID 與密鑰、轉址網址、公司電子郵件網域，以及網域內首次登入是否自動建立帳號。存檔後立即生效，不必重新啟動。用戶端密鑰留空表示沿用原設定。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UpdateOidcSettingInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UpdateOidcSettingInput = parse(input)?;
        let current = ctx.store.oidc_setting().await?;

        let issuer = i
            .issuer
            .as_deref()
            .map(|value| value.trim().trim_end_matches('/').to_string());
        if let Some(issuer) = issuer.as_deref() {
            if !issuer.is_empty() && !is_web_url(issuer) {
                return Err(OpError::BadInput(
                    "供應商網址要以 https:// 開頭，例如 https://accounts.google.com".into(),
                ));
            }
        }

        let redirect_url = i
            .redirect_url
            .as_deref()
            .map(|value| value.trim().to_string());
        if let Some(redirect) = redirect_url.as_deref() {
            if !redirect.is_empty()
                && (!is_web_url(redirect) || !redirect.ends_with(OAUTH_CALLBACK_PATH))
            {
                return Err(OpError::BadInput(format!(
                    "轉址網址要填本系統的網址加上 {OAUTH_CALLBACK_PATH}"
                )));
            }
        }

        let client_id = i.client_id.as_deref().map(|value| value.trim().to_string());
        let client_secret = i.client_secret.as_deref().map(str::trim);
        let allowed_domain = i.allowed_domain.as_deref().map(normalize_login_domain);
        let provision_role = i
            .provision_role
            .as_deref()
            .map(|value| value.trim().to_string());

        let next_domain = allowed_domain.as_deref().unwrap_or(&current.allowed_domain);
        if let Some(role_code) = provision_role.as_deref() {
            if !role_code.is_empty() {
                let roles = ctx.store.roles().await?;
                if !roles.iter().any(|role| role.code == role_code) {
                    let known: Vec<&str> = roles.iter().map(|role| role.code.as_str()).collect();
                    return Err(OpError::BadInput(format!(
                        "查無角色「{role_code}」；可用的角色是 {}",
                        known.join("、")
                    )));
                }
                if next_domain.is_empty() {
                    return Err(OpError::BadInput(
                        "要自動建立帳號請先填公司電子郵件網域，否則任何外部帳號都能建立登入".into(),
                    ));
                }
            }
        }

        let next_enabled = i.enabled.unwrap_or(current.enabled);
        let next_password_disabled = i
            .password_login_disabled
            .unwrap_or(current.password_login_disabled);
        if next_password_disabled && !next_enabled {
            return Err(OpError::BadInput(
                "要停用帳號密碼登入請先啟用外部登入".into(),
            ));
        }
        if next_enabled {
            let next_issuer = issuer.as_deref().unwrap_or(&current.issuer);
            let next_client_id = client_id.as_deref().unwrap_or(&current.client_id);
            let next_redirect = redirect_url.as_deref().unwrap_or(&current.redirect_url);
            let has_secret = match client_secret {
                Some(secret) => !secret.is_empty(),
                None => !current.client_secret.is_empty(),
            };
            let missing: Vec<&str> = [
                ("供應商網址", next_issuer.is_empty()),
                ("用戶端 ID", next_client_id.is_empty()),
                ("用戶端密鑰", !has_secret),
                ("轉址網址", next_redirect.is_empty()),
            ]
            .into_iter()
            .filter_map(|(label, missing)| missing.then_some(label))
            .collect();
            if !missing.is_empty() {
                return Err(OpError::BadInput(format!(
                    "要啟用外部登入還缺 {}",
                    missing.join("、")
                )));
            }
        }

        let mut changed: Vec<String> = Vec::new();
        let mut changed_field = |label: &str, differs: bool| {
            if differs {
                changed.push(label.to_string());
            }
            differs
        };
        let write_enabled = i
            .enabled
            .filter(|value| changed_field("啟用狀態", *value != current.enabled));
        let write_issuer = issuer
            .as_deref()
            .filter(|value| changed_field("供應商網址", *value != current.issuer));
        let write_client_id = client_id
            .as_deref()
            .filter(|value| changed_field("用戶端 ID", *value != current.client_id));
        let write_secret = client_secret
            .filter(|value| changed_field("用戶端密鑰", *value != current.client_secret));
        let write_redirect = redirect_url
            .as_deref()
            .filter(|value| changed_field("轉址網址", *value != current.redirect_url));
        let write_domain = allowed_domain
            .as_deref()
            .filter(|value| changed_field("公司電子郵件網域", *value != current.allowed_domain));
        let write_role = provision_role
            .as_deref()
            .filter(|value| changed_field("自動建立帳號", *value != current.provision_role));
        let write_password_disabled = i.password_login_disabled.filter(|value| {
            changed_field("帳號密碼登入", *value != current.password_login_disabled)
        });

        if changed.is_empty() {
            return Ok(OpOutcome::new(
                json!({ "changed": changed, "enabled": current.enabled }),
                "設定外部登入 · 沒有變更".to_string(),
            ));
        }

        let updated = ctx
            .store
            .update_oidc_setting(laysi_store::WriteOidcSetting {
                enabled: write_enabled,
                issuer: write_issuer,
                client_id: write_client_id,
                client_secret: write_secret,
                redirect_url: write_redirect,
                allowed_domain: write_domain,
                provision_role: write_role,
                password_login_disabled: write_password_disabled,
                updated_by: &ctx.actor,
            })
            .await?;

        let summary = format!(
            "設定外部登入 · {}（{}）",
            if updated.enabled {
                "已啟用"
            } else {
                "未啟用"
            },
            changed.join("、")
        );
        Ok(OpOutcome::new(
            json!({
                "changed": changed,
                "enabled": updated.enabled,
                "issuer": updated.issuer,
                "provider_label": laysi_store::provider_label(&updated.issuer),
                "allowed_domain": updated.allowed_domain,
                "provision_role": updated.provision_role,
                "password_login_disabled": updated.password_login_disabled,
            }),
            summary,
        ))
    }
}

struct ListApprovalRules;

#[async_trait]
impl Operation for ListApprovalRules {
    fn name(&self) -> &'static str {
        "list_approval_rules"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀費用報支的簽核流程：關卡數、各關卡簽核角色與金額門檻。金額門檻可隨時調整。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let rules = ctx.store.approval_rules().await?;
        let steps: Vec<Value> = rules
            .iter()
            .map(|r| {
                json!({
                    "step_no": r.step_no,
                    "role_code": r.role_code,
                    "role_name": r.role_name,
                    "label": r.label,
                    "condition_note": r.condition_note,
                    "min_amount": r.min_amount.map(|a| a.to_string()),
                    "always": r.min_amount.is_none(),
                })
            })
            .collect();

        let summary = format!("讀取簽核流程 · {} 關", steps.len());
        Ok(OpOutcome::new(
            json!({
                "steps": steps,
                "enforcement_note": "流程至少保留一關。",
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display labels must cover every value permitted by migrations/0001.
    /// A gap shows as "ungrouped" on the chart of accounts, which reads as if
    /// the account were unconfigured.
    #[test]
    fn every_report_group_has_a_chinese_label() {
        for g in [
            "current_asset",
            "non_current_asset",
            "current_liability",
            "non_current_liability",
            "equity",
            "revenue",
            "cogs",
            "operating_expense",
            "other_income",
            "other_expense",
        ] {
            assert_ne!(group_label(g), "未分組", "{g} 沒有中文標籤");
        }
        assert_eq!(group_label("something_else"), "未分組");
    }

    #[test]
    fn every_cash_flow_section_has_a_chinese_label() {
        for s in ["cash", "operating", "investing", "financing"] {
            assert_ne!(section_label(Some(s)), "未分類", "{s} 沒有中文標籤");
        }
        // P&L accounts have no activity; that is deliberate, not a gap.
        assert_eq!(section_label(None), "併入本期損益");
    }

    /// A blank working day is an answer, not a missing field: `late()` returns
    /// `None` for "nobody said when work starts", which is not "nobody was
    /// late". Every other setting still has to carry a value.
    #[test]
    fn the_working_day_can_be_cleared_but_the_company_name_cannot() {
        assert_eq!(
            CLEARABLE_SETTINGS,
            [
                "brand_logo",
                "work_start",
                "work_end",
                "break_start",
                "break_end",
                "late_grace_minutes",
                "flex_window_minutes",
                "audit_keep_days"
            ],
            "可清空的設定變了：每一項的空值都要在畫面上有意義"
        );
        for code in ["company_name", "tax_id", "fiscal_year", "currency"] {
            assert!(!CLEARABLE_SETTINGS.contains(&code), "{code} 不該可以清空");
        }
    }

    /// The stored string is put into an `<img src>` on the signed-out
    /// sign-in screen, so anything that is not an image data URI has to be
    /// refused at the point it is written.
    #[test]
    fn a_logo_is_an_image_data_uri_within_the_size_cap() {
        let png = base64::engine::general_purpose::STANDARD.encode([0x89, b'P', b'N', b'G']);
        assert!(
            validate_setting(BRAND_LOGO_SETTING, &format!("data:image/png;base64,{png}")).is_ok()
        );
        assert!(validate_setting(
            BRAND_LOGO_SETTING,
            "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4="
        )
        .is_ok());
        for bad in [
            "https://example.test/logo.png",
            "data:text/html;base64,PHNjcmlwdD48L3NjcmlwdD4=",
            "data:image/gif;base64,R0lGODlh",
            "data:image/png;base64,not base64",
            "<svg></svg>",
        ] {
            assert!(
                validate_setting(BRAND_LOGO_SETTING, bad).is_err(),
                "{bad} 不該通過"
            );
        }
        let huge =
            base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_BRAND_LOGO_BYTES + 1]);
        assert!(
            validate_setting(BRAND_LOGO_SETTING, &format!("data:image/png;base64,{huge}")).is_err()
        );

        for good in BRAND_DISPLAYS {
            assert!(validate_setting(BRAND_DISPLAY_SETTING, good).is_ok());
        }
        assert!(validate_setting(BRAND_DISPLAY_SETTING, "兩個都要").is_err());
    }

    #[test]
    fn a_working_day_is_hh_mm_on_a_24_hour_clock() {
        for good in ["00:00", "09:00", "18:30", "23:59"] {
            assert!(is_clock_time(good), "{good} 應該通過");
            assert!(validate_setting("work_start", good).is_ok());
        }
        for bad in [
            "9:00",
            "0900",
            "24:00",
            "18:60",
            "18:5",
            "上午九點",
            "09:00:00",
        ] {
            assert!(!is_clock_time(bad), "{bad} 不該通過");
            assert!(validate_setting("work_end", bad).is_err());
        }
    }

    #[test]
    fn every_account_kind_has_a_chinese_label() {
        for k in ["asset", "liability", "equity", "revenue", "expense"] {
            assert_ne!(kind_label(k), "其他", "{k} 沒有中文標籤");
        }
    }

    /// Changing permissions, changing settings and creating logins are the
    /// system's own controls, so they sit on the admin floor rather than the
    /// owner one. Drop any of them to a lower tier and the permission system
    /// stops meaning anything, since anyone could grant themselves anything.
    #[test]
    fn system_controls_are_admin_only() {
        for op in ops() {
            if matches!(
                op.name(),
                "set_role_permission" | "update_company_setting" | "create_user_account"
            ) {
                assert_eq!(
                    op.risk(),
                    Risk::AdminOnly,
                    "{} 不該低於限系統管理員",
                    op.name()
                );
            }
        }
    }

    /// Permission operation names must exist in the registry, or missing_ops
    /// would never be empty. This does not validate the migration data (that
    /// needs a database), only that the spelling check itself works.
    #[test]
    fn the_registry_can_answer_whether_an_operation_exists() {
        let known: HashSet<&'static str> = crate::ops::all().iter().map(|o| o.name()).collect();
        assert!(known.contains("post_voucher"));
        assert!(known.contains("get_balance_sheet"));
        assert!(known.contains("set_role_permission"));
        assert!(!known.contains("approve_expense"));
    }

    #[test]
    fn audit_retention_is_a_positive_number_of_days_or_left_empty() {
        assert!(validate_setting(crate::AUDIT_KEEP_SETTING, "365").is_ok());
        assert!(validate_setting(crate::AUDIT_KEEP_SETTING, "1").is_ok());

        // Zero would delete the record of the call that set it to zero, and
        // the way to keep everything is to clear the setting instead.
        for bad in ["0", "-1", "30.5", "半年", "365 天"] {
            assert!(
                validate_setting(crate::AUDIT_KEEP_SETTING, bad).is_err(),
                "「{bad}」不該是有效的保存天數"
            );
        }
    }
}
