//! Employee self-service: the caller's own home, payslips, leave and claims.
//!
//! "Own" can only come from `ctx.employee_id`. None of these five operations
//! accepts an employee number, a name, an employee_id, an actor or a claimant
//! — authorisation rests on the shape of the input, not on `Risk`. The account
//! resolved from the UI session supplies `user_account.employee_id`, and every
//! read and write uses that primary key. An account with no linked employee
//! record gets an explicit error rather than an empty list that would hide the
//! missing link. MCP tokens carry no employee link, and 0009 leaves all five
//! self-service tools disabled for AI.
//!
//! Leave entitlement still has one source: this module passes completed
//! service months to `annual_leave_days`, and the database only sums approved
//! days actually taken. The number of days on a request is entered by the
//! employee, because the system has no weekend or public holiday rules and
//! cannot derive it from a date range.

use crate::ops::attendance::{leave_kind_label, leave_status_label, valid_leave_kind, LEAVE_KINDS};
use crate::ops::employees::{service_label, service_months};
use crate::ops::expenses::claim_json;
use crate::ops::insurance::ruleset_json;
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_regulation::Ruleset;
use laysi_store::{AttendanceRow, Employee, LeaveRequest, NewLeaveRequest, Payslip};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

const BANK_ACCOUNT_VISIBLE_DIGITS: usize = 4;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetMyHome),
        Box::new(ListMyPayslips),
        Box::new(ListMyLeaveRequests),
        Box::new(RequestLeave),
        Box::new(ListMyExpenseClaims),
    ]
}

async fn actor_employee(ctx: &OpCtx) -> Result<Employee, OpError> {
    let employee_id = ctx.employee_id.ok_or_else(|| {
        OpError::BadInput(
            "目前登入的帳號尚未連結員工資料，自助功能需對應一筆員工主檔".into(),
        )
    })?;
    ctx.store
        .employee_by_id(employee_id)
        .await?
        .ok_or_else(|| {
            OpError::BadInput("登入帳號連結的員工主檔不存在，請管理員檢查帳號設定".into())
        })
}

fn service_through(employee: &Employee, today: NaiveDate) -> NaiveDate {
    employee.left_on.map_or(today, |left| left.min(today))
}

#[derive(Clone, Copy)]
struct AnnualLeaveBalance {
    entitlement: Money,
    taken: Money,
}

impl AnnualLeaveBalance {
    fn remaining(self) -> Money {
        self.entitlement - self.taken
    }
}

fn annual_leave_balance(
    rs: &Ruleset,
    service_months: u32,
    taken: Money,
) -> Result<AnnualLeaveBalance, laysi_regulation::RegulationError> {
    Ok(AnnualLeaveBalance {
        entitlement: Money::from(laysi_regulation::annual_leave_days(rs, service_months)?),
        taken,
    })
}

async fn my_annual_leave(
    ctx: &OpCtx,
    employee: &Employee,
) -> Result<(&'static Ruleset, AnnualLeaveBalance), OpError> {
    let rs = laysi_regulation::resolve("annual_leave", ctx.today)?;
    let taken = ctx
        .store
        .annual_leave_taken(ctx.today.year())
        .await?
        .into_iter()
        .find(|(employee_id, _)| *employee_id == employee.id)
        .map(|(_, days)| days)
        .unwrap_or(Money::ZERO);
    let months = service_months(employee.hired_on, service_through(employee, ctx.today));
    Ok((rs, annual_leave_balance(rs, months, taken)?))
}

fn annual_leave_json(year: i32, rs: &Ruleset, balance: AnnualLeaveBalance) -> Value {
    json!({
        "year": year,
        "entitlement": balance.entitlement.to_string(),
        "taken": balance.taken.to_string(),
        "remaining": balance.remaining().to_string(),
        "ruleset": ruleset_json(rs),
        "quota_stored": false,
        "note": "額度依年資與 annual_leave 規則現算；已休只加總本年度已核准的特休。",
    })
}

fn masked_bank_account(value: &str) -> String {
    let digits = value
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<Vec<_>>();
    if digits.len() <= BANK_ACCOUNT_VISIBLE_DIGITS {
        return if value.trim().is_empty() {
            "未提供".into()
        } else {
            "已遮罩".into()
        };
    }

    let tail = digits[digits.len() - BANK_ACCOUNT_VISIBLE_DIGITS..]
        .iter()
        .collect::<String>();
    let bank = value
        .split(|c: char| c.is_ascii_digit())
        .next()
        .unwrap_or_default()
        .trim();
    if bank.is_empty() {
        format!("尾碼 {tail}")
    } else {
        format!("{bank} · 尾碼 {tail}")
    }
}

fn profile_json(employee: &Employee, months: u32) -> Value {
    json!({
        "emp_no": employee.emp_no,
        "name": employee.name,
        "dept_code": employee.dept_code,
        "dept_label": employee.dept_label,
        "title": employee.title,
        "role_code": employee.role_code,
        "role_name": employee.role_name,
        "employment_type": employee.employment_type,
        "hired_on": employee.hired_on,
        "left_on": employee.left_on,
        "status": employee.status,
        "service_months": months,
        "service_label": service_label(months),
        "bank_account_masked": masked_bank_account(&employee.bank_account),
        "privacy_note": "不回身分證字號；匯薪帳戶只回銀行名稱與尾碼。",
    })
}

fn attendance_json(row: &AttendanceRow) -> Value {
    json!({
        "month": row.month,
        "required_days": row.required_days.to_string(),
        "worked_days": row.worked_days.to_string(),
        "leave_days": row.leave_days.to_string(),
        "overtime_hours": row.overtime_hours.to_string(),
        "overtime_pay": row.overtime_pay.to_string(),
        "late_count": row.late_count,
        "note": row.note,
        "overtime_recomputed": false,
        "overtime_note": "加班費倍數尚未收錄至規則庫，金額由差勤來源帶入，本系統不自行重算。",
    })
}

fn leave_request_json(request: &LeaveRequest) -> Value {
    json!({
        "leave_no": request.leave_no,
        "kind": request.kind,
        "kind_label": leave_kind_label(&request.kind),
        "from_on": request.from_on,
        "to_on": request.to_on,
        "days": request.days.to_string(),
        "reason": request.reason,
        "status": request.status,
        "status_label": leave_status_label(&request.status),
        "decided_by": request.decided_by,
        "decided_role": request.decided_role,
        "decided_at": request.decided_at,
        "decision_note": request.decision_note,
        "created_at": request.created_at,
        "created_by": request.created_by,
    })
}

fn payslip_json(slip: &Payslip) -> Value {
    json!({
        "period": slip.period,
        "pay_on": slip.pay_on,
        "base_salary": slip.base_salary.to_string(),
        "overtime": slip.overtime.to_string(),
        "allowance": slip.allowance.to_string(),
        "gross": slip.gross.to_string(),
        "deductions": {
            "labor_insurance": slip.labor_employee.to_string(),
            "health_insurance": slip.health_employee.to_string(),
            "income_tax": slip.income_tax.to_string(),
            "total": slip.deduction.to_string(),
        },
        "net": slip.net.to_string(),
        "employer_contributions": {
            "labor_insurance": slip.labor_employer.to_string(),
            "health_insurance": slip.health_employer.to_string(),
            "pension": slip.pension_employer.to_string(),
            "total": (
                slip.labor_employer + slip.health_employer + slip.pension_employer
            ).to_string(),
        },
        "paid_days": slip.paid_days.to_string(),
        "month_days": slip.month_days.to_string(),
        "prorated": slip.paid_days < slip.month_days,
        "rulesets": {
            "labor_insurance": slip.labor_version,
            "health_insurance": slip.health_version,
            "pension": slip.pension_version,
            "withholding": slip.withholding_version,
        },
        "note": slip.note,
    })
}

// My home

struct GetMyHome;

#[async_trait]
impl Operation for GetMyHome {
    fn name(&self) -> &'static str {
        "get_my_home"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀目前呼叫者本人的員工首頁：基本資料（不含身分證字號，銀行帳戶僅留尾碼）、年資、本月出勤、特休餘額，以及本人送出的請假與報支的目前關卡。無任何參數可指定他人。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let months = service_months(employee.hired_on, service_through(&employee, ctx.today));
        let month = ctx.today.format("%Y-%m").to_string();
        let attendance = ctx
            .store
            .attendance(&month)
            .await?
            .into_iter()
            .find(|row| row.employee_id == employee.id);
        let (annual_rs, annual_balance) = my_annual_leave(ctx, &employee).await?;
        let leaves = ctx.store.leave_requests_by_employee(employee.id).await?;
        let approval_rules = ctx.store.approval_rules().await?;
        let claims = ctx
            .store
            .expense_claims_by_claimant(&employee.name, i64::MAX)
            .await?;

        let pending_leaves = leaves
            .iter()
            .filter(|request| request.status == "pending")
            .count();
        let pending_claims = claims
            .iter()
            .filter(|claim| claim.status == "submitted" || claim.status == "approving")
            .count();
        let summary = format!(
            "讀 {} 的員工自助首頁 · 年資 {} · 本月出勤{} · {pending_leaves} 張請假、{pending_claims} 張報支進行中",
            employee.name,
            service_label(months),
            if attendance.is_some() { "已帶入" } else { "尚無資料" },
        );

        Ok(OpOutcome::new(
            json!({
                "profile": profile_json(&employee, months),
                "attendance_month": month,
                "attendance": attendance.as_ref().map(attendance_json),
                "attendance_note": if attendance.is_some() {
                    "本月差勤已有來源資料。"
                } else {
                    "本月尚無差勤資料，不代表請假與遲到皆為零。"
                },
                "annual_leave": annual_leave_json(
                    ctx.today.year(),
                    annual_rs,
                    annual_balance,
                ),
                "submissions": {
                    "leave_requests": leaves
                        .iter()
                        .map(leave_request_json)
                        .collect::<Vec<_>>(),
                    "expense_claims": claims
                        .iter()
                        .map(|claim| claim_json(claim, &approval_rules))
                        .collect::<Vec<_>>(),
                },
                "pending": {
                    "leave_requests": pending_leaves,
                    "expense_claims": pending_claims,
                },
                "identity_note": "資料對象由已驗證 session 的 ctx.employee_id 決定，不使用姓名比對。",
            }),
            summary,
        ))
    }
}

// My payslips

struct ListMyPayslips;

#[async_trait]
impl Operation for ListMyPayslips {
    fn name(&self) -> &'static str {
        "list_my_payslips"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀目前登入帳號所連結員工本人的全部薪資單：應發、代扣、實發與公司負擔。輸入不含員工欄位，對象僅由 ctx.employee_id 決定；與回傳全公司資料的 BossOnly 操作 list_payslips 不同。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let slips = ctx.store.payslips_by_employee(employee.id).await?;
        let gross: Money = slips.iter().map(|slip| slip.gross).sum();
        let net: Money = slips.iter().map(|slip| slip.net).sum();
        let summary = format!(
            "讀 {} 的薪資單 · {} 期 · 應發 {} 實發 {}",
            employee.name,
            slips.len(),
            money(gross),
            money(net)
        );

        Ok(OpOutcome::new(
            json!({
                "employee": {
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "dept_label": employee.dept_label,
                    "title": employee.title,
                },
                "payslips": slips.iter().map(payslip_json).collect::<Vec<_>>(),
                "periods": slips
                    .iter()
                    .map(|slip| slip.period.clone())
                    .collect::<Vec<_>>(),
                "gross_total": gross.to_string(),
                "net_total": net.to_string(),
                "scope_note": "僅查詢已驗證 session 的 ctx.employee_id；輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

// My leave

struct ListMyLeaveRequests;

#[async_trait]
impl Operation for ListMyLeaveRequests {
    fn name(&self) -> &'static str {
        "list_my_leave_requests"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀目前呼叫者自己的全部請假單與本年度特休額度、已休、剩餘。額度由查詢日適用的 annual_leave ruleset 與年資現算，不存在資料庫。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let requests = ctx.store.leave_requests_by_employee(employee.id).await?;
        let (annual_rs, annual_balance) = my_annual_leave(ctx, &employee).await?;
        let pending = requests
            .iter()
            .filter(|request| request.status == "pending")
            .count();
        let summary = format!(
            "讀 {} 的請假單 · {} 張（{pending} 張等簽）· 特休依 annual_leave {} 現算",
            employee.name,
            requests.len(),
            annual_rs.version
        );

        Ok(OpOutcome::new(
            json!({
                "employee": {
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                },
                "requests": requests
                    .iter()
                    .map(leave_request_json)
                    .collect::<Vec<_>>(),
                "pending_count": pending,
                "annual_leave": annual_leave_json(
                    ctx.today.year(),
                    annual_rs,
                    annual_balance,
                ),
                "scope_note": "僅查詢已驗證 session 的 ctx.employee_id；輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

// Submitting a leave request

struct RequestLeave;

#[derive(Deserialize, JsonSchema)]
pub struct RequestLeaveInput {
    /// Leave type. Valid values match the CHECK on `leave_request.kind`.
    pub kind: String,
    /// First day of leave.
    pub from_on: NaiveDate,
    /// Last day of leave.
    pub to_on: NaiveDate,
    /// Days requested, entered by the employee; half days may use decimals.
    /// The system does not derive this from the date range.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub days: Money,
    /// Reason for the request.
    #[serde(default)]
    pub reason: Option<String>,
}

#[async_trait]
impl Operation for RequestLeave {
    fn name(&self) -> &'static str {
        "request_leave"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "替目前登入帳號所連結的員工送一張待簽核請假單。employee_id 由 ctx.employee_id 決定，created_by 來自同一個已驗證身分；輸入不能指定申請人。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<RequestLeaveInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RequestLeaveInput = parse(input)?;
        if !valid_leave_kind(&i.kind) {
            let values = LEAVE_KINDS
                .iter()
                .map(|kind| format!("{kind}（{}）", leave_kind_label(kind)))
                .collect::<Vec<_>>()
                .join("、");
            return Err(OpError::BadInput(format!(
                "「{}」不是認得的假別。目前有：{values}",
                i.kind
            )));
        }
        if i.to_on < i.from_on {
            return Err(OpError::BadInput("請假結束日不能早於開始日".into()));
        }
        if i.days <= Money::ZERO {
            return Err(OpError::BadInput("請假日數要大於零".into()));
        }

        let employee = actor_employee(ctx).await?;
        let request = ctx
            .store
            .create_leave_request(NewLeaveRequest {
                employee_id: employee.id,
                kind: &i.kind,
                from_on: i.from_on,
                to_on: i.to_on,
                days: i.days,
                reason: i.reason.as_deref().unwrap_or(""),
                actor: &ctx.actor,
            })
            .await?;
        let summary = format!(
            "{} · {} {}申請 {} 日 · {} 到 {} · 等簽核",
            request.leave_no,
            employee.name,
            leave_kind_label(&request.kind),
            request.days.normalize(),
            request.from_on,
            request.to_on
        );

        Ok(OpOutcome::new(
            json!({
                "request": leave_request_json(&request),
                "status": "pending",
                "days_source": "由申請人填寫；系統尚未收錄例假日與國定假日規則，因此不從日期範圍推算。",
                "scope_note": "employee_id 由已驗證 session 的 ctx.employee_id 決定，created_by 來自同一個登入身分。",
            }),
            summary,
        ))
    }
}

// My expense claims

struct ListMyExpenseClaims;

#[async_trait]
impl Operation for ListMyExpenseClaims {
    fn name(&self) -> &'static str {
        "list_my_expense_claims"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀目前呼叫者本人送出的費用報支單，包含各關卡的產生原因、已簽核人員、目前關卡與最終產生的傳票。輸入不含 claimant 或其他人員欄位。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let rules = ctx.store.approval_rules().await?;
        let claims = ctx
            .store
            .expense_claims_by_claimant(&employee.name, i64::MAX)
            .await?;
        let waiting: Money = claims
            .iter()
            .filter(|claim| claim.status == "submitted" || claim.status == "approving")
            .map(|claim| claim.amount)
            .sum();
        let waiting_count = claims
            .iter()
            .filter(|claim| claim.status == "submitted" || claim.status == "approving")
            .count();
        let summary = format!(
            "讀 {} 的費用報支 · {} 張（{waiting_count} 張進行中，合計 {}）",
            employee.name,
            claims.len(),
            money(waiting)
        );

        Ok(OpOutcome::new(
            json!({
                "employee": {
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                },
                "claims": claims
                    .iter()
                    .map(|claim| claim_json(claim, &rules))
                    .collect::<Vec<_>>(),
                "waiting_count": waiting_count,
                "waiting_total": waiting.to_string(),
                "scope_note": "先以 ctx.employee_id 取得已連結員工，再查詢該員工姓名送出的報支；輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// Leave entitlement is read only from the ruleset payload. Replace the
    /// payload for the applicable band and the same service length must give
    /// a different answer; this module has no entitlement constant to fall
    /// back on.
    #[test]
    fn annual_leave_quota_changes_with_the_ruleset_payload() {
        let rs = laysi_regulation::all()
            .iter()
            .find(|ruleset| ruleset.domain == "annual_leave")
            .expect("規則庫缺少 annual_leave");
        let tiers = rs
            .payload
            .get("tiers")
            .and_then(Value::as_array)
            .expect("annual_leave payload 缺少 tiers");
        let first = tiers.first().expect("annual_leave tiers 是空的");
        let months = first
            .get("months")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<u32>().ok())
            .expect("第一個 tiers.months 不是整數字串");
        let original_days = first
            .get("days")
            .and_then(Value::as_str)
            .expect("第一個 tiers.days 不是字串");
        let replacement = tiers
            .iter()
            .skip(1)
            .filter_map(|tier| tier.get("days").and_then(Value::as_str))
            .find(|days| *days != original_days)
            .expect("需要另一個不同的 tiers.days 才能驗證 payload 可替換");

        let before = annual_leave_balance(rs, months, Money::ZERO).unwrap();
        let mut changed = rs.clone();
        changed.payload["tiers"][0]["days"] = Value::String(replacement.to_string());
        let after = annual_leave_balance(&changed, months, Money::ZERO).unwrap();

        assert_ne!(before.entitlement, after.entitlement);
    }

    /// The structural guarantee behind self-service authorisation: every read
    /// operation takes empty input, and submitting leave accepts only the
    /// type, dates, days and reason. Adding any field naming a person must
    /// make this test fail first.
    #[test]
    fn no_self_service_input_can_select_another_employee() {
        let expected: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::from([
            ("get_my_home", BTreeSet::new()),
            ("list_my_payslips", BTreeSet::new()),
            ("list_my_leave_requests", BTreeSet::new()),
            (
                "request_leave",
                BTreeSet::from(["kind", "from_on", "to_on", "days", "reason"]),
            ),
            ("list_my_expense_claims", BTreeSet::new()),
        ]);
        let forbidden = [
            "employee_id",
            "emp_no",
            "name",
            "actor",
            "claimant",
            "user_id",
        ];
        let operations = ops();
        assert_eq!(operations.len(), expected.len());

        for operation in operations {
            let schema = operation.input_schema();
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|values| values.keys().map(String::as_str).collect::<BTreeSet<_>>())
                .unwrap_or_default();
            assert_eq!(
                properties,
                expected[operation.name()],
                "{} 的 Input 欄位改了，必須重新審查是否能指定別人",
                operation.name()
            );
            for field in forbidden {
                assert!(
                    !properties.contains(field),
                    "{} 出現可指定他人的欄位 {field}",
                    operation.name()
                );
            }
        }
    }

    #[test]
    fn bank_accounts_are_masked_again_before_they_leave_the_operation() {
        assert_eq!(
            masked_bank_account("示範銀行 3812-0000-5508"),
            "示範銀行 · 尾碼 5508"
        );
        assert_eq!(masked_bank_account("5508"), "已遮罩");
        assert_eq!(masked_bank_account(""), "未提供");
    }
}
