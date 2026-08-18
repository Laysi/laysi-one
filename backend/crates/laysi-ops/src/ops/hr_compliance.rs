//! HR compliance checks and annual withholding statements.
//!
//! [`GetHrCompliance`] splits every item into `checked` and `passed`. Minimum
//! wage, the labour insurance ceiling, premium recomputation and leave
//! entitlement have usable rules, so they are checked per employee with the
//! ruleset attached. The full insured-salary brackets and overtime multipliers
//! do not, so they return `checked = false` with a `not_checked_reason`.
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

use crate::ops::employees::service_months;
use crate::ops::insurance::{premiums_on, ruleset_json};
use crate::ops::tax::{code_rule, withhold};
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
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
        "讀人資法遵檢核表：逐人檢查最低工資、勞保計費上限、保費試算與特休餘額。完整投保級距與加班費倍數的規則不足時，明確標示為未檢查，不視為通過。"
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
        let premium_rules = premiums_on(as_of, Money::ZERO, Money::ZERO, Money::ZERO)?;

        let mut minimum_failures = Vec::new();
        for employee in &staff {
            if let Some(required) =
                laysi_regulation::minimum_wage_shortfall(minimum_rs, employee.base_salary)?
            {
                minimum_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "actual": employee.base_salary.to_string(),
                    "required": required.to_string(),
                    "difference": (required - employee.base_salary).to_string(),
                    "message": format!(
                        "月薪 {}，比規則下限少 {}",
                        money(employee.base_salary),
                        money(required - employee.base_salary)
                    ),
                }));
            }
        }
        let minimum_failure_count = minimum_failures.len();

        let mut labor_cap_failures = Vec::new();
        let mut labor_employer = Money::ZERO;
        let mut labor_employee = Money::ZERO;
        let mut health_employer = Money::ZERO;
        let mut health_employee = Money::ZERO;
        let mut pension_employer = Money::ZERO;
        for employee in &staff {
            let premiums = premiums_on(
                as_of,
                employee.labor_insured_salary,
                employee.health_insured_salary,
                employee.pension_wage,
            )?;
            labor_employer += premiums.labor.employer;
            labor_employee += premiums.labor.employee;
            health_employer += premiums.health.employer;
            health_employee += premiums.health.employee;
            pension_employer += premiums.pension;

            if premiums.labor_capped {
                labor_cap_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "reported": employee.labor_insured_salary.to_string(),
                    "charged_on": premiums.labor_base.to_string(),
                    "difference": (employee.labor_insured_salary - premiums.labor_base).to_string(),
                    "message": format!(
                        "申報勞保投保薪資 {}，規則計費只採 {}，請確認申報級距",
                        money(employee.labor_insured_salary),
                        money(premiums.labor_base)
                    ),
                }));
            }
        }
        let labor_cap_failure_count = labor_cap_failures.len();

        let taken = ctx.store.annual_leave_taken(as_of.year()).await?;
        let taken_by_employee: BTreeMap<i32, Money> = taken.into_iter().collect();
        let mut annual_failures = Vec::new();
        for employee in &staff {
            let entitlement = Money::from(laysi_regulation::annual_leave_days(
                annual_rs,
                service_months(employee.hired_on, as_of),
            )?);
            let used = taken_by_employee
                .get(&employee.id)
                .copied()
                .unwrap_or(Money::ZERO);
            if used > entitlement {
                annual_failures.push(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "entitlement": entitlement.to_string(),
                    "taken": used.to_string(),
                    "difference": (used - entitlement).to_string(),
                    "message": format!(
                        "已核准特休 {} 日，比規則額度多 {} 日，請確認是否為公司加給或資料有誤",
                        used.normalize(),
                        (used - entitlement).normalize()
                    ),
                }));
            }
        }
        let annual_failure_count = annual_failures.len();

        let mut checks = vec![
            json!({
                "name": "最低工資",
                "checked": true,
                "passed": minimum_failures.is_empty(),
                "rulesets": [ruleset_json(minimum_rs)],
                "failures": minimum_failures,
                "scope_note": "逐人比對員工主檔的月薪與規則下限。中途到職當月的實發金額不列入比較。",
            }),
            json!({
                "name": "勞保申報值與計費上限",
                "checked": true,
                "passed": labor_cap_failures.is_empty(),
                "rulesets": [ruleset_json(premium_rules.labor_rs)],
                "failures": labor_cap_failures,
                "scope_note": "只檢查申報的投保薪資有沒有超過計費上限。投保薪資分級表本身尚未收錄，另列為未檢查。",
            }),
            json!({
                "name": "勞健保與勞退費額可依規則重算",
                "checked": true,
                "passed": true,
                "rulesets": [
                    ruleset_json(premium_rules.labor_rs),
                    ruleset_json(premium_rules.health_rs),
                    ruleset_json(premium_rules.pension_rs),
                ],
                "failures": [],
                "evidence": {
                    "employee_count": staff.len(),
                    "labor_employer": labor_employer.to_string(),
                    "labor_employee": labor_employee.to_string(),
                    "health_employer": health_employer.to_string(),
                    "health_employee": health_employee.to_string(),
                    "pension_employer": pension_employer.to_string(),
                },
                "scope_note": "逐人以現行費率重算保費。這代表目前的申報值都算得出金額，不代表投保級距本身已核對過。",
            }),
            json!({
                "name": "特休額度與已核准天數",
                "checked": true,
                "passed": annual_failures.is_empty(),
                "rulesets": [ruleset_json(annual_rs)],
                "failures": annual_failures,
                "scope_note": "依年資現算特休額度，再與本年度已核准的天數比較。超出額度只列為待確認，可能是公司加給。",
            }),
        ];
        let unchecked = vec![
            unchecked_check(
                "完整勞健保投保級距與勞退提繳工資級距",
                vec![
                    ruleset_json(premium_rules.labor_rs),
                    ruleset_json(premium_rules.health_rs),
                    ruleset_json(premium_rules.pension_rs),
                ],
                None,
                "本項未檢查：規則庫目前僅收錄費率、分攤方式與勞保計費上限，尚無完整的勞健保投保薪資分級表與勞退提繳工資級距。",
            ),
            unchecked_check(
                "加班費倍數",
                Vec::new(),
                Some("overtime"),
                "本項未檢查。出勤資料的加班費由來源差勤系統帶入，本系統不依時數自行重算。",
            ),
        ];
        checks.extend(unchecked);
        let (checked_count, unchecked_count) = coverage_counts(&checks);
        let coverage_complete = unchecked_count == usize::default();

        let failure_count = minimum_failure_count + labor_cap_failure_count + annual_failure_count;
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
                "coverage_complete": coverage_complete,
                "checked_count": checked_count,
                "unchecked_count": unchecked_count,
                "failure_count": failure_count,
                "checks": checks,
                "available_domains": laysi_regulation::DOMAINS,
                "coverage_note": "「無差異」的範圍是已檢查的項目；未檢查的項目列在下方。",
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
        let mut table_gross = Money::ZERO;
        let mut known_table_withheld = Money::ZERO;
        let mut table_withheld_complete = true;

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
                        "{} 的扣繳基礎「{other}」非 rate 或 table，系統不代為推定算法",
                        record.payee
                    )));
                }
            };

            if record.basis == "table" && withheld.is_none() {
                missing_table_amounts += 1;
            }
            if record.basis == "table" {
                table_gross += record.gross_amount;
                match withheld {
                    Some(amount) => known_table_withheld += amount,
                    None => table_withheld_complete = false,
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
                    "source_note": if person_specific {
                        "來源有給付對象的識別資料，可作為個人憑單。"
                    } else {
                        "來源沒有識別資料，這筆可能是多人合併，不能直接當成個人憑單。"
                    },
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
        let payroll_gross_difference = payroll_gross - table_gross;
        let payroll_withheld_difference =
            table_withheld_complete.then_some(payroll_withheld - known_table_withheld);
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
                    "withholding_record_table_gross": table_gross.to_string(),
                    "gross_difference": payroll_gross_difference.to_string(),
                    "payroll_withheld": payroll_withheld.to_string(),
                    "withholding_record_known_table_withheld":
                        known_table_withheld.to_string(),
                    "withheld_difference":
                        payroll_withheld_difference.map(|amount| amount.to_string()),
                    "table_withheld_complete": table_withheld_complete,
                    "reconciled": payroll_reconciled,
                    "note": "薪資模組與扣繳紀錄分開計算，這裡只作對帳，差異照實列出。",
                },
                "table_note": "依扣繳稅額表的項目，稅額採用來源系統帶入的金額；來源沒有帶就留空。",
                "source_note": "是否認得到個人，以來源紀錄帶的統一編號或身分證號為準。來源只給合併總額時照實保留。",
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
