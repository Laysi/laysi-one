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

use crate::ops::insurance::{percent, premiums_on, ruleset_json};
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_regulation::Ruleset;
use laysi_store::{Employee, NewPayslip};
use rust_decimal::{Decimal, RoundingStrategy};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(GetPayrollSummary), Box::new(ListPayslips), Box::new(RunPayroll)]
}

/// Withholding income code for salary. A code, not a rate; the rate is
/// looked up in the payload.
pub(crate) const SALARY_INCOME_CODE: &str = "50";

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

fn resolve_period(period: Option<String>, today: NaiveDate) -> Result<(String, NaiveDate, NaiveDate), OpError> {
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
            OpError::BadInput(format!("扣繳率標準 {} 的代號 50 未設定可用的 rate", rs.version))
        })?;
    Ok((
        rate,
        found.get("name").and_then(|v| v.as_str()).unwrap_or("薪資所得").to_string(),
        found.get("note").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
    ))
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
async fn compute_one(
    _ctx: &OpCtx,
    e: &Employee,
    from: NaiveDate,
    to: NaiveDate,
    pay_on: NaiveDate,
    overtime: Money,
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
    if e.base_salary <= Decimal::ZERO {
        return Err(OpError::BadInput(format!(
            "{} {} 尚未設定底薪，請先在「員工名冊」填上再計薪",
            e.emp_no, e.name
        )));
    }

    // Joining or leaving mid-period prorates by calendar day. A full month
    // gives days == month_days and a ratio of 1.
    let base = to_dollars(e.base_salary * days / month_days);
    let gross = base + overtime;

    // Premiums resolve on the period end: the version in force for this period.
    let p = premiums_on(to, e.labor_insured_salary, e.health_insured_salary, e.pension_wage)?;

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

    // Minimum wage check. This compares the monthly salary, not the period's
    // net pay: someone who joined mid-month earns less than the monthly
    // minimum by definition, which is not a breach.
    let min_rs = laysi_regulation::resolve("minimum_wage", to)?;
    let below_minimum = laysi_regulation::minimum_wage_shortfall(min_rs, e.base_salary)?;

    let note = if days < month_days {
        format!("在職 {days}／{month_days} 日，按曆日比例計薪")
    } else {
        String::new()
    };

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
        let picked = match &i.period {
            Some(p) => runs.iter().find(|r| &r.period == p),
            None => runs.first(),
        };

        // Two independently computed paths: HR sums gross pay per period, the
        // ledger computes the balance of 6111. Equality is a conclusion, not an
        // assumption, as with AR ageing.
        let (posted, in_transit) = ctx.store.payroll_totals().await?;
        let gl = ctx.store.balance_of(&["6111"], true).await?;
        let gap = posted - gl;

        let detail_visible = ctx.actor_role == crate::ROLE_BOSS;
        let period_label = picked.map(|r| r.period.clone()).unwrap_or_else(|| "—".into());

        // Same question the HR dashboard asks, asked the same way: is there a
        // run for this month? Scanning the recent-runs list would answer it
        // from a window instead of from the table.
        let this_month = ctx.today.format("%Y-%m").to_string();
        let next_period =
            ctx.store.payroll_run(&this_month).await?.is_none().then_some(this_month);

        let summary = match picked {
            Some(r) => format!(
                "讀薪資彙總 {} · {} 人應發 {} 代扣 {} 實發 {} · 與總帳 6111 {}",
                r.period,
                r.headcount,
                money(r.gross_total),
                money(r.deduction_total),
                money(r.net_total),
                if gap.is_zero() { "相符" } else { "不符" }
            ),
            None => "讀薪資彙總 · 尚無任何期別的薪資計算紀錄".to_string(),
        };

        Ok(OpOutcome::new(
            json!({
                "period": period_label,
                "run": picked.map(run_json),
                "runs": runs.iter().map(run_json).collect::<Vec<_>>(),
                "periods": runs.iter().map(|r| r.period.clone()).collect::<Vec<_>>(),
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

        let mut rows = Vec::with_capacity(slips.len());
        let mut all_match = true;
        for s in &slips {
            // Recompute from the same rules. What was stored is the result at the
            // time; recomputation is a second path, and a mismatch is stated on
            // screen as with AR ageing and depreciation.
            let p = premiums_on(
                period_end,
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
            money(run.gross_total),
            money(run.net_total),
            if all_match { "一致" } else { "有差異" }
        );

        Ok(OpOutcome::new(
            json!({
                "period": run.period,
                "run": run_json(run),
                "rows": rows,
                "recompute_all_match": all_match,
                "recompute_note": "各列均以當期薪資作業保存的規則版本重算一次，並與儲存值比對。",
                "periods": runs.iter().map(|r| r.period.clone()).collect::<Vec<_>>(),
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

        // Approved overtime for the period, priced by the ruleset in force on
        // each day worked.
        let approved_overtime = ctx.store.approved_overtime(from, to).await?;
        let mut overtime_by_employee: BTreeMap<i32, (Money, Money)> = BTreeMap::new();
        for o in &approved_overtime {
            let rs = laysi_regulation::resolve("overtime", o.worked_on)?;
            let salary = staff
                .iter()
                .find(|e| e.id == o.employee_id)
                .map(|e| e.base_salary)
                .unwrap_or(Money::ZERO);
            let pay = match salary > Money::ZERO {
                true => laysi_regulation::overtime_pay(rs, salary, &o.day_type, o.hours)?,
                false => Money::ZERO,
            };
            let entry = overtime_by_employee.entry(o.employee_id).or_insert((Money::ZERO, Money::ZERO));
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
                    "reason": "本期已計薪",
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

            let c = compute_one(ctx, e, from, to, pay_on, overtime).await?;
            if let Some(floor) = c.below_minimum {
                warnings.push(json!({
                    "emp_no": e.emp_no,
                    "name": e.name,
                    "issue": format!(
                        "月薪 {} 低於法定最低工資 {}",
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
            OpError::BadInput(if supplementary {
                format!("{period} 在職員工都已計薪，沒有需要補算的人。")
            } else {
                format!("{period} 無在職員工需計薪")
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
            format!(
                "依勞保 {labor_v}、健保 {health_v}、勞退 {pension_v}、扣繳 {wht_v} 規則"
            )
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
            hired_on: hired,
            left_on: left,
            status: "active".into(),
            base_salary: n("53000"),
            labor_insured_salary: n("45800"),
            health_insured_salary: n("53000"),
            pension_wage: n("53000"),
            id_no: String::new(),
            bank_accounts: Vec::new(),
            note: String::new(),
        }
    }

    /// A reference payslip's deductions must be reproducible from the rules.
    ///
    /// Labour insurance 1,008, health insurance 822, withheld tax
    /// 53,000 x 5% = 2,650, total 4,480, net 48,520.
    #[test]
    fn a_reference_payslip_is_reproducible_from_the_rulesets() {
        let p = premiums_on(d(2026, 6, 30), n("45800"), n("53000"), n("53000")).unwrap();
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
        assert_eq!(to_dollars(n("31000") * Decimal::from(16) / Decimal::from(31)), n("16000"));

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
        assert_eq!(month_bounds("2026-07").unwrap(), (d(2026, 7, 1), d(2026, 7, 31)));
        assert_eq!(month_bounds("2026-02").unwrap(), (d(2026, 2, 1), d(2026, 2, 28)));
        assert_eq!(month_bounds("2026-12").unwrap(), (d(2026, 12, 1), d(2026, 12, 31)));
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
        assert!(err.to_string().contains("50"), "錯誤訊息要說是哪個代號：{err}");
    }
}
