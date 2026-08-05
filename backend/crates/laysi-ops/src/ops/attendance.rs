//! Attendance, leave requests and approvals.
//!
//! The ruleset library does not yet carry overtime multipliers, so
//! [`GetAttendance`] passes through the `overtime_pay` supplied by the
//! timekeeping system without recomputing it.
//!
//! The database stores only approved annual leave days. The entitlement is
//! derived from the `annual_leave` ruleset and completed months of service;
//! for departed employees service stops at the termination date.
//!
//! [`DecideLeaveRequest`] does not read the status first at the operation
//! layer. Whether the request is still pending is decided by the store during
//! the update, so two managers acting at once cannot both win.

use crate::ops::employees::service_months;
use crate::ops::insurance::ruleset_json;
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_regulation::Ruleset;
use laysi_store::LeaveRequest;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetAttendance),
        Box::new(ListLeaveRequests),
        Box::new(DecideLeaveRequest),
    ]
}

fn valid_month(month: &str) -> bool {
    chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").is_ok()
}

/// Values accepted by `leave_request.kind`. A migration test compares this
/// against the CHECK verbatim, so a new leave type cannot quietly fall
/// through to "other" on screen.
pub(crate) const LEAVE_KINDS: &[&str] = &[
    "annual",
    "sick",
    "personal",
    "official",
    "marriage",
    "bereavement",
    "maternity",
    "menstrual",
];

pub(crate) fn valid_leave_kind(kind: &str) -> bool {
    LEAVE_KINDS.contains(&kind)
}

pub(crate) fn leave_kind_label(kind: &str) -> &'static str {
    match kind {
        "annual" => "特休",
        "sick" => "病假",
        "personal" => "事假",
        "official" => "公假",
        "marriage" => "婚假",
        "bereavement" => "喪假",
        "maternity" => "產假",
        "menstrual" => "生理假",
        _ => "其他",
    }
}

pub(crate) fn leave_status_label(status: &str) -> &'static str {
    match status {
        "approved" => "已准假",
        "rejected" => "已駁回",
        _ => "等簽核",
    }
}

#[derive(Clone, Copy)]
struct AnnualBalance {
    entitlement: Money,
    taken: Money,
}

impl AnnualBalance {
    fn remaining(self) -> Money {
        self.entitlement - self.taken
    }
}

fn annual_entitlement(
    rs: &Ruleset,
    hired_on: NaiveDate,
    earned_through: NaiveDate,
) -> Result<Money, laysi_regulation::RegulationError> {
    annual_entitlement_for_service(rs, service_months(hired_on, earned_through))
}

fn annual_entitlement_for_service(
    rs: &Ruleset,
    months: u32,
) -> Result<Money, laysi_regulation::RegulationError> {
    Ok(Money::from(laysi_regulation::annual_leave_days(rs, months)?))
}

fn leave_json(request: &LeaveRequest, balance: Option<AnnualBalance>) -> Value {
    json!({
        "leave_no": request.leave_no,
        "emp_no": request.emp_no,
        "name": request.name,
        "dept_label": request.dept_label,
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
        "annual_leave": balance.map(|b| json!({
            "entitlement": b.entitlement.to_string(),
            "taken": b.taken.to_string(),
            "remaining": b.remaining().to_string(),
        })),
    })
}

// Attendance

struct GetAttendance;

#[derive(Deserialize, JsonSchema, Default)]
pub struct GetAttendanceInput {
    /// Month (YYYY-MM). Defaults to this month.
    #[serde(default)]
    pub month: Option<String>,
}

#[async_trait]
impl Operation for GetAttendance {
    fn name(&self) -> &'static str {
        "get_attendance"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀某個月的逐人出勤、請假、遲到與加班時數。加班費金額由差勤系統帶入；規則庫尚未收錄加班費倍數，本系統不自行重算。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<GetAttendanceInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: GetAttendanceInput = parse(input)?;
        let month = i
            .month
            .unwrap_or_else(|| ctx.today.format("%Y-%m").to_string());
        if !valid_month(&month) {
            return Err(OpError::BadInput(format!(
                "月份要寫成 YYYY-MM，收到的是「{month}」"
            )));
        }

        let rows = ctx.store.attendance(&month).await?;
        let months = ctx.store.attendance_months().await?;
        let required_days: Money = rows.iter().map(|r| r.required_days).sum();
        let worked_days: Money = rows.iter().map(|r| r.worked_days).sum();
        let leave_days: Money = rows.iter().map(|r| r.leave_days).sum();
        let overtime_hours: Money = rows.iter().map(|r| r.overtime_hours).sum();
        let overtime_pay: Money = rows.iter().map(|r| r.overtime_pay).sum();
        let late_count: i64 = rows.iter().map(|r| i64::from(r.late_count)).sum();

        let attendance_rows = rows
            .iter()
            .map(|r| {
                json!({
                    "emp_no": r.emp_no,
                    "name": r.name,
                    "dept_label": r.dept_label,
                    "title": r.title,
                    "month": r.month,
                    "required_days": r.required_days.to_string(),
                    "worked_days": r.worked_days.to_string(),
                    "leave_days": r.leave_days.to_string(),
                    "overtime_hours": r.overtime_hours.to_string(),
                    "overtime_pay": r.overtime_pay.to_string(),
                    "late_count": r.late_count,
                    "note": r.note,
                })
            })
            .collect::<Vec<_>>();

        let summary = format!(
            "讀 {month} 出勤 · {} 人 · 出勤 {} 日、請假 {} 日、加班 {} 小時 · 加班費 {} 由來源系統帶入",
            attendance_rows.len(),
            worked_days.normalize(),
            leave_days.normalize(),
            overtime_hours.normalize(),
            money(overtime_pay)
        );

        Ok(OpOutcome::new(
            json!({
                "month": month,
                "months": months,
                "rows": attendance_rows,
                "headcount": rows.len(),
                "totals": {
                    "required_days": required_days.to_string(),
                    "worked_days": worked_days.to_string(),
                    "leave_days": leave_days.to_string(),
                    "overtime_hours": overtime_hours.to_string(),
                    "overtime_pay": overtime_pay.to_string(),
                    "late_count": late_count,
                },
                "overtime_recomputed": false,
                "overtime_note": "加班費倍數尚未收錄至規則庫，金額由差勤系統帶入，本系統不依時數自行計算。如需重算，要先補上可依日期解析的加班費規則。",
            }),
            summary,
        ))
    }
}

// Leave

struct ListLeaveRequests;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListLeaveRequestsInput {
    /// Include approved and rejected requests; by default only pending ones.
    #[serde(default)]
    pub include_done: bool,
    /// Maximum rows. Omit for no additional limit.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[async_trait]
impl Operation for ListLeaveRequests {
    fn name(&self) -> &'static str {
        "list_leave_requests"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀請假單與逐人特休餘額。額度依查詢當日的 annual_leave ruleset 與年資現算，資料庫只留已核准的實休天數。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListLeaveRequestsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListLeaveRequestsInput = parse(input)?;
        if i.limit.is_some_and(|limit| limit <= i64::default()) {
            return Err(OpError::BadInput("limit 要大於零".into()));
        }

        let requests = ctx
            .store
            .leave_requests(i.include_done, i.limit.unwrap_or(i64::MAX))
            .await?;
        let staff = ctx.store.employees(true, None).await?;
        let rs = laysi_regulation::resolve("annual_leave", ctx.today)?;
        let taken = ctx.store.annual_leave_taken(ctx.today.year()).await?;
        let taken_by_employee: BTreeMap<i32, Money> = taken.into_iter().collect();
        let mut balances = BTreeMap::new();

        for employee in &staff {
            let earned_through = employee
                .left_on
                .map_or(ctx.today, |left| left.min(ctx.today));
            let entitlement = annual_entitlement(rs, employee.hired_on, earned_through)?;
            let used = taken_by_employee
                .get(&employee.id)
                .copied()
                .unwrap_or(Money::ZERO);
            balances.insert(
                employee.id,
                AnnualBalance {
                    entitlement,
                    taken: used,
                },
            );
        }

        let rows = requests
            .iter()
            .map(|request| leave_json(request, balances.get(&request.employee_id).copied()))
            .collect::<Vec<_>>();
        let annual_balances = staff
            .iter()
            .filter(|employee| employee.status != "left")
            .filter_map(|employee| {
                let balance = balances.get(&employee.id)?;
                Some(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "dept_label": employee.dept_label,
                    "entitlement": balance.entitlement.to_string(),
                    "taken": balance.taken.to_string(),
                    "remaining": balance.remaining().to_string(),
                }))
            })
            .collect::<Vec<_>>();
        let pending = requests
            .iter()
            .filter(|request| request.status == "pending")
            .count();

        let summary = format!(
            "讀請假單 · {} 張（{pending} 張等簽）· 特休額度依 annual_leave {} 現算",
            rows.len(),
            rs.version
        );
        Ok(OpOutcome::new(
            json!({
                "requests": rows,
                "pending_count": pending,
                "annual_leave_balances": annual_balances,
                "annual_leave_year": ctx.today.year(),
                "annual_leave_ruleset": ruleset_json(rs),
                "annual_leave_quota_stored": false,
                "annual_leave_note": "資料庫只存已核准、實際休掉的天數；額度每次由年資與 annual_leave 規則現算。離職員工的年資停在離職日，不會離職後繼續累加。",
            }),
            summary,
        ))
    }
}

struct DecideLeaveRequest;

#[derive(Deserialize, JsonSchema)]
pub struct DecideLeaveRequestInput {
    /// Leave request number.
    pub leave_no: String,
    /// `true` approves, `false` rejects.
    pub approve: bool,
    /// Note for the requester.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for DecideLeaveRequest {
    fn name(&self) -> &'static str {
        "decide_leave_request"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "准假或駁回一張仍在等簽核的請假單，留下決定者、身分與說明。不是 pending 時由 store 拒絕，不在操作層先做一份可能過期的判斷。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<DecideLeaveRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DecideLeaveRequestInput = parse(input)?;
        let request = ctx
            .store
            .decide_leave_request(
                &i.leave_no,
                i.approve,
                &ctx.actor,
                &ctx.actor_role,
                i.note.as_deref().unwrap_or(""),
            )
            .await?;

        let action = if i.approve { "准假" } else { "駁回" };
        let summary = format!(
            "{} · {} {} {} {} 日 · 由 {}（{}）{action}",
            request.leave_no,
            request.emp_no,
            request.name,
            leave_kind_label(&request.kind),
            request.days.normalize(),
            ctx.actor,
            ctx.actor_role
        );
        Ok(OpOutcome::new(
            json!({ "request": leave_json(&request, None), "action": request.status }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every value in the `leave_request.kind` CHECK needs its own label. If a
    /// migration adds a type and this does not follow, the screen quietly
    /// shows "other", so the two sets are compared verbatim.
    #[test]
    fn every_leave_kind_in_the_database_check_has_a_label() {
        let migration =
            include_str!("../../../../migrations/0008_hr_employees_attendance_payroll.sql");
        let table = migration
            .split_once("CREATE TABLE leave_request")
            .expect("0008 migration 缺少 leave_request")
            .1
            .split_once("CREATE INDEX leave_request_emp_idx")
            .expect("找不到 leave_request 表的結尾")
            .0;
        let check = table
            .split_once("CHECK (kind IN")
            .expect("leave_request.kind 缺少 CHECK")
            .1
            .split_once(")),")
            .expect("leave_request.kind CHECK 沒有結尾")
            .0;
        let migration_kinds: BTreeSet<&str> =
            check.split('\'').skip(1).step_by(2).collect();
        let labelled_kinds: BTreeSet<&str> = LEAVE_KINDS.iter().copied().collect();

        assert_eq!(
            migration_kinds, labelled_kinds,
            "資料庫合法假別與操作層標籤集合不一致"
        );
        for kind in migration_kinds {
            assert_ne!(
                leave_kind_label(kind),
                "其他",
                "{kind} 是資料庫合法值，不能顯示成其他"
            );
        }
    }

    /// Leave entitlement comes only from the ruleset payload. Replace the
    /// payload for a band and the same service length must give a different
    /// answer; the operation layer may not hide a default entitlement.
    #[test]
    fn annual_leave_entitlement_changes_with_the_ruleset_payload() {
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

        let before = annual_entitlement_for_service(rs, months).unwrap();
        let mut changed = rs.clone();
        changed.payload["tiers"][0]["days"] = Value::String(replacement.to_string());
        let after = annual_entitlement_for_service(&changed, months).unwrap();

        assert_ne!(before, after);
    }
}
