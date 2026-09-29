//! Payroll, the link where HR actually meets finance.
//!
//! This module contains no rates. Labour insurance, health insurance and
//! pension go through [`crate::ops::insurance::premiums_on`] into
//! `laysi-regulation`; the withholding rate for salary income is read from
//! entry 50 of `payload.codes` in the `withholding` ruleset. The only number
//! here is how many days a month has, which is the calendar, not regulation.
//!
//! The two rulesets resolve on different dates:
//!
//! - Premiums resolve on the period end, the same rule as depreciation.
//! - Withholding resolves on the payment date, because the obligation arises
//!   when payment is made (Income Tax Act art. 88), so a payment that crosses
//!   a year boundary uses the rate of the year it was paid in.
//!
//! Overtime pay comes from approved overtime requests: the hours are what
//! somebody asked for and a manager agreed to, and the multipliers of Labor
//! Standards Act art. 24 are in the `overtime` ruleset, resolved on the day
//! worked. No multiplier appears in this module. Hours with no approved
//! request are not paid: an approval is the only record that the overtime
//! was agreed to.
//!
//! Approved leave moves the base by the `leave_pay` ruleset, resolved on each
//! day of leave: a monthly salary loses the share a kind of leave does not
//! pay, an hourly wage gains the share it does. Nothing else is deducted.
//! Days with no punch and no leave, missing departures, pending requests and
//! a part timer's public holidays are listed for somebody to look at, and
//! change no amount.
//!
//! The payroll voucher has three parts and is created unposted:
//!
//! ```text
//! Dr 6111 Salary expense    gross pay (one line per department)
//! Cr 2117 Withheld items    employee insurance shares + withheld income tax
//! Cr 1102 Bank              net pay
//! ```
//!
//! The employer's insurance and pension contributions are not in this voucher.
//! They are a company expense rather than an employee deduction, and posting
//! them needs an insurance expense account and a second voucher. That link is
//! not built yet, so the screen reports it as not wired rather than folding it
//! into 6111.
//!
//! [`GetPayrollSummary`] is `ReadOnly` and [`ListPayslips`] is
//! `Risk::BossOnly` (the 0006 permission matrix: the accountant sees totals,
//! not individuals). Both read the same table; the difference is whether
//! `dispatch` stops the caller. The filter is not written into the query.

use crate::ops::attendance::{
    default_day_kind, judges_absence, leave_kind_label, mark_days, month_days,
    ordinary_hourly_wage, punch_days, DayKind, PunchDay, WorkSchedule,
};
#[cfg(test)]
use crate::ops::employees::INSURED_EMPLOYEE;
use crate::ops::employees::{pay_basis_label, service_months, BASIS_HOURLY, BASIS_MONTHLY};
use crate::ops::insurance::{percent, premiums_on, ruleset_json};
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use laysi_domain::business_time;
use laysi_domain::Money;
use laysi_regulation::{LeaveDay, Ruleset};
use laysi_store::{Employee, LeaveRequest, NewPayslip};
use rust_decimal::{Decimal, RoundingStrategy};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetPayrollSummary),
        Box::new(ListPayslips),
        Box::new(RunPayroll),
        Box::new(VoidPayrollRun),
    ]
}

/// Withholding income code for salary. A code, not a rate; the rate is
/// looked up in the payload.
pub(crate) const SALARY_INCOME_CODE: &str = "50";

/// Why somebody was left out of a run. Matched again when the run pays
/// nobody, to tell "everyone is already paid" from "somebody is waiting on
/// data", which are two different things to do next.
const ALREADY_PAID: &str = "本期已計薪";

/// Round to whole dollars. A rounding mode, not a regulated rate.
fn to_dollars(v: Money) -> Money {
    v.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

/// Split `YYYY-MM` into the first and last day of that month. Calendar
/// arithmetic, not regulation.
fn month_bounds(month: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (y, m) = month.split_once('-')?;
    let (y, m): (i32, u32) = (y.parse().ok()?, m.parse().ok()?);
    let from = NaiveDate::from_ymd_opt(y, m, 1)?;
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Some((from, NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()?))
}

fn resolve_period(
    period: Option<String>,
    today: NaiveDate,
) -> Result<(String, NaiveDate, NaiveDate), OpError> {
    let period = period.unwrap_or_else(|| today.format("%Y-%m").to_string());
    let (from, to) = month_bounds(&period)
        .ok_or_else(|| OpError::BadInput(format!("期別要寫成 YYYY-MM，收到的是「{period}」")))?;
    Ok((period, from, to))
}

/// Withholding rate for salary income.
///
/// `Ruleset::decimal` walks object paths, and `codes` is an array, so the
/// lookup is done here. The value still comes only from the payload; this
/// function does not know what any rate looks like.
fn salary_withholding_rate(rs: &Ruleset) -> Result<(Money, String, String), OpError> {
    let found = rs
        .payload
        .get("codes")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|c| c.get("code").and_then(|v| v.as_str()) == Some(SALARY_INCOME_CODE))
        })
        .ok_or_else(|| {
            OpError::BadInput(format!(
                "扣繳率標準 {} 未收錄所得格式代號 {SALARY_INCOME_CODE}（薪資所得），無法計算代扣稅額",
                rs.version
            ))
        })?;

    let rate = found
        .get("rate")
        .and_then(|v| v.as_str())
        .and_then(|s| Decimal::from_str_exact(s).ok())
        .ok_or_else(|| {
            OpError::BadInput(format!(
                "扣繳率標準 {} 的代號 50 未設定可用的 rate",
                rs.version
            ))
        })?;
    Ok((
        rate,
        found
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("薪資所得")
            .to_string(),
        found
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    ))
}

/// One person's punch days in the period.
fn punch_days_of(
    punches: &[laysi_store::AttendancePunch],
    employee_id: i32,
    offset: FixedOffset,
) -> Vec<PunchDay> {
    let mine: Vec<_> = punches
        .iter()
        .filter(|p| p.employee_id == employee_id)
        .cloned()
        .collect();
    punch_days(&mine, offset)
}

/// Hours an hourly worker is paid at the plain rate: punched time, breaks
/// taken out, less the part of it that approved overtime covers. The art. 24
/// multipliers already include the ordinary wage for those hours, so leaving
/// them in would pay them twice. Only the overlap comes out: overtime worked
/// after punching out, or on a day with a missed punch, was never in the
/// punched time.
///
/// Only complete days count: a day clocked in and never out has no length,
/// and guessing one would put money on it. That day is a missed punch, and
/// the correction flow exists to fix it before payroll runs.
fn ordinary_hours(
    days: &[PunchDay],
    schedule: &WorkSchedule,
    overtime: &[(NaiveDateTime, NaiveDateTime)],
) -> Money {
    let seconds: i64 = days
        .iter()
        .filter_map(|d| {
            let worked = d.worked_seconds(schedule)?;
            let (arrived, left) = (d.arrived?, d.left?);
            let covered: i64 = overtime
                .iter()
                .map(|&(from, to)| {
                    let (start, end) = (from.max(arrived), to.min(left));
                    match end > start {
                        true => (end - start).num_seconds() - schedule.break_seconds(start, end),
                        false => 0,
                    }
                })
                .sum();
            Some((worked - covered).max(0))
        })
        .sum();
    Decimal::from(seconds) / Decimal::from(3600)
}

/// Days this employee actually worked in the period. Someone who joined or
/// left mid-period is paid pro rata by calendar day.
///
/// Calendar-day proration is company policy, not regulation — another
/// company might prorate by working day. It may live in code because it has
/// no effective date; the labour insurance rate does, so that one may not
/// (see the settings/regulation boundary in 0006).
fn paid_days(e: &Employee, from: NaiveDate, to: NaiveDate) -> i64 {
    let start = e.hired_on.max(from);
    let end = e.left_on.map(|d| d.min(to)).unwrap_or(to);
    if end < start {
        0
    } else {
        (end - start).num_days() + 1
    }
}

/// Whether this person is paid in this period.
fn in_service(e: &Employee, from: NaiveDate, to: NaiveDate) -> bool {
    e.hired_on <= to && e.left_on.is_none_or(|d| d >= from)
}

/// The period being paid, as the attendance checks need it.
struct Period<'a> {
    name: &'a str,
    from: NaiveDate,
    to: NaiveDate,
    /// The real date: a day is only missed once it is over.
    today: NaiveDate,
    schedule: &'a WorkSchedule,
}

/// One kind of approved leave inside the period, and what it pays.
#[derive(Default)]
struct LeaveTotal {
    days: Money,
    paid_days: Money,
    hours: Money,
    paid_hours: Money,
}

/// Approved leave inside the period, by kind, priced by `leave_pay`.
///
/// `requests` must reach back to 1 January: sickness leave's yearly allowance
/// is counted from then. Each request is split over its dates in proportion
/// to the working time on each, but its stored hours and days are what gets
/// split -- they were settled when it was sent, and a later change to the
/// working window must not restate an approved request.
fn leave_in_period(
    e: &Employee,
    requests: &[&LeaveRequest],
    schedule: &WorkSchedule,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<BTreeMap<String, LeaveTotal>, OpError> {
    // Days before 1 January are in the split -- a request's stored total
    // covers them -- but never priced: they belong to last year's payroll,
    // and last year may have no ruleset in this database at all.
    let year_start = NaiveDate::from_ymd_opt(from.year(), 1, 1).unwrap_or(from);
    let mut days: Vec<(LeaveDay, Money)> = Vec::new();
    for l in requests.iter().filter(|l| l.status == "approved") {
        let dates: Vec<NaiveDate> = l
            .from_on
            .iter_days()
            .take_while(|d| *d <= l.to_on)
            .collect();
        let seconds: Vec<i64> = dates
            .iter()
            .map(|date| {
                let open = date.and_time(NaiveTime::MIN);
                let close = open + chrono::Duration::days(1);
                schedule
                    .leave_seconds(l.from_at.max(open), l.to_at.min(close))
                    .unwrap_or(0)
            })
            .collect();
        let total: i64 = seconds.iter().sum();
        for (i, date) in dates.iter().enumerate() {
            // Without a working window to split by, the whole request sits
            // on its first date: the stored total is still right.
            let share = match total > 0 {
                true => Decimal::from(seconds[i]) / Decimal::from(total),
                false if i == 0 => Decimal::ONE,
                false => Decimal::ZERO,
            };
            if share.is_zero() || *date > to || *date < year_start {
                continue;
            }
            days.push((
                LeaveDay {
                    date: *date,
                    kind: l.kind.clone(),
                    days: l.days * share,
                    service_months: service_months(e.hired_on, *date),
                },
                l.hours * share,
            ));
        }
    }
    days.sort_by_key(|(d, _)| d.date);
    let (leave_days, hours): (Vec<LeaveDay>, Vec<Money>) = days.into_iter().unzip();
    let paid = laysi_regulation::leave_paid_days(&leave_days)?;

    let first = e.hired_on.max(from);
    let last = e.left_on.map_or(to, |left| left.min(to));
    let mut totals: BTreeMap<String, LeaveTotal> = BTreeMap::new();
    for ((day, hours), paid) in leave_days.iter().zip(hours).zip(paid) {
        if day.date < first || day.date > last {
            continue;
        }
        let t = totals.entry(day.kind.clone()).or_default();
        t.days += day.days;
        t.paid_days += paid;
        t.hours += hours;
        if !day.days.is_zero() {
            t.paid_hours += hours * paid / day.days;
        }
    }
    Ok(totals)
}

/// Days that need somebody to look at them before the period is paid. None of
/// them changes an amount: an absence may be a forgotten punch, a pending
/// request may still be refused, and a public holiday on a part timer's
/// agreed day needs the agreement the roster does not record.
fn attendance_check(
    e: &Employee,
    p: &Period,
    punched: Vec<PunchDay>,
    requests: &[&LeaveRequest],
    awaiting_fix: &[NaiveDate],
) -> Option<Value> {
    let (schedule, from, to, today) = (p.schedule, p.from, p.to, p.today);
    let last = e.left_on.map_or(today, |left| left.min(today));
    let mut days = month_days(punched, p.name, e.hired_on, last);
    let judge = judges_absence(schedule, &e.employment_type, &e.insured_as);
    mark_days(&mut days, requests, awaiting_fix, schedule, judge, today);
    let absent: Vec<NaiveDate> = days.iter().filter(|d| d.absent).map(|d| d.date).collect();
    let open: Vec<NaiveDate> = days.iter().filter(|d| d.open()).map(|d| d.date).collect();
    let pending: Vec<&str> = requests
        .iter()
        .filter(|l| l.status == "pending" && l.to_on >= from && l.from_on <= to)
        .map(|l| l.leave_no.as_str())
        .collect();
    let holidays: Vec<NaiveDate> = match e.pay_basis == BASIS_HOURLY {
        true => schedule
            .calendar
            .iter()
            .filter(|(date, (kind, _))| {
                **date >= e.hired_on.max(from)
                    && **date <= last.min(to)
                    && *kind == DayKind::Holiday
                    && default_day_kind(**date).works()
            })
            .map(|(date, _)| *date)
            .collect(),
        false => Vec::new(),
    };
    if absent.is_empty() && open.is_empty() && pending.is_empty() && holidays.is_empty() {
        return None;
    }
    Some(json!({
        "emp_no": e.emp_no,
        "name": e.name,
        "absent_dates": absent,
        "open_dates": open,
        "pending_leaves": pending,
        "holiday_dates": holidays,
    }))
}

/// A computed payslip together with the rule versions used to produce it.
struct Computed {
    slip: NewPayslip,
    labor_version: String,
    health_version: String,
    pension_version: String,
    withholding_version: String,
    /// Floor when monthly salary is below the statutory minimum wage.
    /// `None` means compliant.
    below_minimum: Option<Money>,
}

/// Compute one person's pay for the period. Premiums and withholding rates
/// all come from the ruleset.
fn compute_one(
    e: &Employee,
    from: NaiveDate,
    to: NaiveDate,
    pay_on: NaiveDate,
    overtime: Money,
    plain_hours: Money,
    leave: &BTreeMap<String, LeaveTotal>,
) -> Result<Computed, OpError> {
    let month_days = Decimal::from((to - from).num_days() + 1);
    let days = Decimal::from(paid_days(e, from, to));
    if days <= Decimal::ZERO {
        return Err(OpError::BadInput(format!(
            "{} {} 於本期無在職日，不應列入計薪名單",
            e.emp_no, e.name
        )));
    }

    // Zero is "not agreed yet", not "unpaid": 0028 dropped the requirement that
    // a roster entry carry an amount. Saying so here is the point of that
    // change; a zero payslip would look like a computation rather than a gap.
    let hourly = e.pay_basis == BASIS_HOURLY;
    if e.base_salary <= Decimal::ZERO {
        return Err(OpError::BadInput(format!(
            "{} {} 尚未設定{}，請先在「員工名冊」填上再計薪",
            e.emp_no,
            e.name,
            if hourly { "時薪" } else { "底薪" }
        )));
    }

    // Hourly pay is the rate times the punched hours outside approved
    // overtime (see `ordinary_hours`); a monthly salary is prorated by
    // calendar day, so a full month gives days == month_days and a ratio of
    // 1. Proration does not apply to hourly pay:
    // the hours already say how much of the period was worked.
    //
    // Leave then moves it by the share `leave_pay` does not pay: a monthly
    // salary loses that share of a day, a day being the salary over the
    // month's calendar days as in the proration; an hourly wage gains the
    // share it does pay of the hours on leave. One amount per kind, rounded
    // on its own, so the note adds up to the payslip.
    let leave_amounts: Vec<(&str, Money, Money)> = leave
        .iter()
        .map(|(kind, t)| match hourly {
            true => (
                kind.as_str(),
                t.hours,
                to_dollars(e.base_salary * t.paid_hours),
            ),
            false => (
                kind.as_str(),
                t.days,
                -to_dollars(e.base_salary * (t.days - t.paid_days) / month_days),
            ),
        })
        .filter(|(_, _, amount)| !amount.is_zero())
        .collect();
    let base = match hourly {
        true => to_dollars(e.base_salary * plain_hours),
        false => to_dollars(e.base_salary * days / month_days),
    } + leave_amounts.iter().map(|(_, _, a)| *a).sum::<Money>();
    let base = base.max(Money::ZERO);
    let gross = base + overtime;

    // Premiums resolve on the period end: the version in force for this period.
    let p = premiums_on(
        to,
        &e.insured_as,
        e.labor_insured_salary,
        e.health_insured_salary,
        e.pension_wage,
    )?;

    // Withholding resolves on the payment date: the obligation arises then.
    let wht = laysi_regulation::resolve("withholding", pay_on)?;
    let (rate, _, _) = salary_withholding_rate(wht)?;
    let income_tax = to_dollars(gross * rate);

    let deduction = p.labor.employee + p.health.employee + income_tax;
    if deduction > gross {
        return Err(OpError::BadInput(format!(
            "{} {} 的代扣 {} 大於應發 {}，實發將為負數，請先確認投保級距與本期在職日數",
            e.emp_no,
            e.name,
            money(deduction),
            money(gross)
        )));
    }

    // Minimum wage check. This compares the agreed rate, not the period's net
    // pay: someone who joined mid-month earns less than the monthly minimum by
    // definition, which is not a breach. The floor follows the pay basis; the
    // two are published together and neither one bounds the other.
    let min_rs = laysi_regulation::resolve("minimum_wage", to)?;
    let below_minimum =
        laysi_regulation::minimum_wage_shortfall(min_rs, e.base_salary, &e.pay_basis)?;

    let mut note = match (hourly, days < month_days) {
        (true, _) => format!(
            "時薪制，本期正常工時 {} 小時",
            plain_hours.round_dp(2).normalize()
        ),
        (false, true) => format!("在職 {days}／{month_days} 日，按曆日比例計薪"),
        (false, false) => String::new(),
    };
    let leave_note: Vec<String> = leave_amounts
        .iter()
        .map(|(kind, qty, amount)| match hourly {
            true => format!(
                "{} {} 小時計 {}",
                leave_kind_label(kind),
                qty.round_dp(2).normalize(),
                money(*amount)
            ),
            false => format!(
                "{} {} 日扣 {}",
                leave_kind_label(kind),
                qty.round_dp(2).normalize(),
                money(amount.abs())
            ),
        })
        .collect();
    if !leave_note.is_empty() {
        if !note.is_empty() {
            note.push('；');
        }
        note.push_str(&leave_note.join("、"));
    }

    Ok(Computed {
        slip: NewPayslip {
            employee_id: e.id,
            dept_id: e.dept_id,
            dept_label: e.dept_label.clone(),
            base_salary: base,
            overtime,
            allowance: Money::ZERO,
            gross,
            labor_insured_salary: e.labor_insured_salary,
            health_insured_salary: e.health_insured_salary,
            pension_wage: e.pension_wage,
            labor_employee: p.labor.employee,
            health_employee: p.health.employee,
            income_tax,
            labor_employer: p.labor.employer,
            health_employer: p.health.employer,
            pension_employer: p.pension,
            deduction,
            net: gross - deduction,
            paid_days: days,
            month_days,
            note,
        },
        labor_version: p.labor_rs.version.clone(),
        health_version: p.health_rs.version.clone(),
        pension_version: p.pension_rs.version.clone(),
        withholding_version: wht.version.clone(),
        below_minimum,
    })
}

/// JSON shape of a payroll period, without per-person amounts.
fn run_json(r: &laysi_store::PayrollRun) -> Value {
    json!({
        "period": r.period,
        "pay_on": r.pay_on,
        "headcount": r.headcount,
        "gross_total": r.gross_total.to_string(),
        "deduction_total": r.deduction_total.to_string(),
        "net_total": r.net_total.to_string(),
        "employer_total": r.employer_total.to_string(),
        "voucher_no": r.voucher_no,
        "voucher_status": r.voucher_status,
        // Which rule versions this period was computed under. The only place
        // that can answer that after a retroactive amendment.
        "rulesets": {
            "labor_insurance": r.labor_version,
            "health_insurance": r.health_version,
            "pension": r.pension_version,
            "withholding": r.withholding_version,
        },
        "note": r.note,
        "created_at": r.created_at,
        "created_by": r.created_by,
    })
}

/// The periods of these runs, newest first, each named once. A period holds
/// more than one run once somebody is paid late, and the screen offers periods.
fn distinct_periods(runs: &[laysi_store::PayrollRun]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for r in runs {
        if !seen.contains(&r.period) {
            seen.push(r.period.clone());
        }
    }
    seen
}

/// One period as the screen totals it. A person paid late is paid by a second
/// run of the same period, so a period is a sum of runs and not one of them.
fn period_json(runs: &[&laysi_store::PayrollRun]) -> Option<Value> {
    let first = runs.first()?;
    Some(json!({
        "period": first.period,
        "pay_on": first.pay_on,
        "headcount": runs.iter().map(|r| r.headcount).sum::<i32>(),
        "gross_total": runs.iter().map(|r| r.gross_total).sum::<Money>().to_string(),
        "deduction_total": runs.iter().map(|r| r.deduction_total).sum::<Money>().to_string(),
        "net_total": runs.iter().map(|r| r.net_total).sum::<Money>().to_string(),
        "employer_total": runs.iter().map(|r| r.employer_total).sum::<Money>().to_string(),
        "run_count": runs.len(),
    }))
}

// Payroll summary

struct GetPayrollSummary;

#[derive(Deserialize, JsonSchema, Default)]
pub struct PayrollPeriodInput {
    /// Period (YYYY-MM). Defaults to the most recently computed one.
    #[serde(default)]
    pub period: Option<String>,
}

#[async_trait]
impl Operation for GetPayrollSummary {
    fn name(&self) -> &'static str {
        "get_payroll_summary"
    }
    fn risk(&self) -> Risk {
        // The summary is not personal data, and accounting needs it to
        // reconcile against 6111 in the ledger. Per-person detail is in
        // list_payslips, which is owner only.
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀某一期的薪資彙總：應發、代扣、實發、公司負擔，並與總帳 6111 薪資支出對帳。不含任何個人金額。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<PayrollPeriodInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PayrollPeriodInput = parse(input)?;
        let runs = ctx.store.payroll_runs(24).await?;
        let selected = match &i.period {
            Some(p) => Some(p.clone()),
            None => runs.first().map(|r| r.period.clone()),
        };
        let picked: Vec<&laysi_store::PayrollRun> = runs
            .iter()
            .filter(|r| Some(&r.period) == selected.as_ref())
            .collect();

        // Two independently computed paths: HR sums gross pay per period, the
        // ledger computes the balance of 6111. Equality is a conclusion, not an
        // assumption, as with AR ageing.
        let (posted, in_transit) = ctx.store.payroll_totals().await?;
        let gl = ctx.store.balance_of(&["6111"], true).await?;
        let gap = posted - gl;

        let detail_visible = ctx.actor_role == crate::ROLE_BOSS;
        let period_label = selected.clone().unwrap_or_else(|| "—".into());

        // Same question the HR dashboard asks, asked the same way: is there a
        // run for this month? Scanning the recent-runs list would answer it
        // from a window instead of from the table.
        let this_month = ctx.today.format("%Y-%m").to_string();
        let next_period = ctx
            .store
            .payroll_run(&this_month)
            .await?
            .is_none()
            .then_some(this_month);

        let summary = match picked.is_empty() {
            false => format!(
                "讀薪資彙總 {} · {} 人應發 {} 代扣 {} 實發 {} · 與總帳 6111 {}",
                period_label,
                picked.iter().map(|r| r.headcount).sum::<i32>(),
                money(picked.iter().map(|r| r.gross_total).sum()),
                money(picked.iter().map(|r| r.deduction_total).sum()),
                money(picked.iter().map(|r| r.net_total).sum()),
                if gap.is_zero() { "相符" } else { "不符" }
            ),
            true => "讀薪資彙總 · 尚無任何期別的薪資計算紀錄".to_string(),
        };

        Ok(OpOutcome::new(
            json!({
                "period": period_label,
                "run": period_json(&picked),
                // The runs that add up to the selected period, newest first.
                // The screen states each one: they carry a voucher and a set
                // of rule versions of their own.
                "period_runs": picked.iter().map(|r| run_json(r)).collect::<Vec<_>>(),
                "runs": runs.iter().map(run_json).collect::<Vec<_>>(),
                "periods": distinct_periods(&runs),
                // Reconciliation: HR gross total against the posted balance of 6111.
                "hr_posted_total": posted.to_string(),
                "gl_balance": gl.to_string(),
                "difference": gap.to_string(),
                "reconciled": gap.is_zero(),
                "not_yet_posted": in_transit.to_string(),
                // Whether per-person detail is visible. The screen states that the
                // caller's role cannot see it rather than showing a blank.
                "detail_visible": detail_visible,
                "detail_op": "list_payslips",
                "detail_note": if detail_visible {
                    "目前身分為負責人，可檢視逐人明細。"
                } else {
                    "逐人薪資明細限負責人檢視。目前身分可檢視彙總，無法檢視個別金額。"
                },
                "actor_role": ctx.actor_role,
                // What the run form should start on: the current month when it
                // has no run yet. Nothing to suggest once it is paid — the
                // next period is not due until the month turns.
                "next_period": next_period,
                "suggested_pay_on": ctx.today,
                "employer_in_ledger": false,
                "employer_note": "公司負擔的勞健保與勞退不在薪資傳票內。",
            }),
            summary,
        ))
    }
}

// Per-person payslips (owner only)

struct ListPayslips;

#[async_trait]
impl Operation for ListPayslips {
    fn name(&self) -> &'static str {
        "list_payslips"
    }
    fn risk(&self) -> Risk {
        // Who earns what is the most sensitive data in the system, and one of
        // the few rules here that genuinely stops a person.
        Risk::BossOnly
    }
    /// Reads only. Carries the identity floor above, which is a question
    /// about who may call it rather than about what it changes, so the
    /// licence floor has to be told separately.
    fn writes(&self) -> bool {
        false
    }
    fn summary(&self) -> &'static str {
        "逐人薪資明細：本薪、加班、代扣三項與實發，以及公司負擔的勞健保與勞退。限負責人。以同一份規則重算一次，再與存下來的數字比對。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<PayrollPeriodInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PayrollPeriodInput = parse(input)?;
        let runs = ctx.store.payroll_runs(24).await?;
        let run = match &i.period {
            Some(p) => runs.iter().find(|r| &r.period == p),
            None => runs.first(),
        }
        .ok_or_else(|| {
            OpError::BadInput(match &i.period {
                Some(p) => format!("{p} 尚未計算薪資"),
                None => "尚無任何期別的薪資計算紀錄".into(),
            })
        })?;

        let (_, _, period_end) = resolve_period(Some(run.period.clone()), ctx.today)?;
        let slips = ctx.store.payslips(&run.period).await?;
        // Every run of the period, because payslips are listed by period and a
        // late joiner is paid by a second run.
        let period_runs: Vec<&laysi_store::PayrollRun> =
            runs.iter().filter(|r| r.period == run.period).collect();

        let mut rows = Vec::with_capacity(slips.len());
        let mut all_match = true;
        for s in &slips {
            // Recompute from the same rules. What was stored is the result at the
            // time; recomputation is a second path, and a mismatch is stated on
            // screen as with AR ageing and depreciation.
            let p = premiums_on(
                period_end,
                &s.insured_as,
                s.labor_insured_salary,
                s.health_insured_salary,
                s.pension_wage,
            )?;
            let same = p.labor.employee == s.labor_employee
                && p.labor.employer == s.labor_employer
                && p.health.employee == s.health_employee
                && p.health.employer == s.health_employer
                && p.pension == s.pension_employer;
            all_match &= same;

            rows.push(json!({
                "emp_no": s.emp_no,
                "name": s.name,
                "title": s.title,
                "dept_label": s.dept_label,
                "base_salary": s.base_salary.to_string(),
                "overtime": s.overtime.to_string(),
                "allowance": s.allowance.to_string(),
                "gross": s.gross.to_string(),
                "labor_insured_salary": s.labor_insured_salary.to_string(),
                "health_insured_salary": s.health_insured_salary.to_string(),
                "pension_wage": s.pension_wage.to_string(),
                "labor_employee": s.labor_employee.to_string(),
                "health_employee": s.health_employee.to_string(),
                "income_tax": s.income_tax.to_string(),
                "deduction": s.deduction.to_string(),
                "net": s.net.to_string(),
                "labor_employer": s.labor_employer.to_string(),
                "health_employer": s.health_employer.to_string(),
                "pension_employer": s.pension_employer.to_string(),
                "employer_total": (s.labor_employer + s.health_employer + s.pension_employer)
                    .to_string(),
                "paid_days": s.paid_days.to_string(),
                "month_days": s.month_days.to_string(),
                "prorated": s.paid_days < s.month_days,
                "note": s.note,
                // Recompute under today's rules and compare with what was stored.
                "recompute_matches": same,
            }));
        }

        let summary = format!(
            "讀 {} 逐人薪資明細 · {} 人 · 應發 {} 實發 {} · 依存下來的規則版本重算{}",
            run.period,
            rows.len(),
            money(period_runs.iter().map(|r| r.gross_total).sum()),
            money(period_runs.iter().map(|r| r.net_total).sum()),
            if all_match { "一致" } else { "有差異" }
        );

        Ok(OpOutcome::new(
            json!({
                "period": run.period,
                "run": period_json(&period_runs),
                "rows": rows,
                "recompute_all_match": all_match,
                "recompute_note": "各列都以當期薪資作業保存的規則版本重算一次，並與儲存值比對。",
                "periods": distinct_periods(&runs),
            }),
            summary,
        ))
    }
}

// Compute payroll and produce the voucher

struct RunPayroll;

#[derive(Deserialize, JsonSchema, Default)]
pub struct RunPayrollInput {
    /// Period (YYYY-MM). Defaults to this month.
    #[serde(default)]
    pub period: Option<String>,
    /// Payment date (YYYY-MM-DD). Defaults to today; withholding resolves on it.
    #[serde(default)]
    pub pay_on: Option<NaiveDate>,
    /// Which bank account pays. Defaults to the first 1102 bank account.
    #[serde(default)]
    pub bank_account_id: Option<i32>,
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for RunPayroll {
    fn name(&self) -> &'static str {
        "run_payroll"
    }
    fn risk(&self) -> Risk {
        // Decides what everyone is paid this month and produces a voucher.
        // Same tier as post_voucher.
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "計算某一期的薪資並產生待過帳傳票（借 6111 薪資支出逐部門／貸 2117 代扣款／貸 1102 銀行存款）。保費依期末日、扣繳依給付日解析規則，每一期都記下用的四個規則版本。已計薪的人不會再算一次，同一期可以為後來到職或漏算的人補算，各自產生一張傳票。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RunPayrollInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RunPayrollInput = parse(input)?;
        let (period, from, to) = resolve_period(i.period, ctx.today)?;
        let pay_on = i.pay_on.unwrap_or(ctx.today);

        // A period can be paid more than once (0054). What may not happen is
        // paying one person twice, so the people already holding a payslip for
        // this period are the ones this run leaves alone.
        let already_paid = ctx.store.payroll_paid_employee_ids(&period).await?;
        let supplementary = !already_paid.is_empty();

        let staff = ctx.store.employees(true, None).await?;

        // Hours actually worked in the period, for whoever is paid by the
        // hour. Read once and split per person: `punch_days` walks a run of
        // days and would fold two people's same date into one row.
        let punches = ctx.store.punches(&period, None).await?;
        let schedule = WorkSchedule::load(ctx).await?;

        // Leave from 1 January: sickness leave's yearly allowance is counted
        // from then. Pending requests come along for the checks only.
        let year_start = NaiveDate::from_ymd_opt(from.year(), 1, 1).unwrap_or(from);
        let leaves = ctx
            .store
            .leave_requests_matching(true, None, Some((year_start, to)))
            .await?;
        let awaiting = ctx.store.punch_corrections(false, None).await?;
        let checking = Period {
            name: &period,
            from,
            to,
            today: business_time(Utc::now(), ctx.offset).date(),
            schedule: &schedule,
        };
        let mut attendance_checks: Vec<Value> = Vec::new();

        // Approved overtime for the period, priced by the ruleset in force on
        // each day worked.
        let approved_overtime = ctx.store.approved_overtime(from, to).await?;
        let mut overtime_by_employee: BTreeMap<i32, (Money, Money)> = BTreeMap::new();
        let mut overtime_spans: BTreeMap<i32, Vec<(NaiveDateTime, NaiveDateTime)>> =
            BTreeMap::new();
        for o in &approved_overtime {
            overtime_spans
                .entry(o.employee_id)
                .or_default()
                .push((o.from_at, o.to_at));
            let rs = laysi_regulation::resolve("overtime", o.worked_on)?;
            let worker = staff.iter().find(|e| e.id == o.employee_id);
            let salary = worker.map(|e| e.base_salary).unwrap_or(Money::ZERO);
            let pay = match salary > Money::ZERO {
                true => {
                    let basis = worker
                        .map(|e| e.pay_basis.as_str())
                        .unwrap_or(BASIS_MONTHLY);
                    let wage = ordinary_hourly_wage(rs, basis, salary)?;
                    laysi_regulation::overtime_pay(rs, wage, &o.day_type, o.hours)?
                }
                false => Money::ZERO,
            };
            let entry = overtime_by_employee
                .entry(o.employee_id)
                .or_insert((Money::ZERO, Money::ZERO));
            entry.0 += o.hours;
            entry.1 += pay;
        }
        let overtime_version = approved_overtime
            .first()
            .map(|o| laysi_regulation::resolve("overtime", o.worked_on))
            .transpose()?
            .map(|rs| rs.version.clone());

        let mut slips: Vec<NewPayslip> = Vec::new();
        let mut warnings: Vec<Value> = Vec::new();
        let mut skipped: Vec<Value> = Vec::new();
        let mut versions: Option<(String, String, String, String)> = None;

        for e in &staff {
            if already_paid.contains(&e.id) {
                skipped.push(json!({
                    "emp_no": e.emp_no,
                    "name": e.name,
                    "reason": ALREADY_PAID,
                }));
                continue;
            }
            if !in_service(e, from, to) {
                skipped.push(json!({
                    "emp_no": e.emp_no,
                    "name": e.name,
                    "reason": if e.hired_on > to { "本期尚未到職" } else { "本期已離職" },
                }));
                continue;
            }
            let overtime = overtime_by_employee
                .get(&e.id)
                .map(|(_, pay)| *pay)
                .unwrap_or(Money::ZERO);

            let days = punch_days_of(&punches, e.id, ctx.offset);
            let requests: Vec<&LeaveRequest> =
                leaves.iter().filter(|l| l.employee_id == e.id).collect();
            let fixes: Vec<NaiveDate> = awaiting
                .iter()
                .filter(|c| c.employee_id == e.id)
                .map(|c| c.punched_on)
                .collect();
            attendance_checks.extend(attendance_check(
                e,
                &checking,
                punch_days_of(&punches, e.id, ctx.offset),
                &requests,
                &fixes,
            ));
            let leave = leave_in_period(e, &requests, &schedule, from, to)?;

            // An hourly worker with nothing clocked and no paid leave has no
            // pay to compute. A payslip of zero would read as an amount
            // somebody worked out.
            let paid_leave = leave.values().any(|t| !t.paid_hours.is_zero());
            if e.pay_basis == BASIS_HOURLY
                && ordinary_hours(&days, &schedule, &[]).is_zero()
                && !paid_leave
            {
                skipped.push(json!({
                    "emp_no": e.emp_no,
                    "name": e.name,
                    "reason": "時薪制，本期無完整打卡紀錄",
                }));
                continue;
            }

            let spans = overtime_spans.get(&e.id).map(Vec::as_slice).unwrap_or(&[]);
            let plain = ordinary_hours(&days, &schedule, spans);
            let c = compute_one(e, from, to, pay_on, overtime, plain, &leave)?;
            if let Some(floor) = c.below_minimum {
                warnings.push(json!({
                    "emp_no": e.emp_no,
                    "name": e.name,
                    "issue": format!(
                        "{} {} 低於法定最低工資 {}",
                        pay_basis_label(&e.pay_basis),
                        money(e.base_salary),
                        money(floor)
                    ),
                }));
            }
            if versions.is_none() {
                versions = Some((
                    c.labor_version.clone(),
                    c.health_version.clone(),
                    c.pension_version.clone(),
                    c.withholding_version.clone(),
                ));
            }
            slips.push(c.slip);
        }

        let (labor_v, health_v, pension_v, wht_v) = versions.ok_or_else(|| {
            // Somebody left out for a reason other than having been paid is
            // the answer to why nobody is being paid. Reporting them as all
            // paid would hide the one person who is waiting on something.
            let waiting: Vec<String> = skipped
                .iter()
                .filter(|s| s["reason"] != ALREADY_PAID)
                .map(|s| {
                    format!(
                        "{} {}（{}）",
                        s["emp_no"].as_str().unwrap_or_default(),
                        s["name"].as_str().unwrap_or_default(),
                        s["reason"].as_str().unwrap_or_default(),
                    )
                })
                .collect();
            OpError::BadInput(match (waiting.is_empty(), supplementary) {
                (false, _) => format!("{period} 沒有人計得出薪資：{}", waiting.join("、")),
                (true, true) => format!("{period} 在職員工都已計薪，沒有需要補算的人。"),
                (true, false) => format!("{period} 無在職員工需計薪"),
            })
        })?;

        let bank_account_id = match i.bank_account_id {
            Some(id) => id,
            None => ctx.store.primary_bank_account("1102").await?,
        };

        let labor_ee: Money = slips.iter().map(|s| s.labor_employee).sum();
        let health_ee: Money = slips.iter().map(|s| s.health_employee).sum();
        let tax: Money = slips.iter().map(|s| s.income_tax).sum();
        let deduction_memo = format!(
            "勞保 {} 健保 {} 代扣所得稅 {}",
            money(labor_ee),
            money(health_ee),
            money(tax)
        );

        let note = i.note.clone().unwrap_or_else(|| {
            format!("依勞保 {labor_v}、健保 {health_v}、勞退 {pension_v}、扣繳 {wht_v} 規則")
        });

        let run = ctx
            .store
            .record_payroll_run(laysi_store::RecordPayroll {
                period: &period,
                pay_on,
                slips: &slips,
                bank_account_id,
                income_code: SALARY_INCOME_CODE,
                deduction_memo: &deduction_memo,
                labor_version: &labor_v,
                health_version: &health_v,
                pension_version: &pension_v,
                withholding_version: &wht_v,
                note: &note,
                actor: &ctx.actor,
            })
            .await?;

        let voucher_no = run.voucher_no.clone().unwrap_or_default();
        let voucher = ctx.store.voucher_by_no(&voucher_no).await?;

        // The audit summary names the versions: which rules a period was
        // computed under must be visible from the audit trail alone.
        let summary = format!(
            "{period} 薪資作業 · {} 人應發 {} 代扣 {} 實發 {} · 公司另負擔 {} · 依勞保 {labor_v}、健保 {health_v}、勞退 {pension_v}、扣繳 {wht_v} 規則 · {voucher_no} 待過帳",
            run.headcount,
            money(run.gross_total),
            money(run.deduction_total),
            money(run.net_total),
            money(run.employer_total),
        );

        let wht_rs = laysi_regulation::resolve("withholding", pay_on)?;
        let (rate, income_name, income_note) = salary_withholding_rate(wht_rs)?;

        Ok(OpOutcome::new(
            json!({
                "run": run_json(&run),
                "voucher": voucher,
                "skipped": skipped,
                // Below minimum wage does not block the run — people still have to
                // be paid — but it must be visible.
                "warnings": warnings,
                "attendance_checks": attendance_checks,
                "deduction_breakdown": {
                    "labor_employee": labor_ee.to_string(),
                    "health_employee": health_ee.to_string(),
                    "income_tax": tax.to_string(),
                },
                "withholding": {
                    "income_code": SALARY_INCOME_CODE,
                    "income_name": income_name,
                    "rate": rate.to_string(),
                    "rate_percent": percent(rate),
                    "note": income_note,
                    "ruleset": ruleset_json(wht_rs),
                },
                "overtime": {
                    "approved_count": approved_overtime.len(),
                    "hours": overtime_by_employee.values().map(|(h, _)| *h).sum::<Money>().normalize().to_string(),
                    "pay": overtime_by_employee.values().map(|(_, p)| *p).sum::<Money>().to_string(),
                    "ruleset_version": overtime_version,
                    "note": "加班費依核准的加班申請與加班日適用的加班規則計算。沒有核准紀錄的加班不計入。",
                },
                "employer_in_ledger": false,
                "employer_note": "公司負擔的勞健保與勞退未納入本張傳票。",
            }),
            summary,
        )
        .with_voucher(voucher_no))
    }
}

// Undo a payroll run so the period can be computed again

struct VoidPayrollRun;

#[derive(Deserialize, JsonSchema)]
pub struct VoidPayrollRunInput {
    /// Period (YYYY-MM) to undo.
    pub period: String,
    /// Reason for voiding. Required, and written into the audit summary.
    pub reason: String,
}

#[async_trait]
impl Operation for VoidPayrollRun {
    fn name(&self) -> &'static str {
        "void_payroll_run"
    }
    fn risk(&self) -> Risk {
        // Deletes payslips and the voucher that carried them. Same tier as
        // producing them.
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "作廢一整期的薪資作業，薪資單、扣繳紀錄與待過帳傳票一併刪除，主檔改好後可以重新計算。傳票已過帳的期間不受理，差額請在下一期補發或補扣。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<VoidPayrollRunInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: VoidPayrollRunInput = parse(input)?;
        let reason = crate::ops::ledger::require_void_reason(&i.reason)?;
        let (period, _, _) = resolve_period(Some(i.period), ctx.today)?;

        let voided = ctx.store.void_payroll_run(&period).await?;
        let vouchers = match voided.voucher_nos.is_empty() {
            true => "無傳票".to_string(),
            false => voided.voucher_nos.join("、"),
        };
        let summary = format!(
            "{} 薪資作業已作廢 · {} 人薪資單與傳票 {} 一併刪除 · 理由：{reason}",
            voided.period, voided.headcount, vouchers
        );
        Ok(OpOutcome::new(json!({ "voided": voided }), summary))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn n(s: &str) -> Money {
        Decimal::from_str(s).unwrap()
    }

    fn emp(hired: NaiveDate, left: Option<NaiveDate>) -> Employee {
        Employee {
            id: 1,
            emp_no: "TEST-001".into(),
            name: "測試員工".into(),
            dept_id: None,
            dept_code: None,
            dept_label: None,
            title: "測試職稱".into(),
            employment_type: "full_time".into(),
            pay_basis: BASIS_MONTHLY.into(),
            insured_as: INSURED_EMPLOYEE.into(),
            birthday: None,
            hired_on: hired,
            left_on: left,
            status: "active".into(),
            has_login: false,
            base_salary: n("53000"),
            labor_insured_salary: n("45800"),
            health_insured_salary: n("53000"),
            pension_wage: n("53000"),
            id_no: String::new(),
            bank_accounts: Vec::new(),
            dependents: Vec::new(),
            note: String::new(),
        }
    }

    /// A reference payslip's deductions must be reproducible from the rules.
    ///
    /// Labour insurance 1,008, health insurance 822, withheld tax
    /// 53,000 x 5% = 2,650, total 4,480, net 48,520.
    #[test]
    fn a_reference_payslip_is_reproducible_from_the_rulesets() {
        let p = premiums_on(
            d(2026, 6, 30),
            INSURED_EMPLOYEE,
            n("45800"),
            n("53000"),
            n("53000"),
        )
        .unwrap();
        assert_eq!(p.labor.employee, n("1008"));
        assert_eq!(p.health.employee, n("822"));

        let wht = laysi_regulation::resolve("withholding", d(2026, 7, 5)).unwrap();
        let (rate, name, _) = salary_withholding_rate(wht).unwrap();
        assert_eq!(name, "薪資所得");
        let tax = to_dollars(n("53000") * rate);
        assert_eq!(tax, n("2650"), "53,000 × 扣繳率");

        let deduction = p.labor.employee + p.health.employee + tax;
        assert_eq!(deduction, n("4480"), "代扣合計");
        assert_eq!(n("53000") - deduction, n("48520"), "實發");

        // The three employer-side contributions, the other reference values.
        assert_eq!(p.labor.employer, n("3527"));
        assert_eq!(p.health.employer, n("2647"));
        assert_eq!(p.pension, n("3180"));
    }

    /// The withholding rate comes from the rules too: change the rules and the
    /// same salary yields a different deduction.
    #[test]
    fn the_withholding_rate_comes_from_the_ruleset_not_from_this_file() {
        let rs = laysi_regulation::resolve("withholding", d(2026, 7, 5)).unwrap();
        let (rate, _, _) = salary_withholding_rate(rs).unwrap();
        assert_eq!(to_dollars(n("53000") * rate), n("2650"));

        // Simulate the 2027 salary withholding rate rising to 6%. Not one
        // character of this file changes.
        let mut next = rs.clone();
        next.version = "2027.1".into();
        next.effective_from = d(2027, 1, 1);
        next.payload["codes"][0]["rate"] = json!("0.06");
        let (r2, _, _) = salary_withholding_rate(&next).unwrap();
        assert_eq!(to_dollars(n("53000") * r2), n("3180"));

        // Figures computed under the old rules do not move.
        assert_eq!(to_dollars(n("53000") * rate), n("2650"));
    }

    /// Joining mid-period prorates by calendar day; a full month pays in full.
    #[test]
    fn a_mid_month_hire_is_prorated_by_calendar_days() {
        let (from, to) = month_bounds("2026-07").unwrap();
        assert_eq!((to - from).num_days() + 1, 31);

        let full = emp(d(2019, 6, 17), None);
        assert_eq!(paid_days(&full, from, to), 31, "整月在職");

        // Joined 7/16, so 16..31 is 16 days.
        let joined = emp(d(2026, 7, 16), None);
        assert_eq!(paid_days(&joined, from, to), 16);
        assert_eq!(
            to_dollars(n("31000") * Decimal::from(16) / Decimal::from(31)),
            n("16000")
        );

        // Someone leaving on 8/15 still worked all of July.
        let leaving = emp(d(2025, 3, 3), Some(d(2026, 8, 15)));
        assert_eq!(paid_days(&leaving, from, to), 31);
        assert!(in_service(&leaving, from, to));

        // Someone who left on 5/31 must not appear in the July run.
        let gone = emp(d(2023, 11, 6), Some(d(2026, 5, 31)));
        assert!(!in_service(&gone, from, to));
        assert_eq!(paid_days(&gone, from, to), 0);

        // Nor must someone who has not started.
        let future = emp(d(2026, 9, 1), None);
        assert!(!in_service(&future, from, to));
    }

    /// The month of departure is still paid, prorated to the leaving date.
    #[test]
    fn the_month_someone_leaves_is_still_paid_pro_rata() {
        let (from, to) = month_bounds("2026-08").unwrap();
        let leaving = emp(d(2025, 3, 3), Some(d(2026, 8, 15)));
        assert!(in_service(&leaving, from, to));
        assert_eq!(paid_days(&leaving, from, to), 15);
    }

    #[test]
    fn periods_parse_into_their_own_bounds() {
        assert_eq!(
            month_bounds("2026-07").unwrap(),
            (d(2026, 7, 1), d(2026, 7, 31))
        );
        assert_eq!(
            month_bounds("2026-02").unwrap(),
            (d(2026, 2, 1), d(2026, 2, 28))
        );
        assert_eq!(
            month_bounds("2026-12").unwrap(),
            (d(2026, 12, 1), d(2026, 12, 31))
        );
        assert!(month_bounds("2026/07").is_none());
        assert!(resolve_period(Some("七月".into()), d(2026, 1, 1)).is_err());
        assert_eq!(resolve_period(None, d(2026, 7, 26)).unwrap().0, "2026-07");
    }

    /// A withholding standard with no salary code must error rather than
    /// defaulting to 5%.
    #[test]
    fn a_missing_salary_income_code_is_refused_never_guessed() {
        let rs = laysi_regulation::resolve("withholding", d(2026, 7, 5)).unwrap();
        let mut broken = rs.clone();
        broken.payload["codes"] = json!([]);
        let err = salary_withholding_rate(&broken).unwrap_err();
        assert!(
            err.to_string().contains("50"),
            "錯誤訊息要說是哪個代號：{err}"
        );
    }

    fn no_schedule() -> WorkSchedule {
        WorkSchedule {
            starts_at: None,
            ends_at: None,
            grace_minutes: 0,
            grace_stated: false,
            flex_minutes: 0,
            break_start: None,
            break_end: None,
            calendar: BTreeMap::new(),
        }
    }

    /// Hours are counted per person. The punch rows arrive ordered by employee
    /// and then by date, so a run that ignored the employee would fold two
    /// people's punches on the same date into one day and pay one of them
    /// nothing.
    #[test]
    fn worked_hours_are_counted_one_person_at_a_time() {
        use crate::ops::attendance::business_instant;
        use chrono::NaiveTime;
        use laysi_store::AttendancePunch;

        let tz = laysi_domain::default_business_offset();
        let day = d(2026, 8, 3);
        let at =
            |t: &str| business_instant(day, NaiveTime::parse_from_str(t, "%H:%M").unwrap(), tz);
        let punch = |employee_id: i32, kind: &str, t: &str| AttendancePunch {
            id: 1,
            employee_id,
            emp_no: format!("E00{employee_id}"),
            name: "測試員工".into(),
            dept_label: None,
            punched_on: day,
            punched_at: at(t),
            kind: kind.into(),
            source: "clock".into(),
            note: String::new(),
            created_by: "測試員工".into(),
        };
        let schedule = no_schedule();
        let hours = |p: &[AttendancePunch], id: i32| {
            ordinary_hours(&punch_days_of(p, id, tz), &schedule, &[])
        };

        // Ordered by employee, then date, exactly as the query returns them.
        let punches = vec![
            punch(1, "in", "09:00"),
            punch(1, "out", "13:00"),
            punch(2, "in", "14:00"),
            punch(2, "out", "18:30"),
        ];
        assert_eq!(hours(&punches, 1), n("4"));
        assert_eq!(hours(&punches, 2), n("4.5"));
        assert_eq!(hours(&punches, 3), Money::ZERO, "沒有打卡的人是 0 小時");

        // A day clocked in and never out has no length, so it earns nothing
        // until the correction is made.
        let open = vec![punch(1, "in", "09:00")];
        assert_eq!(hours(&open, 1), Money::ZERO);
    }

    /// The two bases reach the ordinary hourly wage differently: an hourly
    /// rate already is one, and dividing it by the ruleset's monthly hours
    /// would price an hour of overtime below an hour of ordinary time.
    #[test]
    fn overtime_prices_an_hourly_worker_off_their_own_rate() {
        let rs = laysi_regulation::resolve("overtime", d(2026, 8, 3)).unwrap();
        assert_eq!(
            ordinary_hourly_wage(rs, BASIS_HOURLY, n("200")).unwrap(),
            n("200")
        );
        assert_eq!(
            ordinary_hourly_wage(rs, BASIS_MONTHLY, n("48000")).unwrap(),
            n("200"),
            "48,000 ÷ 240 小時"
        );
        assert_eq!(
            laysi_regulation::overtime_pay(rs, n("200"), "weekday", n("2")).unwrap(),
            n("536"),
            "200 × 1.34 × 2"
        );
    }

    /// Approved overtime already carries the ordinary wage in its art. 24
    /// multiplier, so the plain rate pays only the punched time it does not
    /// cover.
    #[test]
    fn approved_overtime_is_not_paid_again_at_the_hourly_rate() {
        use chrono::NaiveTime;

        let rs = laysi_regulation::resolve("overtime", d(2026, 8, 12)).unwrap();
        let schedule = no_schedule();
        let at = |day: u32, t: &str| {
            d(2026, 8, day).and_time(NaiveTime::parse_from_str(t, "%H:%M").unwrap())
        };
        let punched = |day: u32, from: &str, to: &str| PunchDay {
            date: d(2026, 8, day),
            arrived: Some(at(day, from)),
            left: Some(at(day, to)),
            ..Default::default()
        };
        let rate = n("200");

        // Punched 13:00 to 20:00 with 17:00 to 20:00 approved.
        let plain = ordinary_hours(
            &[punched(12, "13:00", "20:00")],
            &schedule,
            &[(at(12, "17:00"), at(12, "20:00"))],
        );
        let overtime = laysi_regulation::overtime_pay(rs, rate, "weekday", n("3")).unwrap();
        assert_eq!(plain, n("4"));
        assert_eq!(overtime, n("870"), "200 × (2 × 1.34 + 1 × 1.67)");
        assert_eq!(to_dollars(rate * plain) + overtime, n("1670"));

        // Punched out at 18:00 and stayed until 20:00: the overtime was never
        // in the punched time.
        assert_eq!(
            ordinary_hours(
                &[punched(12, "09:00", "18:00")],
                &schedule,
                &[(at(12, "18:00"), at(12, "20:00"))],
            ),
            n("9"),
            "打下班卡之後的加班不得從本薪扣"
        );

        // Overtime on a day with a missed punch comes out of no other day.
        assert_eq!(
            ordinary_hours(
                &[punched(12, "13:00", "20:00")],
                &schedule,
                &[(at(13, "17:00"), at(13, "20:00"))],
            ),
            n("7"),
            "漏打卡那天的加班不得扣到別天"
        );

        // A whole rest day approved as overtime: the break is in neither the
        // punched time nor the part of it the overtime covers.
        let with_break = WorkSchedule {
            break_start: NaiveTime::from_hms_opt(12, 0, 0),
            break_end: NaiveTime::from_hms_opt(13, 0, 0),
            ..no_schedule()
        };
        assert_eq!(
            ordinary_hours(
                &[punched(15, "09:00", "18:00")],
                &with_break,
                &[(at(15, "09:00"), at(15, "18:00"))],
            ),
            Money::ZERO
        );
    }

    fn office() -> WorkSchedule {
        let t = |h| NaiveTime::from_hms_opt(h, 0, 0);
        let mut s = no_schedule();
        (s.starts_at, s.ends_at, s.break_start, s.break_end) = (t(9), t(17), t(12), t(13));
        s.calendar
            .insert(d(2026, 5, 1), (DayKind::Holiday, "勞動節".into()));
        s
    }

    fn leave(
        no: &str,
        kind: &str,
        status: &str,
        from: &str,
        to: &str,
        hours: &str,
    ) -> LeaveRequest {
        let at = |v: &str| NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M").unwrap();
        LeaveRequest {
            leave_no: no.into(),
            employee_id: 1,
            emp_no: "TEST-001".into(),
            name: "測試員工".into(),
            dept_label: None,
            kind: kind.into(),
            from_at: at(from),
            to_at: at(to),
            from_on: at(from).date(),
            to_on: at(to).date(),
            hours: n(hours),
            days: n(hours) / n("7"),
            reason: String::new(),
            status: status.into(),
            decided_by: String::new(),
            decided_role: String::new(),
            decided_at: None,
            decision_note: String::new(),
            created_at: Utc::now(),
            created_by: String::new(),
        }
    }

    /// The case #101 was opened on: May, a personal leave day that started in
    /// April and two days of sickness leave. A day is the salary over the
    /// month's 31 calendar days; personal leave pays none of it, sickness
    /// leave half. A request still waiting for a decision pays nothing yet.
    #[test]
    fn a_monthly_salary_loses_the_share_its_leave_does_not_pay() {
        let mut e = emp(d(2026, 4, 15), None);
        e.base_salary = n("42000");
        let requests = [
            leave(
                "LV1",
                "personal",
                "approved",
                "2026-04-30T09:00",
                "2026-05-04T17:00",
                "14",
            ),
            leave(
                "LV2",
                "sick",
                "approved",
                "2026-05-26T09:00",
                "2026-05-27T17:00",
                "14",
            ),
            leave(
                "LV3",
                "personal",
                "pending",
                "2026-05-20T09:00",
                "2026-05-20T17:00",
                "7",
            ),
        ];
        let requests: Vec<&LeaveRequest> = requests.iter().collect();
        let (from, to) = (d(2026, 5, 1), d(2026, 5, 31));
        let leave = leave_in_period(&e, &requests, &office(), from, to).unwrap();
        assert_eq!(
            leave["personal"].days,
            n("1"),
            "四月那一天不算進五月，勞動節與週末不佔"
        );
        assert_eq!(leave["personal"].paid_days, n("0"));
        assert_eq!(leave["sick"].paid_days, n("1"));

        let c = compute_one(
            &e,
            from,
            to,
            d(2026, 6, 5),
            Money::ZERO,
            Money::ZERO,
            &leave,
        )
        .unwrap();
        assert_eq!(c.slip.base_salary, n("39290"), "42,000 − 1,355 − 1,355");
        assert_eq!(c.slip.note, "事假 1 日扣 1,355、病假 2 日扣 1,355");
    }

    /// A request that starts in December is split over all its dates, but only
    /// January's are priced: December belongs to last year's payroll, and this
    /// database has no ruleset for it.
    #[test]
    fn a_request_from_last_year_prices_only_this_years_days() {
        let e = emp(d(2025, 3, 3), None);
        let requests = [leave(
            "LV5",
            "annual",
            "approved",
            "2025-12-29T09:00",
            "2026-01-02T17:00",
            "35",
        )];
        let requests: Vec<&LeaveRequest> = requests.iter().collect();
        let leave = leave_in_period(&e, &requests, &office(), d(2026, 1, 1), d(2026, 1, 31))
            .expect("去年的日子不該讓整期計薪失敗");
        assert_eq!(leave["annual"].days, n("2"), "五天裡落在一月的兩天");
        assert_eq!(leave["annual"].paid_days, n("2"));
    }

    /// An hourly wage is paid on hours, so leave adds the share it pays of the
    /// hours on leave rather than taking anything away.
    #[test]
    fn an_hourly_wage_gains_the_share_its_leave_pays() {
        let mut e = emp(d(2026, 5, 4), None);
        (e.pay_basis, e.base_salary) = (BASIS_HOURLY.into(), n("200"));
        let requests = [leave(
            "LV4",
            "sick",
            "approved",
            "2026-05-27T13:00",
            "2026-05-27T17:00",
            "4",
        )];
        let requests: Vec<&LeaveRequest> = requests.iter().collect();
        let (from, to) = (d(2026, 5, 1), d(2026, 5, 31));
        let leave = leave_in_period(&e, &requests, &office(), from, to).unwrap();
        let c = compute_one(
            &e,
            from,
            to,
            d(2026, 6, 5),
            Money::ZERO,
            n("40.644722"),
            &leave,
        )
        .unwrap();
        assert_eq!(
            c.slip.base_salary,
            n("8529"),
            "200 × 40.64 小時 + 病假 4 小時折半 400"
        );
        assert_eq!(
            c.slip.note,
            "時薪制，本期正常工時 40.64 小時；病假 4 小時計 400"
        );
    }
}
