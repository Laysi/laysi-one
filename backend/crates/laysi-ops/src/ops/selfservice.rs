//! Employee self-service: the caller's own home, payslips, leave and claims.
//!
//! "Own" can only come from `ctx.employee_id`. None of these operations
//! accepts an employee number, a name, an employee_id, an actor or a
//! claimant — authorisation rests on the shape of the input, not on `Risk`.
//! `list_my_notices` is the one that reads nothing personal at all: the
//! bulletin board is the same board for everyone, so it has no person in it
//! to get wrong.
//! The account resolved from the UI session supplies
//! `user_account.employee_id`, and every read and write uses that primary key.
//! An account with no linked employee record gets an explicit error rather
//! than an empty list that would hide the missing link.
//!
//! Over MCP the same primary key comes from `api_token.owner_employee_id`,
//! which only `issue_my_api_token` sets and only from the issuer's own
//! session. A token the owner issued for the installation carries no owner and
//! stays unlinked, so these operations refuse it rather than guessing whose
//! records to return from a display name.
//!
//! `submit_my_expense_claim` exists because `submit_expense_claim` carries a
//! `claimant` field: granting that one to staff would also let them file in
//! someone else's name. Two fields of the general operation are dropped here
//! rather than renamed. The expense account is not asked for, because deciding
//! it is what the accounting approval step is for. The receipt document number
//! is not accepted either: staff hold no document permission, so any number
//! they could pass would be someone else's, and the reply would disclose its
//! name. `attach_my_expense_receipt` sends the bytes instead, which points at
//! nothing that already exists.
//!
//! Leave entitlement still has one source: this module passes completed
//! service months to `annual_leave_days`, and the database only sums approved
//! days actually taken. A request states when the leave starts and ends; the
//! hours and days it is worth are derived from the working window, so the
//! two numbers on the request cannot disagree with the range above them.

use crate::ops::attendance::{
    business_instant, correction_auto_approves, holds_hours, judges_absence, leave_kind_label,
    leave_status_label, mark_days, month_days, month_span, overtime_json, punch_correction_json,
    punch_correction_status_label, punch_days, punch_kind_label, summarise, valid_leave_kind,
    OvertimeLimits, WorkSchedule, LEAVE_KINDS, PUNCH_KINDS, PUNCH_TIME,
};
use crate::ops::documents::{human_size, short_checksum};
use crate::ops::employees::{service_label, service_months};
use crate::ops::expenses::{checked_pay_method, claim_json, required_steps};
use crate::ops::insurance::ruleset_json;
use crate::ops::notices::{history_outcome, missing_notice, notice_json};
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use base64::Engine;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use laysi_domain::{business_time, Money};
use laysi_regulation::Ruleset;
use laysi_store::storage::{self, MAX_OBJECT_BYTES};
use laysi_store::{
    DocumentRow, Employee, LeaveRequest, NewLeaveRequest, NewOvertimeRequest, NewPunchCorrection,
    OvertimeRequest, Payslip, ReserveDocument, SubmitExpenseClaim,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const BANK_ACCOUNT_VISIBLE_DIGITS: usize = 4;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetMyHome),
        Box::new(ListMyPayslips),
        Box::new(ListMyLeaveRequests),
        Box::new(PreviewLeave),
        Box::new(RequestLeave),
        Box::new(WithdrawMyLeaveRequest),
        Box::new(ListMyOvertimeRequests),
        Box::new(SubmitMyOvertimeRequest),
        Box::new(WithdrawMyOvertimeRequest),
        Box::new(ListMyExpenseClaims),
        Box::new(SubmitMyExpenseClaim),
        Box::new(AttachMyExpenseReceipt),
        Box::new(PunchClock),
        Box::new(ListMyPunches),
        Box::new(SubmitMyPunchCorrection),
        Box::new(ListMyPunchCorrections),
        Box::new(ListMyNotices),
        Box::new(MarkMyNoticesRead),
        Box::new(IssueMyApiToken),
        Box::new(ListMyApiTokens),
        Box::new(RevokeMyApiToken),
    ]
}

/// The leave types the database accepts, with their labels.
///
/// The screen offers exactly these. Deriving the list from the caller's past
/// requests instead would hide every type they have not used yet.
fn leave_kinds_json() -> Vec<Value> {
    LEAVE_KINDS
        .iter()
        .map(|kind| json!({ "kind": kind, "label": leave_kind_label(kind) }))
        .collect()
}

async fn actor_employee(ctx: &OpCtx) -> Result<Employee, OpError> {
    let employee_id = ctx
        .employee_id
        .ok_or_else(|| OpError::BadInput("本帳號尚未連結員工編號，請洽系統管理員設定".into()))?;
    ctx.store
        .employee_by_id(employee_id)
        .await?
        .ok_or_else(|| OpError::BadInput("本帳號連結的員工編號查無資料，請洽系統管理員設定".into()))
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
        "note": "額度依年資與特休規則現算；已休只加總本年度已核准的特休。",
    })
}

/// An account number as the owner's own screen shows it: the last digits and
/// nothing else. The bank name travels beside it in its own field, so this
/// function never has to guess where one ends and the other begins.
fn masked_account_no(value: &str) -> String {
    let digits = value
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<Vec<_>>();
    if digits.len() <= BANK_ACCOUNT_VISIBLE_DIGITS {
        return "已遮罩".into();
    }
    let tail = digits[digits.len() - BANK_ACCOUNT_VISIBLE_DIGITS..]
        .iter()
        .collect::<String>();
    format!("尾碼 {tail}")
}

fn profile_json(employee: &Employee, months: u32) -> Value {
    json!({
        "emp_no": employee.emp_no,
        "name": employee.name,
        "dept_code": employee.dept_code,
        "dept_label": employee.dept_label,
        "title": employee.title,
        "employment_type": employee.employment_type,
        "employment_label": crate::ops::employees::employment_label(&employee.employment_type),
        "pay_basis": employee.pay_basis,
        "pay_basis_label": crate::ops::employees::pay_basis_label(&employee.pay_basis),
        "hired_on": employee.hired_on,
        "left_on": employee.left_on,
        "status": employee.status,
        "status_label": crate::ops::employees::status_label(&employee.status),
        "service_months": months,
        "service_label": service_label(months),
        "bank_accounts": employee.bank_accounts.iter().map(|account| json!({
            "bank_name": account.bank_name,
            "account_masked": masked_account_no(&account.account_no),
            "is_payroll": account.is_payroll,
            "note": account.note,
        })).collect::<Vec<_>>(),
        // The same rule as the person's own record: no id number, and no
        // birthday either, which with a name is most of an identity.
        "dependents": employee.dependents.iter().map(|d| json!({
            "name": d.name,
            "relation_label": crate::ops::employees::relation_label(&d.relation),
            "health_enrolled_from": d.health_enrolled_from,
            "health_enrolled_until": d.health_enrolled_until,
        })).collect::<Vec<_>>(),
        "privacy_note": "不回本人與眷屬的身分證字號，也不回眷屬生日；銀行帳戶只回銀行名稱與尾碼。",
    })
}

fn leave_request_json(request: &LeaveRequest) -> Value {
    json!({
        "leave_no": request.leave_no,
        "kind": request.kind,
        "kind_label": leave_kind_label(&request.kind),
        "from_at": request.from_at,
        "to_at": request.to_at,
        "from_on": request.from_on,
        "to_on": request.to_on,
        "hours": request.hours.to_string(),
        "days": request.days.to_string(),
        "reason": request.reason,
        "status": request.status,
        "status_label": leave_status_label(&request.status),
        "decided_by": request.decided_by,
        "decided_role": request.decided_role,
        "decided_at": request.decided_at,
        "decision_note": request.decision_note,
        "cancelled": request.cancelled,
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
        // Dependants the health deduction includes, besides the person.
        "health_dependents": slip.health_dependents,
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
            "health_dependents": slip.dependents_version,
            "partial_month": slip.partial_month_version,
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
        "讀目前呼叫者本人的員工首頁：基本資料（不含身分證字號，銀行帳戶僅留尾碼）、年資、本月出勤、特休餘額，以及本人送出的休假與報支的目前關卡。無任何參數可指定他人。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let months = service_months(employee.hired_on, service_through(&employee, ctx.today));
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
            "讀 {} 的員工自助首頁 · 年資 {} · {pending_leaves} 張休假、{pending_claims} 張報支進行中",
            employee.name,
            service_label(months),
        );

        Ok(OpOutcome::new(
            json!({
                "profile": profile_json(&employee, months),
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
        "讀目前登入帳號所連結員工本人的全部薪資單：應發、代扣、實發與公司負擔。輸入不含員工欄位，對象僅由登入身分決定，不會回傳他人的薪資單。"
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
        "讀本人的全部休假單與本年度特休額度、已休、剩餘。額度由查詢日適用的特休規則與年資現算，不逐人設定。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let requests = ctx.store.leave_requests_by_employee(employee.id).await?;
        let (annual_rs, annual_balance) = my_annual_leave(ctx, &employee).await?;
        let schedule = WorkSchedule::load(ctx).await?;
        let pending = requests
            .iter()
            .filter(|request| request.status == "pending")
            .count();
        let summary = format!(
            "讀{}的休假單 · {} 張（{pending} 張等簽）· 特休依特休規則 {} 現算",
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
                "leave_kinds": leave_kinds_json(),
                "schedule": schedule.json(),
                "next_workday": next_workday(&schedule, ctx.today),
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

struct LeaveWorth {
    hours: Money,
    days: Money,
    /// Dates the range covers that are not working days.
    off_days: usize,
}

/// A system bound on one request, not a statutory figure.
const MAX_LEAVE_SPAN_DAYS: i64 = 366;

/// What a leave range is worth. `request_leave` stores the hours and days and
/// `preview_leave` shows them before sending, so the two come from this one
/// function and the screen cannot promise a number the request does not keep.
fn leave_worth(
    schedule: &WorkSchedule,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> Result<LeaveWorth, OpError> {
    if to <= from {
        return Err(OpError::BadInput("休假結束時刻要晚於開始時刻".into()));
    }
    // The work below walks the range one date at a time, and `preview_leave`
    // is open to AI, so a range spanning millennia would hold a worker.
    if (to.date() - from.date()).num_days() > MAX_LEAVE_SPAN_DAYS {
        return Err(OpError::BadInput(format!(
            "一張休假單最長 {MAX_LEAVE_SPAN_DAYS} 天，請分成幾張送出"
        )));
    }
    let (Some(seconds), Some(daily), Some(start), Some(end)) = (
        schedule.leave_seconds(from, to),
        schedule.daily_seconds(),
        schedule.starts_at,
        schedule.ends_at,
    ) else {
        return Err(OpError::BadInput(
            "休假時數依上下班時間計算，請系統管理員在基礎設定填寫上班時間與下班時間".into(),
        ));
    };
    // A range ending at midnight does not reach into the next date.
    let last = (to - chrono::Duration::seconds(1)).date();
    let dates = from.date().iter_days().take_while(|d| *d <= last);
    let off_days = dates.filter(|d| !schedule.day_kind(*d).works()).count();
    if seconds == 0 {
        let touched = (last - from.date()).num_days() as usize + 1;
        return Err(OpError::BadInput(if off_days == touched {
            "所選日期都不是工作日，請改選工作日".into()
        } else {
            format!(
                "所選時段落在上班時間之外，請改選 {} 至 {} 之間的區間",
                start.format("%H:%M"),
                end.format("%H:%M")
            )
        }));
    }
    Ok(LeaveWorth {
        hours: (Money::from(seconds) / Money::from(3600)).round_dp(2),
        days: (Money::from(seconds) / Money::from(daily)).round_dp(2),
        off_days,
    })
}

/// Today, or the first working day after it: where a new request starts.
fn next_workday(schedule: &WorkSchedule, from: NaiveDate) -> NaiveDate {
    from.iter_days()
        .find(|d| schedule.day_kind(*d).works())
        .unwrap_or(from)
}

struct PreviewLeave;

#[derive(Deserialize, JsonSchema)]
pub struct PreviewLeaveInput {
    /// When the leave would start, in local wall-clock time.
    pub from_at: NaiveDateTime,
    /// When it would end.
    pub to_at: NaiveDateTime,
}

#[async_trait]
impl Operation for PreviewLeave {
    fn name(&self) -> &'static str {
        "preview_leave"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "試算一段休假區間的時數與天數，結果與送出休假單時存下的數字相同。依上下班時間、休息時間與工作日曆計算。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<PreviewLeaveInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PreviewLeaveInput = parse(input)?;
        let schedule = WorkSchedule::load(ctx).await?;
        let worth = leave_worth(&schedule, i.from_at, i.to_at)?;
        let summary = format!(
            "試算休假 {} 到 {} · {} 小時（{} 日）",
            i.from_at.format("%Y-%m-%d %H:%M"),
            i.to_at.format("%Y-%m-%d %H:%M"),
            worth.hours.normalize(),
            worth.days.normalize()
        );
        Ok(OpOutcome::new(
            json!({
                "hours": worth.hours.to_string(),
                "days": worth.days.to_string(),
                "off_days": worth.off_days,
            }),
            summary,
        ))
    }
}

struct RequestLeave;

#[derive(Deserialize, JsonSchema)]
pub struct RequestLeaveInput {
    /// Leave type. Valid values match the CHECK on `leave_request.kind`.
    pub kind: String,
    /// When the leave starts, in local wall-clock time, `2026-08-25T13:30:00`.
    pub from_at: NaiveDateTime,
    /// When the leave ends. Hours and days are derived from the range and the
    /// company's working window, so neither is accepted as input.
    pub to_at: NaiveDateTime,
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
        "以目前登入帳號所連結的員工身分送出待簽核休假單。收起訖時刻，時數與天數依上下班時間、休息時間與工作日曆計算，非工作日不計入。申請人與建立者都由登入身分決定，輸入不能指定他人。"
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
                "「{}」不是有效的假別。目前有：{values}",
                i.kind
            )));
        }
        let schedule = WorkSchedule::load(ctx).await?;
        let LeaveWorth { hours, days, .. } = leave_worth(&schedule, i.from_at, i.to_at)?;

        let employee = actor_employee(ctx).await?;
        let request = ctx
            .store
            .create_leave_request(NewLeaveRequest {
                employee_id: employee.id,
                kind: &i.kind,
                from_at: i.from_at,
                to_at: i.to_at,
                hours,
                days,
                reason: i.reason.as_deref().unwrap_or(""),
                actor: &ctx.actor,
            })
            .await?;
        let stamp = |at: NaiveDateTime| at.format("%Y-%m-%d %H:%M").to_string();
        let summary = format!(
            "{} · {} {}申請 {} 小時（{} 日）· {} 到 {} · 等簽核",
            request.leave_no,
            employee.name,
            leave_kind_label(&request.kind),
            request.hours.normalize(),
            request.days.normalize(),
            stamp(request.from_at),
            stamp(request.to_at)
        );

        Ok(OpOutcome::new(
            json!({
                "request": leave_request_json(&request),
                "status": "pending",
                "schedule": schedule.json(),
                "hours_source": "起訖時刻落在上下班時間內、扣掉休息時間的部分，工作日曆上的非工作日不計入；天數等於時數除以一天的上班時數。",
                "scope_note": "employee_id 由已驗證 session 的 ctx.employee_id 決定，created_by 來自同一個登入身分。",
            }),
            summary,
        ))
    }
}

struct WithdrawMyLeaveRequest;

#[derive(Deserialize, JsonSchema)]
pub struct WithdrawMyLeaveRequestInput {
    /// Number of one's own leave request still waiting for a decision.
    pub leave_no: String,
    /// Why it is taken back.
    #[serde(default)]
    pub reason: Option<String>,
}

#[async_trait]
impl Operation for WithdrawMyLeaveRequest {
    fn name(&self) -> &'static str {
        "withdraw_my_leave_request"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "撤回本人一張仍在等簽核的休假單，撤回後該段時間可以重新申請。已核准的單要由人資撤銷；別人的單號一律查無此單。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<WithdrawMyLeaveRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: WithdrawMyLeaveRequestInput = parse(input)?;
        let reason = i.reason.as_deref().map(str::trim).unwrap_or("");
        let employee = actor_employee(ctx).await?;
        let request = ctx
            .store
            .withdraw_leave_request(
                &i.leave_no,
                employee.id,
                &ctx.actor,
                &ctx.actor_role,
                reason,
            )
            .await?;

        let summary = format!(
            "{} · {} 撤回{} {} 日{}",
            request.leave_no,
            employee.name,
            leave_kind_label(&request.kind),
            request.days.normalize(),
            reason_suffix(reason)
        );
        Ok(OpOutcome::new(
            json!({
                "request": leave_request_json(&request),
                "scope_note": "所屬員工由 ctx.employee_id 決定並直接寫進 WHERE 條件。",
            }),
            summary,
        ))
    }
}

/// How an optional reason ends an audit summary.
fn reason_suffix(reason: &str) -> String {
    match reason {
        "" => String::new(),
        reason => format!(" · 理由：{reason}"),
    }
}

// My overtime

/// Hours an overtime request covers: the length of the range less the break
/// inside it. The break is not working time on a rest day any more than on a
/// weekday, and a whole rest day is one request across it.
fn requested_overtime_hours(
    schedule: &WorkSchedule,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> Money {
    let seconds = (to - from).num_seconds() - schedule.break_seconds(from, to);
    (Money::from(seconds) / Money::from(3600)).round_dp(2)
}

/// What the monthly ceiling in the overtime ruleset is measured against:
/// every request still holding its hours. A pending request is already
/// spoken for, so counting only the approved ones would offer hours the next
/// submission refuses.
fn committed_overtime_hours(requests: &[OvertimeRequest]) -> Money {
    requests
        .iter()
        .filter(|o| holds_hours(&o.status))
        .map(|o| o.hours)
        .sum()
}

struct ListMyOvertimeRequests;

#[async_trait]
impl Operation for ListMyOvertimeRequests {
    fn name(&self) -> &'static str {
        "list_my_overtime_requests"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀本人送出的加班申請與本月已核准時數。加班費依加班日適用的加班規則現算；輸入不含可指定他人的欄位。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let requests = ctx
            .store
            .overtime_requests(true, Some(employee.id), None)
            .await?;
        let rs = laysi_regulation::resolve("overtime", ctx.today)?;
        let ceiling = laysi_regulation::overtime_monthly_max_hours(rs)?;
        let month = ctx.today.format("%Y-%m").to_string();

        let this_month: Vec<_> = requests
            .iter()
            .filter(|o| o.worked_on.format("%Y-%m").to_string() == month)
            .cloned()
            .collect();
        let approved_hours: Money = this_month
            .iter()
            .filter(|o| o.status == "approved")
            .map(|o| o.hours)
            .sum();
        let pending = requests.iter().filter(|o| o.status == "pending").count();
        let left = (ceiling - committed_overtime_hours(&this_month)).max(Money::ZERO);
        let schedule = WorkSchedule::load(ctx).await?;

        let summary = format!(
            "讀{}的加班申請 · {} 張（{pending} 張等簽）· {month} 已核准 {} 小時",
            employee.name,
            requests.len(),
            approved_hours.normalize()
        );
        Ok(OpOutcome::new(
            json!({
                "employee": { "emp_no": employee.emp_no, "name": employee.name },
                "requests": requests
                    .iter()
                    .map(overtime_json)
                    .collect::<Result<Vec<_>, OpError>>()?,
                "pending_count": pending,
                "month": month,
                "month_approved_hours": approved_hours.normalize().to_string(),
                "monthly_max_hours": ceiling.normalize().to_string(),
                "month_hours_left": left.normalize().to_string(),
                "day_types": laysi_regulation::overtime_day_types(rs)?
                    .iter()
                    .map(|t| json!({
                        "code": t.code,
                        "label": t.label,
                        "max_hours": t.max_hours.normalize().to_string(),
                    }))
                    .collect::<Vec<_>>(),
                "schedule": schedule.json(),
                "calendar": schedule.overtime_exceptions(),
                "ruleset": ruleset_json(rs),
                "scope_note": "僅查詢已驗證 session 的 ctx.employee_id；輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

struct SubmitMyOvertimeRequest;

#[derive(Deserialize, JsonSchema)]
pub struct SubmitMyOvertimeRequestInput {
    /// When the overtime starts, in local wall-clock time,
    /// `2026-08-25T18:00:00`.
    pub from_at: NaiveDateTime,
    /// When it ends. The hours are the length of the range less the break;
    /// nothing else is accepted, so the request cannot claim more than it
    /// covers.
    pub to_at: NaiveDateTime,
    /// Why the work is needed.
    #[serde(default)]
    pub reason: Option<String>,
}

#[async_trait]
impl Operation for SubmitMyOvertimeRequest {
    fn name(&self) -> &'static str {
        "submit_my_overtime_request"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "以目前登入帳號所連結的員工身分送出加班申請。時數為起訖時刻的長度扣掉休息時間，一日與每月上限來自加班規則；核准後才列入薪資。申請人與建立者都由登入身分決定，輸入不能指定他人。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<SubmitMyOvertimeRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SubmitMyOvertimeRequestInput = parse(input)?;
        if i.to_at <= i.from_at {
            return Err(OpError::BadInput("加班結束時刻要晚於開始時刻".into()));
        }
        let worked_on = i.from_at.date();
        let schedule = WorkSchedule::load(ctx).await?;
        let hours = requested_overtime_hours(&schedule, i.from_at, i.to_at);
        if hours <= Money::ZERO {
            return Err(OpError::BadInput(
                "所選時段都在休息時間內，請改填實際加班的時段".into(),
            ));
        }

        let rs = laysi_regulation::resolve("overtime", worked_on)?;
        let day_types = laysi_regulation::overtime_day_types(rs)?;
        // The work calendar decides the day type, never the requester: a
        // weekday priced as a rest day skips the working-hours check below
        // and pays hours the salary already covers.
        let code = schedule.overtime_day_type(worked_on);
        let Some(day_type) = day_types.iter().find(|t| t.code == code) else {
            return Err(OpError::BadInput(format!(
                "加班規則沒有 {worked_on} 適用的日別，請系統管理員到「法規版本」補上"
            )));
        };

        // Normal working hours are already paid by the salary, so a weekday
        // request that overlaps them would be paid twice. Without a stated
        // working window there is nothing to overlap and nothing to check.
        if code == "weekday"
            && schedule
                .leave_seconds(i.from_at, i.to_at)
                .is_some_and(|s| s > 0)
        {
            return Err(OpError::BadInput(format!(
                "所選時段與上班時間（{} 至 {}）重疊，加班要填上班時間以外的時段",
                schedule
                    .starts_at
                    .map(|t| t.format("%H:%M").to_string())
                    .unwrap_or_default(),
                schedule
                    .ends_at
                    .map(|t| t.format("%H:%M").to_string())
                    .unwrap_or_default(),
            )));
        }
        if hours > day_type.max_hours {
            return Err(OpError::BadInput(format!(
                "{}加班一天最多 {} 小時，這張單是 {} 小時，請分成多天申請",
                day_type.label,
                day_type.max_hours.normalize(),
                hours.normalize()
            )));
        }

        let employee = actor_employee(ctx).await?;
        let month = worked_on.format("%Y-%m").to_string();
        let ceiling = laysi_regulation::overtime_monthly_max_hours(rs)?;
        let so_far = committed_overtime_hours(
            &ctx.store
                .overtime_requests(true, Some(employee.id), Some(&month))
                .await?,
        );
        if so_far + hours > ceiling {
            return Err(OpError::BadInput(format!(
                "{month} 的加班時數上限為 {} 小時，已送出 {} 小時，這張單再加 {} 小時就超過了",
                ceiling.normalize(),
                so_far.normalize(),
                hours.normalize()
            )));
        }

        let request = ctx
            .store
            .create_overtime_request(NewOvertimeRequest {
                employee_id: employee.id,
                day_type: code,
                from_at: i.from_at,
                to_at: i.to_at,
                hours,
                reason: i.reason.as_deref().unwrap_or(""),
                actor: &ctx.actor,
            })
            .await?;

        let stamp = |at: NaiveDateTime| at.format("%Y-%m-%d %H:%M").to_string();
        let summary = format!(
            "{} · {} {}加班 {} 小時 · {} 到 {} · 等簽核",
            request.request_no,
            employee.name,
            day_type.label,
            request.hours.normalize(),
            stamp(request.from_at),
            stamp(request.to_at)
        );

        Ok(OpOutcome::new(
            json!({
                "request": overtime_json(&request)?,
                "status": "pending",
                "month_hours_after": (so_far + hours).normalize().to_string(),
                "monthly_max_hours": ceiling.normalize().to_string(),
                "ruleset": ruleset_json(rs),
                "scope_note": "employee_id 由已驗證 session 的 ctx.employee_id 決定，created_by 來自同一個登入身分。",
            }),
            summary,
        ))
    }
}

struct WithdrawMyOvertimeRequest;

#[derive(Deserialize, JsonSchema)]
pub struct WithdrawMyOvertimeRequestInput {
    /// Number of one's own overtime request still waiting for a decision.
    pub request_no: String,
    /// Why it is taken back.
    #[serde(default)]
    pub reason: Option<String>,
}

#[async_trait]
impl Operation for WithdrawMyOvertimeRequest {
    fn name(&self) -> &'static str {
        "withdraw_my_overtime_request"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "撤回本人一張仍在等簽核的加班申請，撤回後該段時間可以重新申請，時數也不再計入當月上限。已核准的單要由人資撤銷；別人的單號一律查無此單。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<WithdrawMyOvertimeRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: WithdrawMyOvertimeRequestInput = parse(input)?;
        let reason = i.reason.as_deref().map(str::trim).unwrap_or("");
        let employee = actor_employee(ctx).await?;
        let request = ctx
            .store
            .withdraw_overtime_request(
                &i.request_no,
                employee.id,
                &ctx.actor,
                &ctx.actor_role,
                reason,
            )
            .await?;

        let summary = format!(
            "{} · {} 撤回加班 {} 小時{}",
            request.request_no,
            employee.name,
            request.hours.normalize(),
            reason_suffix(reason)
        );
        Ok(OpOutcome::new(
            json!({
                "request": overtime_json(&request)?,
                "scope_note": "所屬員工由 ctx.employee_id 決定並直接寫進 WHERE 條件。",
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
        "讀目前呼叫者本人送出的費用報支單，包含各關卡的產生原因、已簽核人員、目前關卡與最終產生的傳票。輸入不含申請人或其他人員欄位。"
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
        let mut receipts_by_claim: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for claim in &claims {
            let receipts = ctx
                .store
                .documents_linked_to("expense_claim", &claim.claim_no)
                .await?;
            receipts_by_claim.insert(&claim.claim_no, receipts.iter().map(receipt_json).collect());
        }
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
                    .map(|claim| {
                        let mut value = claim_json(claim, &rules);
                        let receipts = receipts_by_claim
                            .remove(claim.claim_no.as_str())
                            .unwrap_or_default();
                        value["receipt_count"] = json!(receipts.len());
                        value["accepts_receipt"] =
                            json!(OPEN_CLAIM_STATUSES.contains(&claim.status.as_str()));
                        value["receipts"] = json!(receipts);
                        value
                    })
                    .collect::<Vec<_>>(),
                "waiting_count": waiting_count,
                "waiting_total": waiting.to_string(),
                "scope_note": "先以 ctx.employee_id 取得已連結員工，再查詢該員工姓名送出的報支；輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

// Submitting an expense claim

struct SubmitMyExpenseClaim;

#[derive(Deserialize, JsonSchema)]
pub struct SubmitMyExpenseClaimInput {
    /// What the money was spent on.
    pub title: String,
    /// Amount paid, tax inclusive.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub amount: Money,
    /// Spend date. Defaults to today and may not be in the future.
    #[serde(default)]
    pub spent_on: Option<NaiveDate>,
    /// `reimburse` paid with the month-end run (default), or `petty_cash`
    /// already taken from petty cash.
    #[serde(default)]
    pub pay_method: Option<String>,
    /// Anything the approvers need to know.
    #[serde(default)]
    pub memo: Option<String>,
}

#[async_trait]
impl Operation for SubmitMyExpenseClaim {
    fn name(&self) -> &'static str {
        "submit_my_expense_claim"
    }

    fn risk(&self) -> Risk {
        // Submitting moves no money, but it puts a decision in front of a
        // person, so it is not read-only.
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "以目前登入帳號所連結的員工身分送一張費用報支單。申請人姓名與部門取自該員工主檔，輸入不能指定申請人；費用科目由會計關卡決定，本操作不接受科目與收據文件編號。不產生任何分錄。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<SubmitMyExpenseClaimInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SubmitMyExpenseClaimInput = parse(input)?;
        if i.title.trim().is_empty() {
            return Err(OpError::BadInput("報支項目要填寫".into()));
        }
        if i.amount <= Money::ZERO {
            return Err(OpError::BadInput("報支金額要大於零".into()));
        }
        let pay_method = checked_pay_method(i.pay_method.as_deref())?;
        let spent_on = i.spent_on.unwrap_or(ctx.today);
        if spent_on > ctx.today {
            return Err(OpError::BadInput("消費日期不能晚於今天".into()));
        }

        let employee = actor_employee(ctx).await?;
        let claim = ctx
            .store
            .submit_expense_claim(SubmitExpenseClaim {
                title: i.title.trim(),
                claimant: &employee.name,
                claimant_role: &ctx.actor_role,
                employee_id: Some(employee.id),
                dept_code: employee.dept_code.as_deref(),
                spent_on,
                amount: i.amount,
                suggested_account_code: None,
                pay_method,
                document_id: None,
                memo: i.memo.as_deref().unwrap_or("").trim(),
            })
            .await?;

        let rules = ctx.store.approval_rules().await?;
        let steps = required_steps(&rules, claim.amount);
        let summary = format!(
            "{} · {} {} · {} 送出 · 要經過 {} 關（依公司簽核規則的金額門檻）",
            claim.claim_no,
            claim.title,
            money(claim.amount),
            employee.name,
            steps.len()
        );

        Ok(OpOutcome::new(
            json!({
                "claim": claim_json(&claim, &rules),
                "account_note": "費用科目由會計在「會計確認科目」關卡決定。",
                "scope_note": "claimant 取自已驗證 session 的 ctx.employee_id 所對應的員工姓名，部門取自同一筆員工主檔；輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

// Attaching a receipt to one's own claim

/// What an employee needs to recognise a file they attached. The object key
/// and the bucket stay out: staff hold no document permission, and neither is
/// needed to tell one receipt from another.
pub(crate) fn receipt_json(d: &DocumentRow) -> Value {
    json!({
        "doc_no": d.doc_no,
        "file_name": d.file_name,
        "byte_size": d.byte_size,
        "size_label": human_size(d.byte_size),
        "content_type": d.content_type,
        "checksum_short": short_checksum(&d.checksum),
        "uploaded_at": d.uploaded_at,
    })
}

/// Display label for a claim status. Presentation only.
fn claim_status_label(status: &str) -> &'static str {
    match status {
        "submitted" => "已送出",
        "approving" => "簽核中",
        "approved" => "已核准",
        "rejected" => "已退回",
        _ => "狀態不明",
    }
}

struct AttachMyExpenseReceipt;

#[derive(Deserialize, JsonSchema)]
pub struct AttachMyExpenseReceiptInput {
    /// The claim to attach to, e.g. `EX2608-004`. Only claims filed through
    /// self-service by the caller can be found.
    pub claim_no: String,
    /// File name. Non-ASCII names are kept; the object key is transliterated.
    pub file_name: String,
    /// File contents, base64.
    pub content_base64: String,
}

/// Claim states that still take a receipt. Once accounting has produced a
/// voucher the paperwork behind it should not change underneath.
const OPEN_CLAIM_STATUSES: [&str; 2] = ["submitted", "approving"];

#[async_trait]
impl Operation for AttachMyExpenseReceipt {
    fn name(&self) -> &'static str {
        "attach_my_expense_receipt"
    }

    fn risk(&self) -> Risk {
        // Writes to object storage, and what lands there is never deleted.
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "為本人送出的費用報支單附上收據檔案。內容以 base64 傳入，存進物件儲存並記下內容雜湊；報支單號要屬於目前登入帳號所連結的員工，別人的單號一律查無此單。已核准或已產生傳票的單不再收件。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<AttachMyExpenseReceiptInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: AttachMyExpenseReceiptInput = parse(input)?;
        let objects = ctx.objects()?;
        let employee = actor_employee(ctx).await?;

        let file_name = i.file_name.trim();
        if file_name.is_empty() {
            return Err(OpError::BadInput("檔名要填寫".into()));
        }
        let body = base64::engine::general_purpose::STANDARD
            .decode(i.content_base64.trim())
            .map_err(|e| OpError::BadInput(format!("檔案內容不是合法的 base64：{e}")))?;
        if body.is_empty() {
            return Err(OpError::BadInput("檔案是空的".into()));
        }
        if body.len() > MAX_OBJECT_BYTES {
            return Err(OpError::BadInput(format!(
                "檔案 {} 位元組，超過單檔上限 {} 位元組",
                body.len(),
                MAX_OBJECT_BYTES
            )));
        }

        // The employee is in the WHERE clause, so somebody else's claim number
        // comes back as not found rather than as a refusal that would confirm
        // the number exists.
        let claim = ctx
            .store
            .expense_claim_of_employee(i.claim_no.trim(), employee.id)
            .await?;
        if !OPEN_CLAIM_STATUSES.contains(&claim.status.as_str()) {
            return Err(OpError::BadInput(format!(
                "{} 目前是「{}」，已經不能再附收據，請洽會計",
                claim.claim_no,
                claim_status_label(&claim.status)
            )));
        }

        let slot = ctx
            .store
            .reserve_document_slot(ReserveDocument {
                doc_no: None,
                name: file_name,
                kind: "expense_receipt",
                link_type: "expense_claim",
                link_ref: &claim.claim_no,
                note: format!("{} 的收據", claim.claim_no).as_str(),
                today: ctx.today,
                actor: &ctx.actor,
            })
            .await?;

        let key = storage::object_key(
            &ctx.today.format("%y%m").to_string(),
            &slot.doc_no,
            slot.version_no,
            file_name,
        );
        let content_type = storage::guess_content_type(file_name);
        let stored = objects.put(&key, &body, content_type, file_name).await?;
        let document = ctx
            .store
            .record_document_version(&slot, &stored, file_name, &ctx.actor)
            .await?;
        ctx.store
            .set_claim_document_if_absent(&claim.claim_no, slot.document_id)
            .await?;

        let receipts = ctx
            .store
            .documents_linked_to("expense_claim", &claim.claim_no)
            .await?;
        let summary = format!(
            "{} 附上收據 {} · {} · 目前共 {} 份",
            claim.claim_no,
            document.doc_no,
            document.file_name,
            receipts.len()
        );

        Ok(OpOutcome::new(
            json!({
                "claim_no": claim.claim_no,
                "receipt": receipt_json(&document),
                "receipts": receipts.iter().map(receipt_json).collect::<Vec<_>>(),
                "scope_note": "報支單以 ctx.employee_id 直接寫進 WHERE 條件取得，別人的單號查不到；檔案內容不經解讀，只計算 BLAKE3 供日後比對。",
            }),
            summary,
        ))
    }
}

// Clocking in and out

struct PunchClock;

#[derive(Deserialize, JsonSchema)]
pub struct PunchClockInput {
    /// `in` to start the day, `out` to end it.
    pub kind: String,
    /// Anything worth recording, such as why the arrival was late.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for PunchClock {
    fn name(&self) -> &'static str {
        "punch_clock"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "以目前登入帳號所連結的員工身分打上班或下班卡。時間一律取伺服器當下的時刻並記錄到秒，輸入不能指定時間，也不能指定員工。一天只記一次上班與一次下班；漏打或打錯請送出補登申請，或由人資補登。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<PunchClockInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PunchClockInput = parse(input)?;
        if !PUNCH_KINDS.contains(&i.kind.as_str()) {
            return Err(OpError::BadInput(format!(
                "打卡別只接受 in（上班）或 out（下班），收到「{}」",
                i.kind
            )));
        }

        // The real clock, not `ctx.today`. A punch records when something
        // actually happened, so `LAYSI_TODAY` must not move it; the database
        // also checks that the day and the timestamp agree.
        let now = Utc::now();
        let local = laysi_domain::business_time(now, ctx.offset);
        let employee = actor_employee(ctx).await?;
        let punch = ctx
            .store
            .record_punch(
                laysi_store::NewPunch {
                    employee_id: employee.id,
                    punched_on: local.date(),
                    punched_at: now,
                    kind: &i.kind,
                    source: "self",
                    note: i.note.as_deref().unwrap_or("").trim(),
                    actor: &ctx.actor,
                    offset: ctx.offset,
                },
                None,
            )
            .await?;

        let schedule = WorkSchedule::load(ctx).await?;
        let limits = OvertimeLimits::resolve(ctx.today)?;
        let month = local.date().format("%Y-%m").to_string();
        let days = punch_days(
            &ctx.store.punches(&month, Some(employee.id)).await?,
            ctx.offset,
        );
        let today = days.iter().find(|d| d.date == local.date());
        let late = today
            .and_then(|d| d.arrived)
            .and_then(|arrived| schedule.late(arrived));

        let summary = format!(
            "{} {} 打{}卡 {}{}",
            employee.emp_no,
            employee.name,
            punch_kind_label(&i.kind),
            local.format(PUNCH_TIME),
            match late {
                Some(true) => "（遲到）",
                Some(false) => "",
                None => "",
            }
        );

        Ok(OpOutcome::new(
            json!({
                "punch": {
                    "punched_on": punch.punched_on,
                    "at": local.format(PUNCH_TIME).to_string(),
                    "kind": punch.kind,
                    "kind_label": punch_kind_label(&punch.kind),
                    "source": punch.source,
                },
                "today": today.map(|d| d.json(&schedule, &limits)),
                "schedule": schedule.json(),
                "scope_note": "員工由已驗證 session 的 ctx.employee_id 決定，時間取自伺服器時鐘；輸入不含員工欄位，也不含時間欄位。",
            }),
            summary,
        ))
    }
}

// My punches

struct ListMyPunches;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListMyPunchesInput {
    /// Month (YYYY-MM). Defaults to this month. This chooses a period, never
    /// a person: the person is always ctx.employee_id.
    #[serde(default)]
    pub month: Option<String>,
}

#[async_trait]
impl Operation for ListMyPunches {
    fn name(&self) -> &'static str {
        "list_my_punches"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀本人某個月的逐日打卡明細與統計，並與匯入的出勤月統計對帳。輸入只有月份，沒有任何可指定他人的欄位。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListMyPunchesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListMyPunchesInput = parse(input)?;
        let month = i
            .month
            .unwrap_or_else(|| ctx.today.format("%Y-%m").to_string());
        if NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").is_err() {
            return Err(OpError::BadInput(format!(
                "月份要寫成 YYYY-MM，收到的是「{month}」"
            )));
        }

        let employee = actor_employee(ctx).await?;
        let schedule = WorkSchedule::load(ctx).await?;
        let punched = punch_days(
            &ctx.store.punches(&month, Some(employee.id)).await?,
            ctx.offset,
        );
        let limits = OvertimeLimits::resolve(ctx.today)?;

        let local_today = laysi_domain::business_time(Utc::now(), ctx.offset).date();
        // Days without a punch are rows too: they are the ones a correction is
        // asked for, and the screen offers the correction on the day itself.
        let mut days = month_days(
            punched,
            &month,
            employee.hired_on,
            employee.left_on.unwrap_or(local_today).min(local_today),
        );
        let leaves = ctx
            .store
            .leave_requests_matching(true, Some(employee.id), Some(month_span(&month)))
            .await?;
        let awaiting: Vec<NaiveDate> = ctx
            .store
            .punch_corrections(false, Some(employee.id))
            .await?
            .iter()
            .map(|c| c.punched_on)
            .collect();
        mark_days(
            &mut days,
            &leaves.iter().collect::<Vec<_>>(),
            &awaiting,
            &schedule,
            judges_absence(&schedule, &employee.employment_type, &employee.insured_as),
            local_today,
        );
        let stats = summarise(&days, &schedule, &limits);

        // The card at the top is always about today, so browsing an earlier
        // month must not answer "what next" from a month today is not in.
        let this_month = local_today.format("%Y-%m").to_string();
        let elsewhere = match this_month == month {
            true => None,
            false => punch_days(
                &ctx.store.punches(&this_month, Some(employee.id)).await?,
                ctx.offset,
            )
            .into_iter()
            .find(|d| d.date == local_today),
        };

        // What the button on screen should offer next. Derived here so the
        // frontend does not have to reimplement the one-per-day rule.
        let today = elsewhere
            .as_ref()
            .or_else(|| days.iter().find(|d| d.date == local_today));
        let next_kind = match today {
            Some(day) if day.arrived.is_none() => Some("in"),
            Some(day) if day.left.is_none() => Some("out"),
            Some(_) => None,
            None => Some("in"),
        };

        let summary = format!(
            "讀 {} 的 {month} 打卡 · 打滿 {} 天、共 {} 小時{}",
            employee.name,
            stats.complete_days,
            stats.json()["worked_hours"].as_str().unwrap_or("0"),
            match stats.late_days {
                Some(late) => format!(" · 遲到 {late} 天"),
                None => " · 未設定上班時間，不判定遲到".into(),
            }
        );

        Ok(OpOutcome::new(
            json!({
                "employee": { "emp_no": employee.emp_no, "name": employee.name },
                "month": month,
                "today": local_today,
                "today_punches": today.map(|d| d.json(&schedule, &limits)),
                "next_kind": next_kind,
                "next_kind_label": next_kind.map(punch_kind_label),
                "days": days.iter().map(|d| d.json(&schedule, &limits)).collect::<Vec<_>>(),
                "summary": stats.json(),
                "schedule": schedule.json(),
                "scope_note": "僅查詢已驗證 session 的 ctx.employee_id；輸入只有月份。",
            }),
            summary,
        ))
    }
}

// Connecting an assistant as yourself

/// The MCP tools this role can actually reach, and why the others are out.
///
/// Two independent gates decide it and they fail for different reasons, so
/// the reply names both rather than returning one merged list: a tool the
/// role lacks permission for is a matter for the owner to change, and a tool
/// switched off for AI is a matter for the MCP screen.
async fn my_mcp_tools(ctx: &OpCtx) -> Result<Value, OpError> {
    let permitted = ctx.store.permitted_ops(&ctx.actor_role).await?;
    let grants = ctx.store.tool_grants().await?;
    let mut usable = Vec::new();
    let mut blocked = Vec::new();
    for (tool, enabled) in grants {
        if !permitted.contains(&tool) {
            continue;
        }
        if enabled {
            usable.push(tool);
        } else {
            blocked.push(tool);
        }
    }
    usable.sort();
    blocked.sort();
    let role_name = ctx
        .store
        .roles()
        .await?
        .into_iter()
        .find(|r| r.code == ctx.actor_role)
        .map(|r| r.name)
        .unwrap_or_else(|| ctx.actor_role.clone());
    Ok(json!({
        "usable": usable,
        "blocked_for_ai": blocked,
        "role": ctx.actor_role,
        "role_name": role_name,
        // Where to go next is left out on purpose: the role is fixed when
        // the account is created, the permission matrix is the
        // administrator's, and the tool switches have no operation behind
        // them. Naming any of the three would send the reader somewhere that
        // cannot help.
        "note": "可用工具為本人角色權限與 MCP 工具開關的交集。",
    }))
}

struct IssueMyApiToken;

#[derive(Deserialize, JsonSchema)]
pub struct IssueMyApiTokenInput {
    /// What to call this token, so it can be told apart in the list later.
    /// Named `label` rather than `name` because in this module every field
    /// called `name` has meant a person, and the test that forbids those
    /// should not have to carry an exception.
    pub label: String,
    /// What it is for, kept as a label. It records intent and does not
    /// restrict anything; the role does that.
    #[serde(default)]
    pub scope: Option<String>,
    /// Days until rotation is due. Omitted means no fixed cycle.
    #[serde(default)]
    pub rotate_days: Option<i32>,
}

#[async_trait]
impl Operation for IssueMyApiToken {
    fn name(&self) -> &'static str {
        "issue_my_api_token"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "以目前登入者本人的身分核發一組 MCP Token。輸入只有名稱、用途與輪替天數，不能指定代表對象與角色，兩者都取自 session，因此這組 Token 能做的事與本人完全相同。明文僅回傳一次。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<IssueMyApiTokenInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: IssueMyApiTokenInput = parse(input)?;
        if i.label.trim().is_empty() {
            return Err(OpError::BadInput(
                "Token 要有名稱，否則清單上無法分辨是哪一組".into(),
            ));
        }
        if i.rotate_days.is_some_and(|d| d <= 0) {
            return Err(OpError::BadInput("輪替天數要大於零".into()));
        }

        let employee = actor_employee(ctx).await?;
        let expiry_note = match i.rotate_days {
            Some(d) => format!("{d} 天後輪替"),
            None => "未設定輪替週期".into(),
        };
        let issued = ctx
            .store
            .issue_api_token(laysi_store::IssueApiToken {
                name: i.label.trim(),
                kind: "mcp",
                scope: i.scope.as_deref().unwrap_or("與本人相同").trim(),
                actor: &ctx.actor,
                actor_role: &ctx.actor_role,
                created_by: &ctx.actor,
                rotate_days: i.rotate_days,
                expiry_note: &expiry_note,
                owner_employee_id: Some(employee.id),
                owner_user_id: None,
            })
            .await?;

        let summary = format!(
            "{} 自助核發 MCP Token · {}（{}…）· 權限與本人相同 · {expiry_note}",
            employee.name, issued.token.name, issued.token.prefix
        );

        Ok(OpOutcome::new(
            json!({
                "token": issued.token,
                "secret": issued.secret,
                "tools": my_mcp_tools(ctx).await?,
                "secret_note": "明文只顯示一次，請立即複製保存；系統只留雜湊，無法回查。",
                "scope_note": "代表對象與角色取自已驗證 session，輸入無法指定；這組 Token 的權限與本人相同。",
            }),
            summary,
        ))
    }
}

struct ListMyApiTokens;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListMyApiTokensInput {
    /// Include tokens already revoked.
    #[serde(default)]
    pub include_revoked: bool,
}

#[async_trait]
impl Operation for ListMyApiTokens {
    fn name(&self) -> &'static str {
        "list_my_api_tokens"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀本人自助核發的 MCP Token，以及本人在 MCP 上實際能呼叫的工具清單。不含負責人代發的 Token，輸入不含可指定他人的欄位。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListMyApiTokensInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListMyApiTokensInput = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let tokens = ctx
            .store
            .api_tokens_owned_by(employee.id, i.include_revoked)
            .await?;
        let now = chrono::Utc::now();
        let live = tokens
            .iter()
            .filter(|t| t.revoked_at.is_none() && !t.is_expired(now))
            .count();

        let rows: Vec<Value> = tokens
            .iter()
            .map(|t| {
                let expired = t.is_expired(now);
                json!({
                    "id": t.id,
                    "name": t.name,
                    "scope": t.scope,
                    "prefix": t.prefix,
                    "created_at": t.created_at,
                    "last_used_at": t.last_used_at,
                    "rotate_days": t.rotate_days,
                    "expires_at": t.expires_at(),
                    "expired": expired,
                    "expiry_note": t.expiry_note,
                    "revoked_at": t.revoked_at,
                    "live": t.revoked_at.is_none() && !expired,
                })
            })
            .collect();

        let tools = my_mcp_tools(ctx).await?;
        let summary = format!(
            "讀 {} 自助核發的 MCP Token · {live} 組有效、共 {} 筆 · 可用工具 {} 項",
            employee.name,
            rows.len(),
            tools["usable"].as_array().map_or(0, Vec::len)
        );

        Ok(OpOutcome::new(
            json!({
                "employee": { "emp_no": employee.emp_no, "name": employee.name },
                "tokens": rows,
                "live_count": live,
                "tools": tools,
                "endpoint": "/mcp",
                "scope_note": "只列出 owner_employee_id 等於 ctx.employee_id 的 Token；負責人代發的不在此列。",
            }),
            summary,
        ))
    }
}

struct RevokeMyApiToken;

#[derive(Deserialize, JsonSchema)]
pub struct RevokeMyApiTokenInput {
    /// Token id from `list_my_api_tokens`.
    pub id: i32,
}

#[async_trait]
impl Operation for RevokeMyApiToken {
    fn name(&self) -> &'static str {
        "revoke_my_api_token"
    }

    fn risk(&self) -> Risk {
        // Revoking moves in the safe direction, and a leaked token must not
        // wait for anybody. It is still not read-only: the connection stops.
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "撤銷一組本人自助核發的 MCP Token。只能撤銷本人核發的 Token，別人核發的一律查無此筆。撤銷後不可復原。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<RevokeMyApiTokenInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RevokeMyApiTokenInput = parse(input)?;
        let employee = actor_employee(ctx).await?;
        let token = ctx
            .store
            .revoke_own_api_token(i.id, employee.id, &ctx.actor)
            .await?;
        let summary = format!(
            "{} 撤銷本人的 MCP Token · {}（{}…）· 下一次呼叫立即失效",
            employee.name, token.name, token.prefix
        );
        Ok(OpOutcome::new(
            json!({
                "token": { "id": token.id, "name": token.name, "prefix": token.prefix,
                           "revoked_at": token.revoked_at },
                "scope_note": "所屬員工由 ctx.employee_id 決定並直接寫進 WHERE 條件。",
            }),
            summary,
        ))
    }
}

// My punch corrections
//
// The person who knows a punch is missing is the person who cannot record
// one. These two put that back within reach without letting anybody reach
// another person's day: neither input names an employee, so the subject can
// only be ctx.employee_id.

struct SubmitMyPunchCorrection;

#[derive(Deserialize, JsonSchema)]
pub struct SubmitMyPunchCorrectionInput {
    /// The working day the punch belongs to.
    pub punched_on: NaiveDate,
    /// `in` for arrival, `out` for departure.
    pub kind: String,
    /// The time it was, `HH:MM:SS` or `HH:MM`. A remembered time rarely has
    /// a second, so `HH:MM` is stored as second zero.
    pub at: String,
    /// Why it is being asked for. Kept on the record and shown to whoever
    /// decides.
    #[serde(default)]
    pub reason: Option<String>,
}

#[async_trait]
impl Operation for SubmitMyPunchCorrection {
    fn name(&self) -> &'static str {
        "submit_my_punch_correction"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "以目前登入帳號所連結的員工身分送出打卡補登申請。對象與建立者都由登入身分決定，輸入不能指定他人。公司設定為自動通過時，在同一個交易寫進打卡紀錄。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<SubmitMyPunchCorrectionInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SubmitMyPunchCorrectionInput = parse(input)?;
        if !PUNCH_KINDS.contains(&i.kind.as_str()) {
            return Err(OpError::BadInput(format!(
                "打卡別只接受 in（上班）或 out（下班），收到「{}」",
                i.kind
            )));
        }
        let text = i.at.trim();
        let at = NaiveTime::parse_from_str(text, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M"))
            .map_err(|_| {
                OpError::BadInput(format!(
                    "時間要寫成 HH:MM:SS 或 HH:MM，收到的是「{}」",
                    i.at
                ))
            })?;
        if i.punched_on > ctx.today {
            return Err(OpError::BadInput("不能為未來的日期送出補登申請".into()));
        }

        let employee = actor_employee(ctx).await?;
        let auto = correction_auto_approves(ctx).await?;
        let saved = ctx
            .store
            .submit_punch_correction(
                NewPunchCorrection {
                    employee_id: employee.id,
                    punched_on: i.punched_on,
                    kind: &i.kind,
                    punched_at: business_instant(i.punched_on, at, ctx.offset),
                    reason: i.reason.as_deref().unwrap_or("").trim(),
                    actor: &ctx.actor,
                    offset: ctx.offset,
                },
                auto,
            )
            .await?;

        let summary = format!(
            "{} {} 的{}卡補登申請 {} · {}",
            employee.emp_no,
            saved.punched_on,
            punch_kind_label(&saved.kind),
            business_time(saved.punched_at, ctx.offset).format(PUNCH_TIME),
            punch_correction_status_label(&saved.status)
        );
        Ok(OpOutcome::new(
            json!({
                "correction": punch_correction_json(&saved, ctx.offset),
                "punch_written": auto,
                "next_note": match auto {
                    true => "已寫進打卡紀錄，來源標示為補登。",
                    false => "人資決定後才會寫進打卡紀錄。",
                },
                "scope_note": "employee_id 由已驗證 session 的 ctx.employee_id 決定，輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

struct ListMyPunchCorrections;

#[async_trait]
impl Operation for ListMyPunchCorrections {
    fn name(&self) -> &'static str {
        "list_my_punch_corrections"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀本人送出的打卡補登申請與目前狀態。只回登入身分所連結的員工，輸入不含任何欄位。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let employee = actor_employee(ctx).await?;
        let corrections = ctx.store.punch_corrections(true, Some(employee.id)).await?;
        let waiting = corrections
            .iter()
            .filter(|c| c.status == "submitted")
            .count();
        let auto = correction_auto_approves(ctx).await?;

        let summary = format!(
            "讀本人補登申請 · {} 張（{waiting} 張待審核）",
            corrections.len()
        );
        Ok(OpOutcome::new(
            json!({
                "corrections": corrections
                    .iter()
                    .map(|c| punch_correction_json(c, ctx.offset))
                    .collect::<Vec<_>>(),
                "waiting_count": waiting,
                "auto_approve": auto,
                "scope_note": "僅查詢已驗證 session 的 ctx.employee_id；輸入不含可指定他人的欄位。",
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListMyNoticesInput {
    /// One notice, from a previous listing. Given, the reply is that notice
    /// with every version it has ever had instead of the list.
    #[serde(default)]
    pub id: Option<i32>,
}

struct ListMyNotices;

#[async_trait]
impl Operation for ListMyNotices {
    fn name(&self) -> &'static str {
        "list_my_notices"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀目前公告中與施行中的公告與規章，並標出本人還沒看過的；帶 id 則回該則的每一個版本"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListMyNoticesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListMyNoticesInput = parse(input)?;
        if let Some(id) = i.id {
            let notice = ctx.store.notice(id).await?.ok_or_else(missing_notice)?;
            return history_outcome(
                ctx,
                &notice,
                Some("修訂紀錄不分身分，看得到公告與規章的人讀到的是同一份。"),
            )
            .await;
        }

        let employee = actor_employee(ctx).await?;
        let read_at = ctx.store.notices_read_at(employee.id).await?;
        let rows = ctx.store.current_notices(ctx.today).await?;
        // Never opened it: everything is still to be read. A first visit that
        // claimed nothing was new would hide the rules that were posted
        // before the person joined.
        let unread = |n: &laysi_store::Notice| read_at.is_none_or(|at| n.published_at > at);
        let unread_count = rows.iter().filter(|n| unread(n)).count();

        let summary = format!(
            "讀本人公告與規章 · {} 則（未讀 {unread_count} 則）",
            rows.len()
        );
        Ok(OpOutcome::new(
            json!({
                "notices": rows
                    .iter()
                    .map(|n| {
                        let mut value = notice_json(n, ctx.today);
                        value["unread"] = json!(unread(n));
                        value
                    })
                    .collect::<Vec<_>>(),
                "unread_count": unread_count,
                "read_at": read_at,
                "scope_note": "已讀標記綁定已驗證 session 的 ctx.employee_id，僅本人看得到。",
            }),
            summary,
        ))
    }
}

struct MarkMyNoticesRead;

#[async_trait]
impl Operation for MarkMyNoticesRead {
    fn name(&self) -> &'static str {
        "mark_my_notices_read"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "把目前的公告與規章標為本人已看過。只記時刻，不記看了哪幾則，人資無法讀取這一筆。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let employee = actor_employee(ctx).await?;
        let read_at = ctx.store.mark_notices_read(employee.id).await?;
        Ok(OpOutcome::new(
            json!({
                "read_at": read_at,
                "unread_count": 0,
                "scope_note": "寫在 ctx.employee_id，輸入不含任何欄位。",
            }),
            "標記公告與規章為已看過".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The monthly ceiling counts pending requests, so the hours a second
    /// request may still claim are not the approved ones alone. Counting only
    /// approved hours would let the screen offer hours the submission refuses.
    #[test]
    fn requested_overtime_leaves_the_break_out() {
        use chrono::{NaiveDate, NaiveTime};

        let at = |day: u32, h: u32| {
            NaiveDate::from_ymd_opt(2026, 7, day)
                .unwrap()
                .and_hms_opt(h, 0, 0)
                .unwrap()
        };
        let schedule = WorkSchedule {
            starts_at: NaiveTime::from_hms_opt(9, 0, 0),
            ends_at: NaiveTime::from_hms_opt(17, 0, 0),
            grace_minutes: 0,
            grace_stated: false,
            flex_minutes: 0,
            break_start: NaiveTime::from_hms_opt(12, 0, 0),
            break_end: NaiveTime::from_hms_opt(13, 0, 0),
            calendar: BTreeMap::new(),
        };
        let hours = |from, to| requested_overtime_hours(&schedule, from, to);
        assert_eq!(
            hours(at(11, 9), at(11, 18)),
            Money::from(8),
            "週六整天扣掉午休"
        );
        assert_eq!(
            hours(at(7, 17), at(7, 20)),
            Money::from(3),
            "下班後的加班沒有午休可扣"
        );
        assert_eq!(hours(at(11, 12), at(11, 13)), Money::ZERO, "整段都在午休");
    }

    #[test]
    fn leave_worth_leaves_out_the_break_and_days_off() {
        use crate::ops::attendance::DayKind;
        use chrono::NaiveTime;

        let at = |day: u32, h: u32| {
            NaiveDate::from_ymd_opt(2026, 9, day)
                .unwrap()
                .and_hms_opt(h, 0, 0)
                .unwrap()
        };
        let holiday = NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
        let schedule = WorkSchedule {
            starts_at: NaiveTime::from_hms_opt(9, 0, 0),
            ends_at: NaiveTime::from_hms_opt(18, 0, 0),
            grace_minutes: 0,
            grace_stated: false,
            flex_minutes: 0,
            break_start: NaiveTime::from_hms_opt(12, 0, 0),
            break_end: NaiveTime::from_hms_opt(13, 0, 0),
            calendar: BTreeMap::from([(holiday, (DayKind::Holiday, "中秋節".into()))]),
        };
        let worth = |from, to| {
            let w = leave_worth(&schedule, from, to).unwrap();
            (w.hours, w.days, w.off_days)
        };
        let n = |s: &str| s.parse::<Money>().unwrap();

        assert_eq!(worth(at(24, 9), at(24, 18)), (n("8"), n("1"), 0), "整天");
        assert_eq!(
            worth(at(24, 11), at(24, 14)),
            (n("2"), n("0.25"), 0),
            "跨午休"
        );
        assert_eq!(
            worth(at(24, 9), at(25, 18)),
            (n("8"), n("1"), 1),
            "週四到中秋節"
        );
        assert_eq!(
            worth(at(25, 9), at(28, 18)),
            (n("8"), n("1"), 3),
            "中秋節、週六、週日都不計"
        );
        assert_eq!(worth(at(24, 9), at(25, 0)).2, 0, "結束在午夜，不算進下一天");
        assert!(leave_worth(&schedule, at(26, 9), at(27, 18)).is_err());
        assert!(leave_worth(&schedule, at(24, 18), at(24, 9)).is_err());
        let far = NaiveDate::from_ymd_opt(262_000, 1, 1)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        assert!(leave_worth(&schedule, at(24, 9), far).is_err(), "區間過長");
        assert_eq!(next_workday(&schedule, holiday), at(28, 0).date());
    }

    #[test]
    fn committed_overtime_hours_counts_pending_and_drops_what_was_given_back() {
        let one = |status: &str, hours: &str| OvertimeRequest {
            request_no: "OT2608-001".into(),
            employee_id: 1,
            emp_no: "E001".into(),
            name: "王小明".into(),
            dept_label: None,
            day_type: "weekday".into(),
            pay_basis: crate::ops::employees::BASIS_MONTHLY.into(),
            from_at: NaiveDate::from_ymd_opt(2026, 8, 25)
                .unwrap()
                .and_hms_opt(18, 0, 0)
                .unwrap(),
            to_at: NaiveDate::from_ymd_opt(2026, 8, 25)
                .unwrap()
                .and_hms_opt(20, 0, 0)
                .unwrap(),
            worked_on: NaiveDate::from_ymd_opt(2026, 8, 25).unwrap(),
            hours: hours.parse().unwrap(),
            base_salary: Money::ZERO,
            reason: String::new(),
            status: status.into(),
            decided_by: String::new(),
            decided_role: String::new(),
            decided_at: None,
            decision_note: String::new(),
            cancelled: None,
            created_at: Utc::now(),
            created_by: "e001@example.com".into(),
        };
        let rows = vec![
            one("approved", "3"),
            one("pending", "2"),
            one("rejected", "8"),
            one("withdrawn", "4"),
            one("revoked", "1"),
        ];
        assert_eq!(
            committed_overtime_hours(&rows),
            "5".parse::<Money>().unwrap()
        );
    }

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
    /// operation takes empty input, and the two write operations accept only
    /// the facts of the request itself. Adding any field naming a person must
    /// make this test fail first.
    #[test]
    fn no_self_service_input_can_select_another_employee() {
        let expected: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::from([
            ("get_my_home", BTreeSet::new()),
            ("list_my_payslips", BTreeSet::new()),
            ("list_my_leave_requests", BTreeSet::new()),
            ("preview_leave", BTreeSet::from(["from_at", "to_at"])),
            (
                "request_leave",
                BTreeSet::from(["kind", "from_at", "to_at", "reason"]),
            ),
            // The number names a request, and requests belong to people, so
            // the employee is part of the SQL WHERE clause, as with a token.
            (
                "withdraw_my_leave_request",
                BTreeSet::from(["leave_no", "reason"]),
            ),
            ("list_my_overtime_requests", BTreeSet::new()),
            // The hours are the length of the range and the day type comes
            // from the work calendar, so there is no field to inflate the pay
            // with either.
            (
                "submit_my_overtime_request",
                BTreeSet::from(["from_at", "to_at", "reason"]),
            ),
            (
                "withdraw_my_overtime_request",
                BTreeSet::from(["request_no", "reason"]),
            ),
            ("list_my_expense_claims", BTreeSet::new()),
            (
                "submit_my_expense_claim",
                BTreeSet::from(["title", "amount", "spent_on", "pay_method", "memo"]),
            ),
            // `claim_no` names a claim, and claims belong to people, so it is
            // the same shape of risk as revoking a token by id. The employee
            // is therefore part of the SQL WHERE clause. `file_name` is the
            // name of a file, not of a person; nothing in this input can point
            // at an existing document, which is what kept the claim operation
            // from taking a document number.
            (
                "attach_my_expense_receipt",
                BTreeSet::from(["claim_no", "file_name", "content_base64"]),
            ),
            ("punch_clock", BTreeSet::from(["kind", "note"])),
            // `month` picks a period, not a person. It is the only field in
            // this map that narrows a query at all, and it cannot widen one:
            // whichever month is asked for, the rows still come from
            // ctx.employee_id.
            ("list_my_punches", BTreeSet::from(["month"])),
            // `punched_on`, `kind` and `at` are the facts of the request;
            // none of them can point at another person's day, because the
            // employee is ctx.employee_id and never an input.
            (
                "submit_my_punch_correction",
                BTreeSet::from(["punched_on", "kind", "at", "reason"]),
            ),
            ("list_my_punch_corrections", BTreeSet::new()),
            // `id` names one notice, and a notice belongs to nobody: the
            // board is the same board for everyone, and this operation only
            // reads. It is the one entry here that does not narrow by person
            // because there is no person in it to narrow by.
            ("list_my_notices", BTreeSet::from(["id"])),
            ("mark_my_notices_read", BTreeSet::new()),
            (
                "issue_my_api_token",
                BTreeSet::from(["label", "scope", "rotate_days"]),
            ),
            ("list_my_api_tokens", BTreeSet::from(["include_revoked"])),
            // `id` names a token, which is the one input in this group that
            // could point at something belonging to somebody else. The
            // employee is therefore part of the SQL WHERE clause rather than
            // an `if` after the row is found.
            ("revoke_my_api_token", BTreeSet::from(["id"])),
        ]);
        // dept_code is here for the same reason as the names: the general
        // expense operation takes one, and charging another department's cost
        // centre is the same problem wearing a different word.
        let forbidden = [
            "employee_id",
            "emp_no",
            "name",
            "actor",
            "claimant",
            "user_id",
            "dept_code",
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

    /// A receipt is evidence for a decision that has not been made yet. Once
    /// the claim is approved or rejected the paperwork behind it is fixed, so
    /// the screen offers no attachment and the operation refuses one.
    #[test]
    fn a_settled_claim_takes_no_more_receipts() {
        assert!(OPEN_CLAIM_STATUSES.contains(&"submitted"));
        assert!(OPEN_CLAIM_STATUSES.contains(&"approving"));
        assert!(!OPEN_CLAIM_STATUSES.contains(&"approved"));
        assert!(!OPEN_CLAIM_STATUSES.contains(&"rejected"));
    }

    #[test]
    fn bank_accounts_are_masked_again_before_they_leave_the_operation() {
        assert_eq!(masked_account_no("3812-0000-5508"), "尾碼 5508");
        assert_eq!(masked_account_no("5508"), "已遮罩");
        assert_eq!(masked_account_no(""), "已遮罩");
    }

    /// The person's own screen lists their dependants by name and relation,
    /// and leaves out what would identify them anywhere else.
    #[test]
    fn the_profile_lists_dependants_without_their_id_number_or_birthday() {
        let mut me = crate::ops::insurance::test_employee(
            "active",
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
        );
        me.dependents = vec![laysi_store::EmployeeDependent {
            id: 1,
            name: "王小寶".into(),
            relation: "child".into(),
            id_no: "B223456782".into(),
            birthday: NaiveDate::from_ymd_opt(2026, 3, 1),
            health_enrolled_from: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            health_enrolled_until: None,
        }];
        let profile = profile_json(&me, 0);
        let listed = &profile["dependents"][0];
        assert_eq!(listed["name"], json!("王小寶"));
        assert_eq!(listed["relation_label"], json!("子女"));
        assert_eq!(listed["health_enrolled_from"], json!("2026-03-01"));
        assert!(listed.get("id_no").is_none());
        assert!(listed.get("birthday").is_none());
        let text = profile.to_string();
        assert!(!text.contains("B223456782"), "{text}");
    }
}
