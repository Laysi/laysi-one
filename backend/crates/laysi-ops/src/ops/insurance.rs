//! Labour insurance, health insurance and pension contributions.
//!
//! This module contains no rate, bracket or ceiling. The [`labor_insurance`],
//! [`health_insurance`] and [`pension`] calculators in `laysi-regulation` take
//! a `&Ruleset` resolved from a date; here we only feed employee data in and
//! shape the result. Even the numbers inside the explanatory strings shown on
//! screen are read from the payload, so they follow a rate change.
//!
//! The bracket is a fact, the ceiling is a rule:
//! `employee.labor_insured_salary` is what the company filed with the bureau,
//! and [`laysi_regulation::labor_insurance`] always applies
//! `min(filed_bracket, rule_ceiling)`.
//!
//! The employer's health insurance share exceeds twice the employee's because
//! the law apportions it by the average number of dependants across all
//! insured persons, not by this employee's actual dependants. The screen must
//! say so.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_regulation::{Contribution, Ruleset};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(GetInsuranceSummary), Box::new(ComputeInsurance)]
}

/// Render a rate as a percentage string (`0.11` becomes `11`).
///
/// Derived from the rate rather than written out as "11%", so the label
/// follows a rate change instead of contradicting the calculation.
pub(crate) fn percent(rate: Money) -> String {
    (rate * Decimal::ONE_HUNDRED).normalize().to_string()
}

/// The three contributions for one person (or one bracket) on a given date.
#[derive(Debug)]
pub(crate) struct Premiums {
    pub labor: Contribution,
    pub health: Contribution,
    pub pension: Money,
    /// Labour insurance salary actually charged, after the ceiling.
    pub labor_base: Money,
    /// The filed bracket exceeded the rule ceiling, so the ceiling was used.
    pub labor_capped: bool,
    pub labor_rs: &'static Ruleset,
    pub health_rs: &'static Ruleset,
    pub pension_rs: &'static Ruleset,
}

impl Premiums {
    /// Total employer cost for this employee (labour, health and pension).
    pub fn employer_total(&self) -> Money {
        self.labor.employer + self.health.employer + self.pension
    }

    /// Total employee deduction (the two items withheld from pay; pension is
    /// not deducted).
    pub fn employee_total(&self) -> Money {
        self.labor.employee + self.health.employee
    }
}

/// Compute the three contributions from the rules in force on a date.
///
/// Each domain resolves separately: the labour ceiling and the health
/// brackets are different tables, and an employee may be at the labour
/// ceiling while still climbing the health brackets. No applicable version
/// is an error; the system does not compute someone's premium from a guess.
pub(crate) fn premiums_on(
    as_of: NaiveDate,
    labor_insured: Money,
    health_insured: Money,
    pension_wage: Money,
) -> Result<Premiums, OpError> {
    let labor_rs = laysi_regulation::resolve("labor_insurance", as_of)?;
    let health_rs = laysi_regulation::resolve("health_insurance", as_of)?;
    let pension_rs = laysi_regulation::resolve("pension", as_of)?;

    let cap = labor_rs.decimal("max_insured_salary")?;
    Ok(Premiums {
        labor: laysi_regulation::labor_insurance(labor_rs, labor_insured)?,
        health: laysi_regulation::health_insurance(health_rs, health_insured)?,
        pension: laysi_regulation::pension(pension_rs, pension_wage)?,
        labor_base: labor_insured.min(cap),
        labor_capped: labor_insured > cap,
        labor_rs,
        health_rs,
        pension_rs,
    })
}

/// Provenance of a ruleset, always included in the response so an audit can
/// answer which version a figure was computed under.
pub(crate) fn ruleset_json(rs: &Ruleset) -> Value {
    json!({
        "domain": rs.domain,
        "version": rs.version,
        "effective_from": rs.effective_from,
        "effective_to": rs.effective_to,
        "source_note": rs.source_note,
    })
}

// Insurance and pension summary

struct GetInsuranceSummary;

#[async_trait]
impl Operation for GetInsuranceSummary {
    fn name(&self) -> &'static str {
        "get_insurance_summary"
    }
    fn risk(&self) -> Risk {
        // Insured brackets are what was filed with the authorities, and
        // accounting needs them to check the statements. Individual salary,
        // overtime and net pay are not here; those are in list_payslips,
        // which is owner only.
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀勞健保與勞退：公司負擔、員工自付、逐人投保級距與三項保費。費率、上限與平均眷口數均取自 laysi-regulation，本操作不內含任何數值。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::AsOfInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: crate::AsOfInput = parse(input)?;
        let as_of = i.as_of.unwrap_or(ctx.today);
        let staff = ctx.store.employees(false, None).await?;

        let mut rows = Vec::with_capacity(staff.len());
        let mut labor_er = Money::ZERO;
        let mut labor_ee = Money::ZERO;
        let mut health_er = Money::ZERO;
        let mut health_ee = Money::ZERO;
        let mut pension_er = Money::ZERO;
        let mut capped = 0usize;
        let mut sets: Option<(Value, Value, Value)> = None;

        for e in &staff {
            let p = premiums_on(
                as_of,
                e.labor_insured_salary,
                e.health_insured_salary,
                e.pension_wage,
            )?;
            labor_er += p.labor.employer;
            labor_ee += p.labor.employee;
            health_er += p.health.employer;
            health_ee += p.health.employee;
            pension_er += p.pension;
            if p.labor_capped {
                capped += 1;
            }
            if sets.is_none() {
                sets = Some((
                    ruleset_json(p.labor_rs),
                    ruleset_json(p.health_rs),
                    ruleset_json(p.pension_rs),
                ));
            }

            rows.push(json!({
                "emp_no": e.emp_no,
                "name": e.name,
                "dept_label": e.dept_label,
                "title": e.title,
                "status": e.status,
                "labor_insured_salary": e.labor_insured_salary.to_string(),
                "labor_charged_on": p.labor_base.to_string(),
                "labor_capped": p.labor_capped,
                "labor_employer": p.labor.employer.to_string(),
                "labor_employee": p.labor.employee.to_string(),
                "labor_government": p.labor.government.to_string(),
                "health_insured_salary": e.health_insured_salary.to_string(),
                "health_employer": p.health.employer.to_string(),
                "health_employee": p.health.employee.to_string(),
                "pension_wage": e.pension_wage.to_string(),
                "pension_employer": p.pension.to_string(),
                "employer_total": p.employer_total().to_string(),
                "employee_total": p.employee_total().to_string(),
            }));
        }

        // With no employees at all it must still answer which ruleset applies.
        let (labor_set, health_set, pension_set) = match sets {
            Some(s) => s,
            None => {
                let p = premiums_on(as_of, Money::ZERO, Money::ZERO, Money::ZERO)?;
                (ruleset_json(p.labor_rs), ruleset_json(p.health_rs), ruleset_json(p.pension_rs))
            }
        };

        let labor_rs = laysi_regulation::resolve("labor_insurance", as_of)?;
        let health_rs = laysi_regulation::resolve("health_insurance", as_of)?;
        let pension_rs = laysi_regulation::resolve("pension", as_of)?;
        let cap = labor_rs.decimal("max_insured_salary")?;
        let avg = health_rs.decimal("avg_dependents")?;
        let employer_total = labor_er + health_er + pension_er;

        // The three explanatory lines on screen. Every number in them is read
        // from the payload, so a rate change updates the wording too.
        let notes = json!([
            {
                "title": "勞保投保薪資有上限",
                "body": format!(
                    "月薪高於 {} 的一律以上限計費，目前 {} 位已達上限。普通事故保險費率 {}%，公司負擔 {}%、員工自付 {}%、政府補助 {}%。",
                    money(cap),
                    capped,
                    percent(labor_rs.decimal("premium_rate")?),
                    percent(labor_rs.decimal("employer_share")?),
                    percent(labor_rs.decimal("employee_share")?),
                    percent(labor_rs.decimal("government_share")?),
                ),
                "source": labor_rs.source_note,
                "version": labor_rs.version,
            },
            {
                "title": "健保的公司負擔會超過員工自付的兩倍",
                "body": format!(
                    "雇主依全體平均眷口數 {} 口分攤，非依個別員工實際眷屬人數，故單身員工的公司負擔同樣乘以 {}。一般保險費率 {}%。",
                    avg, avg, percent(health_rs.decimal("rate")?),
                ),
                "source": health_rs.source_note,
                "version": health_rs.version,
            },
            {
                "title": "勞退全額由雇主提繳",
                "body": format!(
                    "提繳率 {}%，全額提繳至員工個人專戶，員工不分攤，故不列入薪資單代扣欄，而屬公司成本。",
                    percent(pension_rs.decimal("employer_rate")?),
                ),
                "source": pension_rs.source_note,
                "version": pension_rs.version,
            },
        ]);

        let summary = format!(
            "讀勞健保與勞退 · {} 人 · 公司負擔 {}（勞保 {}／健保 {}／勞退 {}）· 依勞保 {}、健保 {}、勞退 {} 規則",
            rows.len(),
            money(employer_total),
            money(labor_er),
            money(health_er),
            money(pension_er),
            labor_rs.version,
            health_rs.version,
            pension_rs.version
        );

        Ok(OpOutcome::new(
            json!({
                "as_of": as_of,
                "rows": rows,
                "headcount": rows.len(),
                "capped_count": capped,
                "labor_employer": labor_er.to_string(),
                "labor_employee": labor_ee.to_string(),
                "health_employer": health_er.to_string(),
                "health_employee": health_ee.to_string(),
                "pension_employer": pension_er.to_string(),
                "employer_total": employer_total.to_string(),
                "employee_total": (labor_ee + health_ee).to_string(),
                "notes": notes,
                "rulesets": { "labor": labor_set, "health": health_set, "pension": pension_set },
                // The employer share is not in the ledger yet. State that rather
                // than letting it look like a booked expense.
                "posted_to_ledger": false,
                "posting_note": "公司負擔的勞保、健保與勞退尚未產生分錄，薪資傳票僅含應發、代扣與實發三段。入帳需另設保險費科目並開立新傳票，該項為人資與財務的第四條銜接，目前尚未接上。",
            }),
            summary,
        ))
    }
}

// Single bracket estimate

struct ComputeInsurance;

#[derive(Deserialize, JsonSchema)]
pub struct ComputeInsuranceInput {
    /// Labour insurance salary. Above the rule ceiling the ceiling is charged.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub insured_salary: Money,
    /// Health insurance salary. Defaults to the labour insurance salary.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub health_insured_salary: Option<Money>,
    /// Monthly pension contribution wage. Defaults to the health insurance
    /// salary.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub pension_wage: Option<Money>,
    /// Date whose rules apply (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub as_of: Option<NaiveDate>,
}

#[async_trait]
impl Operation for ComputeInsurance {
    fn name(&self) -> &'static str {
        "compute_insurance"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "試算一個投保級距的勞保、健保與勞退各要出多少，並回報依哪一版規則、公司與員工各分攤多少比例。換一個適用日期就換一套費率。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ComputeInsuranceInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ComputeInsuranceInput = parse(input)?;
        if i.insured_salary < Money::ZERO {
            return Err(OpError::BadInput("投保薪資不可以是負數".into()));
        }
        let as_of = i.as_of.unwrap_or(ctx.today);
        let health = i.health_insured_salary.unwrap_or(i.insured_salary);
        let pension_wage = i.pension_wage.unwrap_or(health);
        let p = premiums_on(as_of, i.insured_salary, health, pension_wage)?;

        let summary = format!(
            "試算保費 · 勞保級距 {} 健保級距 {} · 公司負擔 {}（勞保 {}／健保 {}／勞退 {}）員工自付 {} · 依勞保 {}、健保 {}、勞退 {} 規則",
            money(i.insured_salary),
            money(health),
            money(p.employer_total()),
            money(p.labor.employer),
            money(p.health.employer),
            money(p.pension),
            money(p.employee_total()),
            p.labor_rs.version,
            p.health_rs.version,
            p.pension_rs.version
        );

        Ok(OpOutcome::new(
            json!({
                "as_of": as_of,
                "insured_salary": i.insured_salary.to_string(),
                "labor_charged_on": p.labor_base.to_string(),
                "labor_capped": p.labor_capped,
                "labor": {
                    "employer": p.labor.employer.to_string(),
                    "employee": p.labor.employee.to_string(),
                    "government": p.labor.government.to_string(),
                    "rate_percent": percent(p.labor_rs.decimal("premium_rate")?),
                    "max_insured_salary": p.labor_rs.decimal("max_insured_salary")?.to_string(),
                    "ruleset": ruleset_json(p.labor_rs),
                },
                "health": {
                    "insured_salary": health.to_string(),
                    "employer": p.health.employer.to_string(),
                    "employee": p.health.employee.to_string(),
                    "government": p.health.government.to_string(),
                    "rate_percent": percent(p.health_rs.decimal("rate")?),
                    "avg_dependents": p.health_rs.decimal("avg_dependents")?.to_string(),
                    "ruleset": ruleset_json(p.health_rs),
                },
                "pension": {
                    "contribution_wage": pension_wage.to_string(),
                    "employer": p.pension.to_string(),
                    "rate_percent": percent(p.pension_rs.decimal("employer_rate")?),
                    "ruleset": ruleset_json(p.pension_rs),
                },
                "employer_total": p.employer_total().to_string(),
                "employee_total": p.employee_total().to_string(),
            }),
            summary,
        ))
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

    /// The five regulation reference values. A mismatch means this module has
    /// wired the rules up incorrectly.
    ///
    /// Test conditions: monthly salary 53,000, labour bracket 45,800, health
    /// bracket and pension wage both 53,000.
    #[test]
    fn the_five_reference_benchmarks_come_out_of_the_ruleset() {
        let p = premiums_on(d(2026, 7, 26), n("45800"), n("53000"), n("53000")).unwrap();
        assert_eq!(p.labor.employer, n("3527"), "公司負擔勞保");
        assert_eq!(p.labor.employee, n("1008"), "員工自付勞保");
        assert_eq!(p.health.employer, n("2647"), "公司負擔健保（含平均眷口 1.61）");
        assert_eq!(p.health.employee, n("822"), "員工自付健保");
        assert_eq!(p.pension, n("3180"), "公司提繳勞退");

        assert_eq!(p.employer_total(), n("9354"), "3527 + 2647 + 3180");
        assert_eq!(p.employee_total(), n("1830"), "1008 + 822");
    }

    /// Above the ceiling, labour insurance still charges the ceiling, and the
    /// ceiling comes from the rules rather than from here.
    #[test]
    fn a_salary_above_the_cap_is_charged_at_the_cap() {
        let capped = premiums_on(d(2026, 7, 26), n("120000"), n("53000"), n("53000")).unwrap();
        let at_cap = premiums_on(d(2026, 7, 26), n("45800"), n("53000"), n("53000")).unwrap();
        assert_eq!(capped.labor, at_cap.labor);
        assert!(capped.labor_capped, "要標出來是按上限收的");
        assert!(!at_cap.labor_capped, "剛好等於上限不算「超過」");
        assert_eq!(capped.labor_base, n("45800"));
    }

    /// Rule substitutability, the reason the regulation engine exists, and
    /// something the HR module has to preserve.
    ///
    /// A ruleset with different rates gives the same employee different
    /// premiums without a single change to this file, while 2026 results stay
    /// exactly as they were: months already filed do not move.
    #[test]
    fn a_different_ruleset_gives_the_same_employee_a_different_premium() {
        let base = laysi_regulation::resolve("labor_insurance", d(2026, 7, 26)).unwrap();
        let before = laysi_regulation::labor_insurance(base, n("45800")).unwrap();
        assert_eq!(before.employer, n("3527"));

        // Simulate 2027: rate raised to 12%, ceiling raised to 48,200.
        let mut next = base.clone();
        next.version = "2027.1".into();
        next.effective_from = d(2027, 1, 1);
        next.payload["premium_rate"] = json!("0.12");
        next.payload["max_insured_salary"] = json!("48200");

        let after = laysi_regulation::labor_insurance(&next, n("48200")).unwrap();
        // 48,200 × 0.12 × 0.70 = 4,048.8 → 4,049
        assert_eq!(after.employer, n("4049"));
        assert_ne!(after.employer, before.employer, "換規則就要換答案");

        // The same employee under the old rules is unchanged.
        assert_eq!(
            laysi_regulation::labor_insurance(base, n("45800")).unwrap().employer,
            n("3527")
        );

        // Health insurance and pension behave the same way: change the payload,
        // change the answer.
        let hbase = laysi_regulation::resolve("health_insurance", d(2026, 7, 26)).unwrap();
        let mut hnext = hbase.clone();
        hnext.payload["rate"] = json!("0.055");
        let h = laysi_regulation::health_insurance(&hnext, n("53000")).unwrap();
        // 53,000 × 0.055 × 0.60 × 1.61 = 2,815.89 → 2,816
        assert_eq!(h.employer, n("2816"));
        assert_ne!(h.employer, n("2647"));

        let pbase = laysi_regulation::resolve("pension", d(2026, 7, 26)).unwrap();
        let mut pnext = pbase.clone();
        pnext.payload["employer_rate"] = json!("0.08");
        assert_eq!(laysi_regulation::pension(&pnext, n("53000")).unwrap(), n("4240"));
    }

    /// The percentage label is derived, not written out separately.
    #[test]
    fn the_percent_label_is_derived_from_the_rate() {
        assert_eq!(percent(n("0.11")), "11");
        assert_eq!(percent(n("0.0517")), "5.17");
        assert_eq!(percent(n("0.06")), "6");
        assert_eq!(percent(n("0.70")), "70");
    }

    /// No applicable version is an error; premiums are never computed from a
    /// guessed rate.
    #[test]
    fn a_date_with_no_ruleset_is_refused_never_guessed() {
        let err = premiums_on(d(2025, 12, 31), n("45800"), n("53000"), n("53000")).unwrap_err();
        assert!(matches!(err, OpError::Regulation(_)));
        assert!(err.to_string().contains("不會推測"));
    }
}
