//! HR compliance checks and annual withholding statements.
//!
//! [`GetHrCompliance`] splits every item into `checked` and `passed`. Minimum
//! wage, the insured salary ceilings and leave entitlement have usable rules,
//! so they are checked per employee with the ruleset attached. The full
//! insured-salary brackets and overtime multipliers do not, so they return
//! `checked = false` with a `not_checked_reason`.
//!
//! Every check carries a `screen`: where the finding is fixed. The failure
//! message states what is wrong and the caller draws the way there, so no
//! message names a screen in prose.
//!
//! Whether a bracket was filed at all needs no ruleset: employment, pay and a
//! bracket of zero answer it between them, so that item is checked and can
//! fail. Recomputing the premiums cannot answer it, because zero recomputes
//! to zero and reports no difference — which is why recomputation is not
//! itself an item: it can only ever pass, and the figures it would show are
//! the insurance screen's own.
//!
//! Labour insurance, health insurance and pension all go through
//! [`crate::ops::insurance::premiums_on`]; this module holds no rates,
//! brackets or thresholds. If no ruleset applies to the query date the whole
//! operation fails rather than borrowing another year's rules.
//!
//! Withholding statements keep only the precision the source can justify:
//! `rate` records are computed from the `withholding` ruleset as of the
//! payment date, `table` records carry the tax supplied by the source system,
//! and a missing figure stays `null` with a warning. Sources that merge
//! several people into one row are flagged as not per-person; the payroll
//! module's per-person annual totals are listed separately for reconciliation
//! and never used to fill the gap.

use crate::ops::attendance::{
    annual_balances, punch_days, summarise, OvertimeLimits, WorkSchedule,
};
use crate::ops::employees::{pay_basis_label, BASIS_HOURLY, INSURED_EMPLOYEE, INSURED_OWNER};
use crate::ops::insurance::{missing_brackets, premiums_on, ruleset_json, Coverage, Enrolment};
use crate::ops::payroll::SALARY_INCOME_CODE;
use crate::ops::tax::{code_rule, withhold};
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_store::Employee;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetHrCompliance),
        Box::new(ListWithholdingStatements),
    ]
}

// HR compliance

/// Where a finding of each kind is fixed. The caller turns these into the way
/// there; the same codes the HR dashboard uses.
const SCREEN_EMPLOYEES: &str = "hr";
const SCREEN_ATTENDANCE: &str = "hr_att";

struct GetHrCompliance;

fn unchecked_check(
    name: &str,
    rulesets: Vec<Value>,
    required_domain: Option<&str>,
    reason: &str,
) -> Value {
    let mut check = json!({
        "name": name,
        "checked": false,
        "passed": null,
        "rulesets": rulesets,
        "failures": [],
        "not_checked_reason": reason,
    });
    if let Some(domain) = required_domain {
        check["required_domain"] = json!(domain);
    }
    check
}

/// Employees who are working and paid but have no filed insured bracket.
///
/// The finding is that nothing was filed, which the employment facts answer
/// on their own, so this carries no ruleset. What the bracket should be is a
/// different question, and the table that would answer it is still reported
/// as not checked.
fn unfiled_bracket_failures(staff: &[Employee]) -> Vec<Value> {
    staff
        .iter()
        .filter_map(|employee| {
            let missing = missing_brackets(employee);
            if !missing.any() {
                return None;
            }
            Some(json!({
                "emp_no": employee.emp_no,
                "name": employee.name,
                "missing": missing.labels(),
                "base_salary": employee.base_salary.to_string(),
                "message": format!(
                    "本薪 {}，{} 還沒設定",
                    money(employee.base_salary),
                    missing.labels().join("、"),
                ),
            }))
        })
        .collect()
}

fn coverage_counts(checks: &[Value]) -> (usize, usize) {
    let checked = checks
        .iter()
        .filter(|check| check.get("checked").and_then(Value::as_bool) == Some(true))
        .count();
    let unchecked = checks
        .iter()
        .filter(|check| check.get("checked").and_then(Value::as_bool) == Some(false))
        .count();
    (checked, unchecked)
}

#[async_trait]
impl Operation for GetHrCompliance {
    fn name(&self) -> &'static str {
        "get_hr_compliance"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀人資法遵檢核表：逐人檢查最低工資、投保級距有沒有填、投保薪資計費上限、延長工時上限（已核准的加班申請與打卡推算各一項）與特休餘額。規則不足而未檢查的項目明確標示，不視為通過。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<crate::AsOfInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: crate::AsOfInput = parse(input)?;
        let as_of = i.as_of.unwrap_or(ctx.today);
        let staff = ctx
            .store
            .employees(true, None)
            .await?
            .into_iter()
            .filter(|employee| {
                employee.hired_on <= as_of && employee.left_on.is_none_or(|left| left >= as_of)
            })
            .collect::<Vec<_>>();

        let minimum_rs = laysi_regulation::resolve("minimum_wage", as_of)?;
        let annual_rs = laysi_regulation::resolve("annual_leave", as_of)?;
        let premium_rules = premiums_on(
            as_of,
            &Enrolment {
                insured_as: INSURED_EMPLOYEE,
                employment_insured: true,
                labor_insured: Money::ZERO,
                health_insured: Money::ZERO,
                pension_wage: Money::ZERO,
                covered_dependants: 0,
            },
            Coverage::WHOLE,
            None,
        )?;

        let mut minimum_failures = Vec::new();
        for employee in &staff {
            // The floor follows the pay basis: an hourly rate held against the
            // monthly floor fails every time and says nothing.
            if let Some(required) = laysi_regulation::minimum_wage_shortfall(
                minimum_rs,
                employee.base_salary,
                &employee.pay_basis,
            )? {
                minimum_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "actual": employee.base_salary.to_string(),
                    "required": required.to_string(),
                    "difference": (required - employee.base_salary).to_string(),
                    "message": format!(
                        "{} {}，比規則下限少 {}",
                        pay_basis_label(&employee.pay_basis),
                        money(employee.base_salary),
                        money(required - employee.base_salary)
                    ),
                }));
            }
        }
        let minimum_failure_count = minimum_failures.len();

        let unfiled_failures = unfiled_bracket_failures(&staff);
        let unfiled_failure_count = unfiled_failures.len();

        // The labour bracket is the one salary filed, graded up to the labour
        // ceiling for labour insurance and up to the occupational accident
        // ceiling for that insurance, so only a figure above the second is
        // above every schedule. Only the ceilings are read here, and neither
        // dependants nor the days of the month move them.
        let mut labor_cap_failures = Vec::new();
        for employee in &staff {
            let premiums = premiums_on(
                as_of,
                &Enrolment {
                    covered_dependants: 0,
                    ..Enrolment::of(employee, as_of)
                },
                Coverage::WHOLE,
                None,
            )?;
            if employee.labor_insured_salary > premiums.accident_base {
                labor_cap_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "reported": employee.labor_insured_salary.to_string(),
                    "charged_on": premiums.accident_base.to_string(),
                    "difference": (employee.labor_insured_salary - premiums.accident_base).to_string(),
                    "message": format!(
                        "申報投保薪資 {}，高於職災保險上限 {}，請確認申報級距",
                        money(employee.labor_insured_salary),
                        money(premiums.accident_base)
                    ),
                }));
            }
        }
        let labor_cap_failure_count = labor_cap_failures.len();

        // An owner's health insurance salary may not be below the highest one
        // filed for anybody they employ (National Health Insurance Act art. 20
        // subpara. 1). Nothing on the roster stops it being typed that way, and
        // the bureau finds it at the next audit rather than at the keyboard.
        let highest_employee_bracket = staff
            .iter()
            .filter(|e| e.insured_as != INSURED_OWNER)
            .map(|e| e.health_insured_salary)
            .max()
            .unwrap_or(Money::ZERO);
        let mut owner_bracket_failures = Vec::new();
        for employee in staff
            .iter()
            .filter(|e| e.insured_as == INSURED_OWNER && !e.health_insured_salary.is_zero())
        {
            if employee.health_insured_salary < highest_employee_bracket {
                owner_bracket_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "actual": employee.health_insured_salary.to_string(),
                    "required": highest_employee_bracket.to_string(),
                    "difference": (highest_employee_bracket - employee.health_insured_salary)
                        .to_string(),
                    "message": format!(
                        "健保投保金額 {}，低於受僱員工最高的 {}",
                        money(employee.health_insured_salary),
                        money(highest_employee_bracket)
                    ),
                }));
            }
        }
        let owner_bracket_failure_count = owner_bracket_failures.len();
        let owner_count = staff
            .iter()
            .filter(|e| e.insured_as == INSURED_OWNER)
            .count();

        let mut annual_failures = Vec::new();
        // Part-time leave is granted in proportion to agreed working hours,
        // and the roster does not record those hours. Comparing them against
        // the full-time entitlement would pass everybody regardless.
        let part_time: Vec<&Employee> = staff
            .iter()
            .filter(|e| e.employment_type == "part_time" || e.pay_basis == BASIS_HOURLY)
            .collect();
        let full_time: Vec<(&Employee, NaiveDate)> = staff
            .iter()
            .filter(|e| e.employment_type != "part_time" && e.pay_basis != BASIS_HOURLY)
            .map(|e| (e, as_of))
            .collect();
        let balances = annual_balances(ctx, annual_rs, &full_time).await?;
        for ((employee, _), balance) in full_time.iter().zip(&balances) {
            let (entitlement, used) = (balance.entitlement(), balance.taken);
            if used > entitlement {
                annual_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "entitlement": entitlement.to_string(),
                    "taken": used.to_string(),
                    "difference": (used - entitlement).to_string(),
                    "from_on": balance.year.from,
                    "to_on": balance.year.to,
                    "message": format!(
                        "{}～{} 已核准特休 {} 日，比規則額度多 {} 日，請確認是否為公司加給或資料有誤",
                        balance.year.from,
                        balance.year.to,
                        used.normalize(),
                        (used - entitlement).normalize()
                    ),
                }));
            }
        }
        let annual_failure_count = annual_failures.len();

        // Extended working time against the monthly ceiling. Approved
        // requests are the hours the company agreed to, so they are what the
        // ceiling applies to; a pending one has not been agreed to yet.
        let overtime_rs = laysi_regulation::resolve("overtime", as_of)?;
        let overtime_ceiling = laysi_regulation::overtime_monthly_max_hours(overtime_rs)?;
        let month = as_of.format("%Y-%m").to_string();
        let mut overtime_hours: BTreeMap<i32, Money> = BTreeMap::new();
        for request in ctx
            .store
            .overtime_requests(true, None, Some(&month))
            .await?
        {
            if request.status == "approved" {
                *overtime_hours
                    .entry(request.employee_id)
                    .or_insert(Money::ZERO) += request.hours;
            }
        }
        let mut overtime_failures = Vec::new();
        for employee in &staff {
            let hours = overtime_hours
                .get(&employee.id)
                .copied()
                .unwrap_or(Money::ZERO);
            if hours > overtime_ceiling {
                overtime_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "approved": hours.normalize().to_string(),
                    "ceiling": overtime_ceiling.normalize().to_string(),
                    "difference": (hours - overtime_ceiling).normalize().to_string(),
                    "message": format!(
                        "{month} 已核准加班 {} 小時，比規則上限多 {} 小時",
                        hours.normalize(),
                        (hours - overtime_ceiling).normalize()
                    ),
                }));
            }
        }
        let overtime_failure_count = overtime_failures.len();

        // The same ceilings against the punch record. Approved requests are
        // what the company agreed to; punches are what an inspection reads,
        // and a day worked past article 32 is a finding whether or not anyone
        // filed for it. Unanswerable without a working day to measure from.
        let schedule = WorkSchedule::load(ctx).await?;
        let limits = OvertimeLimits::resolve(as_of)?;
        let mut punched: BTreeMap<i32, Vec<laysi_store::AttendancePunch>> = BTreeMap::new();
        for punch in ctx.store.punches(&month, None).await? {
            punched.entry(punch.employee_id).or_default().push(punch);
        }
        let mut punch_failures = Vec::new();
        for employee in &staff {
            let Some(punches) = punched.get(&employee.id) else {
                continue;
            };
            let stats = summarise(&punch_days(punches, ctx.offset), &schedule, &limits);
            let hours = stats
                .overtime_seconds
                .map(|seconds| Money::from(seconds) / Money::from(3600))
                .unwrap_or(Money::ZERO)
                .round_dp(1);
            let message = match (stats.over_monthly_limit, stats.over_limit_days) {
                (true, _) => Some(format!(
                    "{month} 打卡推算的加班 {} 小時，比規則上限多 {} 小時",
                    hours.normalize(),
                    (hours - stats.monthly_max_hours).normalize()
                )),
                (false, days) if days > 0 => {
                    Some(format!("{month} 有 {days} 天打卡推算的加班超過單日上限"))
                }
                _ => None,
            };
            if let Some(message) = message {
                punch_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "hours": hours.normalize().to_string(),
                    "over_limit_days": stats.over_limit_days,
                    "ceiling": stats.monthly_max_hours.normalize().to_string(),
                    "message": message,
                }));
            }
        }
        let punch_failure_count = punch_failures.len();
        let punch_checked = schedule.counts_overtime();

        let mut checks = vec![
            json!({
                "name": "最低工資",
                "checked": true,
                "passed": minimum_failures.is_empty(),
                "rulesets": [ruleset_json(minimum_rs)],
                "failures": minimum_failures,
                "screen": SCREEN_EMPLOYEES,
                "scope_note": "中途到職當月的實發金額不列入比較。",
            }),
            json!({
                "name": "在職員工的投保級距",
                "checked": true,
                "passed": unfiled_failures.is_empty(),
                "rulesets": [],
                "failures": unfiled_failures,
                "screen": SCREEN_EMPLOYEES,
                "scope_note": "本薪為 0 的員工不列入。",
            }),
            json!({
                "name": "投保薪資申報值與計費上限",
                "checked": true,
                "passed": labor_cap_failures.is_empty(),
                "rulesets": [
                    ruleset_json(premium_rules.labor_rs),
                    ruleset_json(premium_rules.accident_rs),
                ],
                "failures": labor_cap_failures,
                "screen": SCREEN_EMPLOYEES,
            }),
            json!({
                "name": "負責人的健保投保金額",
                "checked": owner_count > 0,
                "not_checked_reason": (owner_count == 0)
                    .then_some("名冊上沒有以負責人身分投保的人。"),
                "passed": owner_bracket_failures.is_empty(),
                "rulesets": [],
                "failures": owner_bracket_failures,
                "screen": SCREEN_EMPLOYEES,
                "scope_note": "尚未填投保金額的不列入。",
            }),
            json!({
                "name": "每月延長工時上限",
                "checked": true,
                "passed": overtime_failures.is_empty(),
                "rulesets": [ruleset_json(overtime_rs)],
                "failures": overtime_failures,
                "screen": SCREEN_ATTENDANCE,
                "scope_note": format!("{month} 已核准的加班，等簽核的申請不計入。"),
            }),
            json!({
                "name": "打卡推算的延長工時",
                "checked": punch_checked,
                "passed": punch_checked.then_some(punch_failures.is_empty()),
                "rulesets": [ruleset_json(overtime_rs)],
                "failures": punch_failures,
                "screen": SCREEN_ATTENDANCE,
                "not_checked_reason": (!punch_checked)
                    .then_some("上下班時間還沒填，打卡無法計算加班時數，請系統管理員設定。"),
                "scope_note": format!("{month} 的打卡紀錄，不論有沒有加班申請。"),
            }),
            json!({
                "name": "特休額度與已核准天數",
                "checked": true,
                "passed": annual_failures.is_empty(),
                "rulesets": [ruleset_json(annual_rs)],
                "failures": annual_failures,
                "screen": SCREEN_ATTENDANCE,
                "scope_note": match part_time.len() {
                    0 => "超出額度可能是公司加給。".to_string(),
                    n => format!(
                        "超出額度可能是公司加給。{n} 位兼職或時薪員工不列入：特休按約定工時比例給予，本系統未記錄約定工時。"
                    ),
                },
            }),
        ];
        let unchecked = vec![
            unchecked_check(
                "完整勞健保投保級距與勞退提繳工資級距",
                vec![
                    ruleset_json(premium_rules.labor_rs),
                    ruleset_json(premium_rules.accident_rs),
                    ruleset_json(premium_rules.health_rs),
                    ruleset_json(premium_rules.pension_rs),
                ],
                None,
                "規則庫只收錄費率、分攤方式與勞保、職災的計費上限，沒有投保薪資分級表，級距請自行核對。",
            ),
            unchecked_check(
                "國定假日與例假出勤工資",
                vec![ruleset_json(overtime_rs)],
                Some("overtime"),
                "規則庫沒有國定假日日曆，無法分辨某一天屬於哪一種，這兩種日子按日加倍的工資請自行核對。",
            ),
        ];
        checks.extend(unchecked);
        let (checked_count, unchecked_count) = coverage_counts(&checks);

        let failure_count = minimum_failure_count
            + unfiled_failure_count
            + labor_cap_failure_count
            + owner_bracket_failure_count
            + overtime_failure_count
            + punch_failure_count
            + annual_failure_count;
        let checked_all_passed = failure_count == usize::default();
        let summary = format!(
            "人資法遵檢核 · {as_of} · 已檢查 {checked_count} 項，{} · {unchecked_count} 項因規則不足未檢查",
            if checked_all_passed {
                "目前無差異".to_string()
            } else {
                format!("發現 {failure_count} 筆待確認差異")
            }
        );

        Ok(OpOutcome::new(
            json!({
                "as_of": as_of,
                "employee_count": staff.len(),
                "checked_all_passed": checked_all_passed,
                "checked_count": checked_count,
                "unchecked_count": unchecked_count,
                "failure_count": failure_count,
                "checks": checks,
            }),
            summary,
        ))
    }
}

// Annual withholding statements

#[derive(Deserialize, JsonSchema, Default)]
pub struct WithholdingStatementsInput {
    /// Income year. Defaults to the current year.
    #[serde(default)]
    pub year: Option<i32>,
}

struct IncomeAccumulator {
    name: String,
    gross: Money,
    known_withheld: Money,
    missing_withheld: bool,
    payment_count: usize,
    bases: BTreeSet<String>,
    basis_labels: BTreeSet<String>,
    ruleset_versions: BTreeSet<String>,
    records: Vec<Value>,
}

struct StatementAccumulator {
    payee: String,
    payee_id: String,
    gross: Money,
    known_withheld: Money,
    missing_withheld: bool,
    payment_count: usize,
    lines: BTreeMap<String, IncomeAccumulator>,
}

fn year_bounds(year: i32) -> Option<(NaiveDate, NaiveDate)> {
    let from = NaiveDate::from_yo_opt(year, 1)?;
    let next_year = year.checked_add(1)?;
    let to = NaiveDate::from_yo_opt(next_year, 1)?.pred_opt()?;
    Some((from, to))
}

struct ListWithholdingStatements;

#[async_trait]
impl Operation for ListWithholdingStatements {
    fn name(&self) -> &'static str {
        "list_withholding_statements"
    }

    fn risk(&self) -> Risk {
        // A withholding statement is a person's full annual income, as
        // sensitive as a per-person payslip.
        Risk::BossOnly
    }

    /// Reads only. Carries the identity floor above, which is a question
    /// about who may call it rather than about what it changes, so the
    /// licence floor has to be told separately.
    fn writes(&self) -> bool {
        false
    }
    fn summary(&self) -> &'static str {
        "讀某年度逐一給付對象的扣繳憑單：各所得格式的全年給付與扣繳稅額。限負責人；依扣繳率的項目自行算稅，依稅額表的項目只採來源帶入的金額，缺值不補算。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<WithholdingStatementsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: WithholdingStatementsInput = parse(input)?;
        let year = i.year.unwrap_or_else(|| ctx.today.year());
        let (from, to) = year_bounds(year)
            .ok_or_else(|| OpError::BadInput(format!("「{year}」不是可用的所得年度")))?;
        let records = ctx.store.withholding_records(from, to).await?;
        let payroll_totals = ctx.store.payroll_year_totals(year).await?;

        let mut statements: BTreeMap<(String, String), StatementAccumulator> = BTreeMap::new();
        let mut used_rulesets: BTreeMap<(String, String), Value> = BTreeMap::new();
        let mut gross_total = Money::ZERO;
        let mut known_withheld_total = Money::ZERO;
        let mut missing_table_amounts = 0usize;
        // Reconciliation against the payroll module runs on the salary income
        // code, not on the basis. Payroll writes its payment records at the
        // ruleset's withholding rate, so counting only `table` records
        // compared payroll against an empty set and reported a whole year of
        // salary as a difference.
        let mut salary_gross = Money::ZERO;
        let mut known_salary_withheld = Money::ZERO;
        let mut salary_withheld_complete = true;

        for record in &records {
            let rs = laysi_regulation::resolve("withholding", record.paid_on)?;
            let rule = code_rule(rs, &record.income_code)?;
            let (withheld, rate, amount_source, basis_label) = match record.basis.as_str() {
                "rate" => (
                    Some(withhold(&rule, record.gross_amount)),
                    Some(rule.rate.to_string()),
                    "ruleset",
                    "依扣繳率標準",
                ),
                "table" => (
                    record.withheld_amount,
                    None,
                    "source_system",
                    "依來源系統的扣繳稅額表",
                ),
                other => {
                    return Err(OpError::BadInput(format!(
                        "{} 的扣繳基礎「{other}」無法計算稅額，請改為 rate 或 table",
                        record.payee
                    )));
                }
            };

            if record.basis == "table" && withheld.is_none() {
                missing_table_amounts += 1;
            }
            if record.income_code == SALARY_INCOME_CODE {
                salary_gross += record.gross_amount;
                match withheld {
                    Some(amount) => known_salary_withheld += amount,
                    None => salary_withheld_complete = false,
                }
            }
            gross_total += record.gross_amount;
            if let Some(amount) = withheld {
                known_withheld_total += amount;
            }
            used_rulesets.insert((rs.domain.clone(), rs.version.clone()), ruleset_json(rs));

            let key = (record.payee.clone(), record.payee_id.clone());
            let statement = statements
                .entry(key)
                .or_insert_with(|| StatementAccumulator {
                    payee: record.payee.clone(),
                    payee_id: record.payee_id.clone(),
                    gross: Money::ZERO,
                    known_withheld: Money::ZERO,
                    missing_withheld: false,
                    payment_count: usize::default(),
                    lines: BTreeMap::new(),
                });
            statement.gross += record.gross_amount;
            statement.payment_count += 1;
            match withheld {
                Some(amount) => statement.known_withheld += amount,
                None => statement.missing_withheld = true,
            }

            let line = statement
                .lines
                .entry(record.income_code.clone())
                .or_insert_with(|| IncomeAccumulator {
                    name: rule.name.clone(),
                    gross: Money::ZERO,
                    known_withheld: Money::ZERO,
                    missing_withheld: false,
                    payment_count: usize::default(),
                    bases: BTreeSet::new(),
                    basis_labels: BTreeSet::new(),
                    ruleset_versions: BTreeSet::new(),
                    records: Vec::new(),
                });
            line.gross += record.gross_amount;
            line.payment_count += 1;
            line.bases.insert(record.basis.clone());
            line.basis_labels.insert(basis_label.to_string());
            line.ruleset_versions.insert(rs.version.clone());
            match withheld {
                Some(amount) => line.known_withheld += amount,
                None => line.missing_withheld = true,
            }
            line.records.push(json!({
                "paid_on": record.paid_on,
                "gross_amount": record.gross_amount.to_string(),
                "withheld_amount": withheld.map(|amount| amount.to_string()),
                "basis": record.basis,
                "basis_label": basis_label,
                "rate": rate,
                "amount_source": amount_source,
                "memo": record.memo,
                "voucher_no": record.voucher_no,
                "ruleset_version": rs.version,
                "warning": (record.basis == "table" && withheld.is_none())
                    .then_some("來源系統沒有帶扣繳稅額，本筆留空，不以扣繳率回推。"),
            }));
        }

        let mut person_specific_count = 0usize;
        let statement_rows = statements
            .into_values()
            .map(|statement| {
                let person_specific = !statement.payee_id.trim().is_empty();
                if person_specific {
                    person_specific_count += 1;
                }
                let income_lines = statement
                    .lines
                    .into_iter()
                    .map(|(code, line)| {
                        json!({
                            "income_code": code,
                            "income_name": line.name,
                            "gross_amount": line.gross.to_string(),
                            "withheld_amount": (!line.missing_withheld)
                                .then(|| line.known_withheld.to_string()),
                            "known_withheld_amount": line.known_withheld.to_string(),
                            "withheld_complete": !line.missing_withheld,
                            "payment_count": line.payment_count,
                            "bases": line.bases,
                            "basis_labels": line.basis_labels,
                            "ruleset_versions": line.ruleset_versions,
                            "records": line.records,
                        })
                    })
                    .collect::<Vec<_>>();

                json!({
                    "payee": statement.payee,
                    "payee_id": statement.payee_id,
                    "person_specific": person_specific,
                    "gross_amount": statement.gross.to_string(),
                    "withheld_amount": (!statement.missing_withheld)
                        .then(|| statement.known_withheld.to_string()),
                    "known_withheld_amount": statement.known_withheld.to_string(),
                    "withheld_complete": !statement.missing_withheld,
                    "payment_count": statement.payment_count,
                    "income_lines": income_lines,
                })
            })
            .collect::<Vec<_>>();

        let payroll_gross: Money = payroll_totals.iter().map(|row| row.gross).sum();
        let payroll_withheld: Money = payroll_totals.iter().map(|row| row.income_tax).sum();
        let payroll_rows = payroll_totals
            .iter()
            .map(|row| {
                json!({
                    "emp_no": row.emp_no,
                    "name": row.name,
                    "payee_id": row.id_no,
                    "gross_amount": row.gross.to_string(),
                    "withheld_amount": row.income_tax.to_string(),
                    "payroll_months": row.months,
                    "source": "薪資模組",
                })
            })
            .collect::<Vec<_>>();
        let payroll_gross_difference = payroll_gross - salary_gross;
        let payroll_withheld_difference =
            salary_withheld_complete.then_some(payroll_withheld - known_salary_withheld);
        let payroll_reconciled = payroll_gross_difference.is_zero()
            && payroll_withheld_difference.is_some_and(|difference| difference.is_zero());

        let withheld_complete = missing_table_amounts == usize::default();
        let summary = format!(
            "{year} 年度扣繳憑單 · {} 位給付對象 · 所得 {} · {}",
            statement_rows.len(),
            money(gross_total),
            if withheld_complete {
                format!("扣繳 {}", money(known_withheld_total))
            } else {
                format!(
                    "已知扣繳 {}，另有 {missing_table_amounts} 筆 table 稅額缺值",
                    money(known_withheld_total)
                )
            }
        );

        Ok(OpOutcome::new(
            json!({
                "year": year,
                "from": from,
                "to": to,
                "statements": statement_rows,
                "statement_count": statement_rows.len(),
                "person_specific_count": person_specific_count,
                "gross_total": gross_total.to_string(),
                "withheld_total": withheld_complete.then(|| known_withheld_total.to_string()),
                "known_withheld_total": known_withheld_total.to_string(),
                "withheld_complete": withheld_complete,
                "missing_table_amounts": missing_table_amounts,
                "rulesets": used_rulesets.into_values().collect::<Vec<_>>(),
                "employee_income": payroll_rows,
                "payroll_reconciliation": {
                    "payroll_gross": payroll_gross.to_string(),
                    "withholding_record_salary_gross": salary_gross.to_string(),
                    "gross_difference": payroll_gross_difference.to_string(),
                    "payroll_withheld": payroll_withheld.to_string(),
                    "withholding_record_known_salary_withheld":
                        known_salary_withheld.to_string(),
                    "withheld_difference":
                        payroll_withheld_difference.map(|amount| amount.to_string()),
                    "salary_withheld_complete": salary_withheld_complete,
                    "reconciled": payroll_reconciled,
                },
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::insurance::test_employee;

    /// Recomputing premiums turns a zero bracket into a zero premium and
    /// finds no difference, so the enrolment itself is a separate item that
    /// names the person and the fields still empty.
    #[test]
    fn an_employed_person_on_pay_with_no_bracket_fails_the_check() {
        let staff = vec![
            test_employee(
                "active",
                Money::from(45000),
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
            ),
            test_employee(
                "active",
                Money::from(53000),
                Money::from(45800),
                Money::from(53000),
                Money::from(53000),
            ),
            test_employee(
                "left",
                Money::from(45000),
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
            ),
        ];

        let failures = unfiled_bracket_failures(&staff);
        assert_eq!(failures.len(), 1, "只有在職有本薪而級距是 0 的那一位");
        assert_eq!(failures[0]["missing"].as_array().unwrap().len(), 3);
        let message = failures[0]["message"].as_str().unwrap();
        assert!(message.contains("勞保投保薪資"), "要說出缺哪一項");
        assert!(
            !message.contains("名冊"),
            "去哪裡補由 check 的 screen 帶，訊息不寫路徑"
        );
    }

    /// Items the rule library does not cover must not count as passed, nor
    /// inflate checked_count. This is the structural guarantee that the
    /// compliance screen never reports "unknown" as "fine".
    #[test]
    fn missing_rules_are_unchecked_and_excluded_from_the_checked_count() {
        let checked = json!({
            "name": "已有規則的項目",
            "checked": true,
            "passed": true,
        });
        let missing = unchecked_check(
            "規則庫尚未收錄的項目",
            Vec::new(),
            Some("not_recorded"),
            "尚未收錄該規則，故未檢查。",
        );

        assert_eq!(missing["checked"], json!(false));
        assert_eq!(missing["passed"], Value::Null);
        assert_eq!(missing["required_domain"], json!("not_recorded"));
        let (checked_count, unchecked_count) = coverage_counts(&[checked, missing]);
        assert_eq!(checked_count, 1);
        assert_eq!(unchecked_count, 1);
    }
}
