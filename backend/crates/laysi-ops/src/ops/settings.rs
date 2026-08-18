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

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetCompanySettings),
        Box::new(UpdateCompanySetting),
        Box::new(ListChartOfAccounts),
        Box::new(ListDimensions),
        Box::new(ListRoles),
        Box::new(SetRolePermission),
        Box::new(CreateUserAccount),
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
        "讀公司與帳務設定：公司名稱、統編、會計年度、本位幣、單據編號規則。營業稅那一列不是設定值，是從法規規則庫依生效日算出來的。"
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
                    "source": "setting",
                    "updated_at": s.updated_at,
                    "updated_by": s.updated_by,
                })
            })
            .collect();

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

        let summary = format!("讀取公司設定 · {} 項（含 1 項來自法規規則庫）", rows.len());
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
        "變更一項公司設定。僅標示為可編輯的項目可以修改；會計年度、期初餘額等項目涉及期初重跑，不是改一個字串就能完成。法規決定的值不在此處管理。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UpdateCompanySettingInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UpdateCompanySettingInput = parse(input)?;
        let value = i.value.trim();
        if value.is_empty() {
            return Err(OpError::BadInput("設定值為必填".into()));
        }
        let s = ctx.store.update_company_setting(&i.code, value, &ctx.actor).await?;
        let summary = format!("改設定 · {} 改成「{}」", s.label, s.value);
        Ok(OpOutcome::new(json!({ "setting": s }), summary))
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
        "讀會計科目表：代號、名稱、類別、正常餘額方向，以及它在資產負債表哪一段、在現金流量表屬於哪一種活動，還有目前的餘額"
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
                    "normal_side_label": if m.normal_side == "debit" { "借" } else { "貸" },
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
        "讀既有維度與其值：部門、專案、成本中心、付款條件。這個操作只讀設定；department 維度已掛到分錄與員工的部門欄位。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let dims = ctx.store.dimensions().await?;
        let values: usize = dims.iter().map(|d| d.values.len()).sum();
        let summary = format!("讀取維度 · {} 種、共 {values} 個值", dims.len());
        Ok(OpOutcome::new(
            json!({
                "dimensions": dims,
                // Only departments are wired up so far; the composite foreign key
                // guarantees dept_id cannot point at another dimension.
                "applied_to_entries": true,
                "entry_dimension": "department",
                "application_note":
                    "voucher_line.dept_id 與 employee.dept_id 只接受 department 維度的值；\
                     project、cost_center、payment_term 尚未掛到分錄。",
            }),
            summary,
        ))
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
        "讀角色權限矩陣：哪個角色有哪些權限、完整還是有限。每次呼叫都會即時強制檢查。"
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
            crate::ops::all().iter().filter(|o| o.risk() == want).map(|o| o.name()).collect()
        };
        let boss_only = with_risk(Risk::BossOnly);
        let admin_only = with_risk(Risk::AdminOnly);

        let perms: Vec<Value> = permissions
            .iter()
            .map(|p| {
                let missing: Vec<&String> =
                    p.op_names.iter().filter(|n| !known.contains(n.as_str())).collect();
                let boss_ops: Vec<&String> =
                    p.op_names.iter().filter(|n| boss_only.contains(n.as_str())).collect();
                let admin_ops: Vec<&String> =
                    p.op_names.iter().filter(|n| admin_only.contains(n.as_str())).collect();
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
            "讀取權限矩陣 · {} 個角色 × {} 條權限 · {} 格有授權",
            roles.len(),
            permissions.len(),
            cells.len()
        );
        Ok(OpOutcome::new(
            json!({
                "roles": roles,
                "permissions": perms,
                "grants": cells,
                // These two fields are the most important output of this operation
                // and must be shown on screen.
                "enforced": true,
                "enforcement_note":
                    "每次呼叫都即時檢查該角色的權限，未獲授權即拒絕並留下紀錄。\
                     「限負責人」為額外限制。",
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
        "設定或收回權限矩陣的一格。權限層級用 full／limited／none；改動會從下一次操作呼叫起立即生效。"
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
        let summary = format!(
            "改權限 · {} 的「{}」設為{}（下一次呼叫立即生效）",
            i.role_code, i.permission_code, what
        );
        Ok(OpOutcome::new(json!({ "cell": cell, "enforced": true }), summary))
    }
}

/// Login name rules, shared with the web bootstrap endpoint so the two
/// entry points cannot drift.
const USERNAME_MIN_CHARS: usize = 3;
const USERNAME_MAX_CHARS: usize = 64;
const DISPLAY_NAME_MAX_CHARS: usize = 64;
const EMAIL_MAX_CHARS: usize = 254;

#[derive(Deserialize, JsonSchema)]
pub struct CreateUserAccountInput {
    /// Login name: letters, digits and `.` `_` `-`, 3 to 64 characters.
    pub username: String,
    /// Name shown on screen and in the audit trail.
    pub display_name: String,
    /// Optional; may be empty.
    #[serde(default)]
    pub email: String,
    /// Role code from the role table, e.g. `acct`.
    pub role_code: String,
    /// Optional employee number to link, so the account can use self-service.
    #[serde(default)]
    pub employee_no: String,
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
        "建立一個登入帳號並指定角色。密碼由系統產生，僅在本次回應出現一次，本人首次登入必須變更；角色建立後無法在系統內變更。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateUserAccountInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CreateUserAccountInput = parse(input)?;

        let username = i.username.trim();
        let count = username.chars().count();
        if !(USERNAME_MIN_CHARS..=USERNAME_MAX_CHARS).contains(&count) {
            return Err(OpError::BadInput("登入名稱需要 3 到 64 個字元".into()));
        }
        if !username.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
            return Err(OpError::BadInput(
                "登入名稱只能使用英文字母、數字與 . _ - 三種符號".into(),
            ));
        }

        let display_name = i.display_name.trim();
        if display_name.is_empty() {
            return Err(OpError::BadInput("顯示名稱為必填".into()));
        }
        if display_name.chars().count() > DISPLAY_NAME_MAX_CHARS {
            return Err(OpError::BadInput("顯示名稱不可超過 64 個字元".into()));
        }

        let email = i.email.trim();
        if !email.is_empty()
            && (email.chars().count() > EMAIL_MAX_CHARS
                || !email.contains('@')
                || email.chars().any(char::is_whitespace))
        {
            return Err(OpError::BadInput("電子郵件格式不正確；此欄可以留空".into()));
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

        if ctx.store.user_by_username(username).await?.is_some() {
            return Err(OpError::BadInput(format!("登入名稱「{username}」已被使用")));
        }

        // Linking is by employee number, not by name: names repeat.
        let employee_no = i.employee_no.trim();
        let employee = if employee_no.is_empty() {
            None
        } else {
            Some(ctx.store.employee(employee_no).await.map_err(|_| {
                OpError::BadInput(format!("查無員工編號「{employee_no}」"))
            })?)
        };

        // The administrator never sets a lasting password. A generated one is
        // returned once and must be changed at first login, so the person who
        // creates the account cannot keep using it.
        let temporary = laysi_store::password::temporary_password();
        let password_hash = laysi_store::password::hash_password(&temporary)
            .map_err(|e| OpError::BadInput(format!("無法建立密碼雜湊：{e}")))?;

        let user = ctx
            .store
            .create_user(laysi_store::CreateUser {
                username,
                display_name,
                email,
                password_hash: Some(&password_hash),
                role_code,
                employee_id: employee.as_ref().map(|e| e.id),
                status: "active",
                must_change_password: true,
                created_by: &ctx.actor,
            })
            .await?;

        let summary = format!(
            "建立帳號 · {}（{}）角色{}{}",
            user.username,
            user.display_name,
            role.name,
            match &employee {
                Some(e) => format!("，連結員工 {}", e.emp_no),
                None => String::new(),
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
                    "role_name": role.name,
                    "employee_no": employee.as_ref().map(|e| e.emp_no.clone()),
                },
                // Shown once and never retrievable: only the hash is stored.
                "temporary_password": temporary,
                "must_change_password": true,
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
        "讀費用報支的簽核流程：幾關、每一關是誰、超過多少錢才需要。金額門檻是公司政策，可隨時調整，不是寫死在程式碼中。"
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
                "enforced": true,
                "enforcement_note":
                    "費用報支會依各關的金額門檻現算所需關卡；同一個關卡編號可覆寫\
                     或刪除，但流程至少保留一關。",
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display labels must cover every value permitted by migrations/0006.
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
                assert_eq!(op.risk(), Risk::AdminOnly, "{} 不該低於限系統管理員", op.name());
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
}
