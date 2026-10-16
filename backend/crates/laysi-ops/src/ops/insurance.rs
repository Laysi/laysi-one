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
//! `min(filed_bracket, rule_ceiling)`. A bracket of zero is a bracket nobody
//! filed rather than a premium of zero, so [`missing_brackets`] separates the
//! two and the summary carries the count.
//!
//! The two health insurance shares count dependants differently. The employee
//! pays one head for themselves and one per covered dependant, up to the cap in
//! `health_dependents`; the employer share is apportioned by the average number
//! of dependants across all insured persons, whoever this employee covers. A
//! single employee's employer share therefore exceeds twice their own, and the
//! screen says how both are counted.
//!
//! The labour bracket is the one salary the company files, and the bureau
//! grades it twice: labour insurance, employment insurance and the arrears
//! wage payment fund up to the labour ceiling, occupational accident insurance
//! up to its own. Employment insurance is billed and rounded apart from the
//! ordinary risk premium. Occupational accident insurance adds the commuting
//! rate to the industry rate the company keeps in its settings, and is not
//! computed while that is blank. The arrears fund is applied once to a month's
//! total, so it is a company figure: [`wage_arrears_on`] takes what each
//! person adds to it, and no row carries a share.

use crate::ops::attendance::month_span;
#[cfg(test)]
use crate::ops::employees::BASIS_MONTHLY;
use crate::ops::employees::{insured_as_label, INSURED_EMPLOYEE, INSURED_OWNER};
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_regulation::{Contribution, MonthPart, Ruleset};
use laysi_store::Employee;
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

/// The company setting holding the occupational accident industry rate the
/// bureau assigned, as a percentage. Blank until the administrator enters it.
pub(crate) const ACCIDENT_RATE_SETTING: &str = "accident_industry_rate";

/// The industry rate in [`ACCIDENT_RATE_SETTING`] as a fraction, or `None`
/// while nobody has entered it.
pub(crate) fn parse_accident_rate(value: &str) -> Result<Option<Money>, OpError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    match value.parse::<Money>() {
        Ok(percent) if percent > Money::ZERO && percent < Money::ONE_HUNDRED => {
            Ok(Some(percent / Money::ONE_HUNDRED))
        }
        _ => Err(OpError::BadInput(format!(
            "職災保險行業別災害費率填大於 0、小於 100 的百分比，例如 0.07，收到「{value}」"
        ))),
    }
}

/// What the settings screen prints under [`ACCIDENT_RATE_SETTING`]: the rate
/// the bill states, which adds the commuting rate in force on `on`.
pub(crate) fn accident_rate_note(on: NaiveDate, stored: &str) -> Result<String, OpError> {
    let rs = laysi_regulation::resolve("occupational_accident", on)?;
    let commuting = rs.decimal("commuting_rate")?;
    Ok(match parse_accident_rate(stored)? {
        Some(industry) => format!(
            "加上下班災害費率 {}%，職災保險費率合計 {}%。",
            percent(commuting),
            percent(industry + commuting)
        ),
        None => format!(
            "填勞保局核定的費率，另加上下班災害費率 {}%；未填時薪資不計職災保險費。",
            percent(commuting)
        ),
    })
}

/// The company's occupational accident industry rate, as a fraction.
pub(crate) async fn accident_rate(ctx: &OpCtx) -> Result<Option<Money>, OpError> {
    let stored = ctx
        .store
        .company_setting(ACCIDENT_RATE_SETTING)
        .await?
        .unwrap_or_default();
    parse_accident_rate(&stored)
}

/// The contributions for one person (or one bracket) on a given date.
#[derive(Debug)]
pub(crate) struct Premiums {
    pub labor: Contribution,
    /// Zero for someone not in employment insurance.
    pub employment: Contribution,
    /// Borne by the company in full. `None` while the company has not entered
    /// its industry rate.
    pub accident: Option<Money>,
    pub health: Contribution,
    pub pension: Money,
    /// Labour insurance salary actually charged, after the ceiling. Employment
    /// insurance and the arrears fund are charged on the same figure.
    pub labor_base: Money,
    /// The filed bracket exceeded the rule ceiling, so the ceiling was used.
    pub labor_capped: bool,
    /// Occupational accident insured salary, after that insurance's ceiling.
    pub accident_base: Money,
    /// Insured as an employee and in employment insurance on the roster.
    pub employment_insured: bool,
    /// What the person adds to the arrears wage payment fund: the labour
    /// salary after the ceiling and the part of the month it counts for.
    /// `None` for an owner, for whom nothing is contributed.
    pub arrears: Option<(Money, MonthPart)>,
    pub labor_rs: &'static Ruleset,
    pub employment_rs: &'static Ruleset,
    pub accident_rs: &'static Ruleset,
    pub health_rs: &'static Ruleset,
    pub pension_rs: &'static Ruleset,
    /// The owner shares that priced health and pension, when the person is
    /// insured as the employer. `None` for everybody else.
    pub owner_rs: Option<&'static Ruleset>,
    /// Covered dependants the health deduction was charged for, after the
    /// cap and not counting the person themselves.
    pub health_dependants: u32,
    pub dependants_rs: &'static Ruleset,
}

impl Premiums {
    /// Total employer cost for this person: labour, employment, occupational
    /// accident and health insurance, and pension. The arrears fund is a
    /// company figure and is not in it.
    pub fn employer_total(&self) -> Money {
        self.labor.employer
            + self.employment.employer
            + self.accident.unwrap_or(Money::ZERO)
            + self.health.employer
            + self.pension
    }

    /// Total employee deduction: the three items withheld from pay. Pension
    /// is not deducted.
    pub fn employee_total(&self) -> Money {
        self.labor.employee + self.employment.employee + self.health.employee
    }
}

/// The filed facts one person is priced on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Enrolment<'a> {
    /// `employee` or `owner`; picks the shares.
    pub insured_as: &'a str,
    /// In employment insurance on the roster. Not read for an owner.
    pub employment_insured: bool,
    pub labor_insured: Money,
    pub health_insured: Money,
    pub pension_wage: Money,
    /// Dependants covered under the person's health insurance on the day
    /// priced ([`covered_dependants`]).
    pub covered_dependants: u32,
}

impl<'a> Enrolment<'a> {
    /// What the roster holds for `e`, with the dependants covered on `on`.
    pub fn of(e: &'a Employee, on: NaiveDate) -> Enrolment<'a> {
        Enrolment {
            insured_as: &e.insured_as,
            employment_insured: e.employment_insured,
            labor_insured: e.labor_insured_salary,
            health_insured: e.health_insured_salary,
            pension_wage: e.pension_wage,
            covered_dependants: covered_dependants(e, on),
        }
    }
}

/// The arrears wage payment fund on `as_of`'s rules for everyone in
/// `arrears`, each entry what one person adds to it ([`Premiums::arrears`]).
pub(crate) fn wage_arrears_on(
    as_of: NaiveDate,
    arrears: impl IntoIterator<Item = Option<(Money, MonthPart)>>,
) -> Result<(Money, &'static Ruleset), OpError> {
    let rs = laysi_regulation::resolve("wage_arrears", as_of)?;
    let fund = laysi_regulation::wage_arrears(rs, arrears.into_iter().flatten())?;
    Ok((fund, rs))
}

/// The arrears fund one run of a period records. The fund is the month's,
/// applied once to its whole total, while a period is paid in more than one
/// run once somebody is paid late. So a run records what the total comes to
/// with `this_run` added to the `earlier` runs' payslips, less what those runs
/// already `recorded`, and the runs of a period add up to one computation.
/// Never below zero: a run has no way to hand back what an earlier one
/// recorded.
pub(crate) fn period_arrears_on(
    as_of: NaiveDate,
    earlier: impl IntoIterator<Item = Option<(Money, MonthPart)>>,
    recorded: Money,
    this_run: impl IntoIterator<Item = Option<(Money, MonthPart)>>,
) -> Result<Money, OpError> {
    let (fund, _) = wage_arrears_on(as_of, earlier.into_iter().chain(this_run))?;
    Ok((fund - recorded).max(Money::ZERO))
}

/// What a stored payslip of the month from `month_start` to `month_end` adds
/// to the arrears fund, as [`Premiums::arrears`] puts it: the labour salary
/// after the ceiling and the days insured. `None` for an owner.
///
/// A payslip of a run from before the fund was recorded counts as well: its
/// wages are part of the month's total, and its run recorded zero, so the
/// next run of the period records the fund for it.
pub(crate) fn slip_arrears(
    s: &laysi_store::Payslip,
    month_start: NaiveDate,
    month_end: NaiveDate,
) -> Result<Option<(Money, MonthPart)>, OpError> {
    if s.insured_as == INSURED_OWNER {
        return Ok(None);
    }
    let cap =
        laysi_regulation::resolve("labor_insurance", month_end)?.decimal("max_insured_salary")?;
    let part = match s.insured_span {
        Some((from, to)) => Coverage::of_month(month_start, month_end, from, to)?.labor,
        None => MonthPart::WHOLE,
    };
    Ok(Some((s.labor_insured_salary.min(cap), part)))
}

/// How much of a month a person is charged for. Labour insurance and pension
/// go by the day in the month of enrolment or withdrawal; health insurance is
/// the whole month or none of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Coverage {
    pub labor: MonthPart,
    pub pension: MonthPart,
    /// Insured on the month's last day. Health insurance charges the whole
    /// month in the month of enrolment and nothing in the month of withdrawal,
    /// which is the same as charging whoever is insured when the month ends.
    pub health: bool,
}

impl Coverage {
    /// Insured from before the month began until after it ended.
    pub const WHOLE: Coverage = Coverage {
        labor: MonthPart::WHOLE,
        pension: MonthPart::WHOLE,
        health: true,
    };

    /// Coverage for the month from `month_start` to `month_end` of someone
    /// insured here from `from` to `to`, both counted. The day counts come
    /// from `partial_month`, resolved on the month's last day like the rates.
    pub fn of_month(
        month_start: NaiveDate,
        month_end: NaiveDate,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Coverage, OpError> {
        let rs = laysi_regulation::resolve("partial_month", month_end)?;
        let part = |key| laysi_regulation::month_part(rs, key, month_start, month_end, from, to);
        Ok(Coverage {
            labor: part("labor_days_per_month")?,
            pension: part("pension_days_per_month")?,
            health: from <= month_end && to >= month_end,
        })
    }

    /// Insured here on no day of the month: hired after it, or gone before
    /// it began. Not a month of joining or leaving.
    pub fn is_none(self) -> bool {
        self.labor.days() == Some(0)
    }

    /// Coverage for a month of an employee enrolled on the day they were hired
    /// and withdrawn on the day they left, as the law requires.
    pub fn of_employee(
        e: &Employee,
        month_start: NaiveDate,
        month_end: NaiveDate,
    ) -> Result<Coverage, OpError> {
        let left = e.left_on.map_or(month_end, |left| left.min(month_end));
        Coverage::of_month(month_start, month_end, e.hired_on, left)
    }
}

/// Compute one person's contributions from the rules in force on a date.
///
/// Each domain resolves separately: the labour ceiling and the health
/// brackets are different tables, and an employee may be at the labour
/// ceiling while still climbing the health brackets. Every domain resolves
/// whether or not it charges this person anything, and no applicable version
/// is an error; the system does not compute someone's premium from a guess.
///
/// `who.insured_as` picks which set of shares applies. Someone insured as the
/// company's own owner bears the health premium in full, has no pension
/// contributed for them, is not in employment insurance and adds nothing to
/// the arrears fund; the first two figures come from the `owner_insurance`
/// ruleset. Labour and occupational accident insurance split the same way
/// for both.
///
/// `who.covered_dependants` is how many of the person's dependants were
/// covered on `as_of`, which every caller that charges a month passes as that
/// month's last day; the cap on how many are charged comes from
/// `health_dependents`.
///
/// `cover` is how much of the month is charged ([`Coverage`]). A month with
/// no health insurance charges no dependants either: they are insured through
/// the person. Employment and occupational accident insurance and the arrears
/// fund are charged for the days labour insurance is.
///
/// `accident_rate` is the company's industry rate as a fraction
/// ([`accident_rate`]); `None` leaves occupational accident insurance out.
pub(crate) fn premiums_on(
    as_of: NaiveDate,
    who: &Enrolment,
    cover: Coverage,
    accident_rate: Option<Money>,
) -> Result<Premiums, OpError> {
    let labor_rs = laysi_regulation::resolve("labor_insurance", as_of)?;
    let employment_rs = laysi_regulation::resolve("employment_insurance", as_of)?;
    let accident_rs = laysi_regulation::resolve("occupational_accident", as_of)?;
    let health_rs = laysi_regulation::resolve("health_insurance", as_of)?;
    let pension_rs = laysi_regulation::resolve("pension", as_of)?;
    let dependants_rs = laysi_regulation::resolve("health_dependents", as_of)?;
    let owner = who.insured_as == INSURED_OWNER;
    let owner_rs = match owner {
        true => Some(laysi_regulation::resolve("owner_insurance", as_of)?),
        false => None,
    };

    let cap = labor_rs.decimal("max_insured_salary")?;
    let labor_base = who.labor_insured.min(cap);
    let employment_insured = !owner && who.employment_insured;
    let covered_dependants = match cover.health {
        true => who.covered_dependants,
        false => 0,
    };
    let health = match owner_rs {
        Some(owner) => laysi_regulation::health_insurance_owner(
            health_rs,
            owner,
            dependants_rs,
            who.health_insured,
            covered_dependants,
        )?,
        None => laysi_regulation::health_insurance(
            health_rs,
            dependants_rs,
            who.health_insured,
            covered_dependants,
        )?,
    };
    let employment =
        laysi_regulation::employment_insurance(employment_rs, labor_base, cover.labor)?;
    Ok(Premiums {
        labor: laysi_regulation::labor_insurance(labor_rs, who.labor_insured, cover.labor)?,
        employment: match employment_insured {
            true => employment,
            false => Contribution::ZERO,
        },
        accident: accident_rate
            .map(|rate| {
                laysi_regulation::occupational_accident(
                    accident_rs,
                    rate,
                    who.labor_insured,
                    cover.labor,
                )
            })
            .transpose()?,
        health: match cover.health {
            true => health,
            false => Contribution::ZERO,
        },
        pension: match owner_rs {
            Some(owner) => laysi_regulation::pension_owner(owner, who.pension_wage, cover.pension)?,
            None => laysi_regulation::pension(pension_rs, who.pension_wage, cover.pension)?,
        },
        labor_base,
        labor_capped: who.labor_insured > cap,
        accident_base: who
            .labor_insured
            .min(accident_rs.decimal("max_insured_salary")?),
        employment_insured,
        arrears: (!owner).then_some((labor_base, cover.labor)),
        labor_rs,
        employment_rs,
        accident_rs,
        health_rs,
        pension_rs,
        owner_rs,
        health_dependants: laysi_regulation::dependants_charged(dependants_rs, covered_dependants)?,
        dependants_rs,
    })
}

/// How many of an employee's dependants were covered under their health
/// insurance on a date. The premium is charged by the month to whoever is
/// covered on its last day, so callers pass the period end; the last day
/// covered counts as covered.
///
/// A dependant is covered here only through the employee, so nobody counts
/// on a day the employee is not employed: someone who left mid-month takes
/// their dependants with them, whatever dates the list still holds.
pub(crate) fn covered_dependants(e: &Employee, on: NaiveDate) -> u32 {
    if e.hired_on > on || e.left_on.is_some_and(|left| left < on) {
        return 0;
    }
    let covered = e
        .dependents
        .iter()
        .filter(|d| {
            d.health_enrolled_from <= on && d.health_enrolled_until.is_none_or(|until| until >= on)
        })
        .count();
    u32::try_from(covered).unwrap_or(u32::MAX)
}

/// Which of the three filed brackets one person has no figure for. The three
/// are judged separately because only one of them may be missing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MissingBrackets {
    pub labor: bool,
    pub health: bool,
    pub pension: bool,
}

impl MissingBrackets {
    pub fn any(self) -> bool {
        self.labor || self.health || self.pension
    }

    /// The missing brackets under the names the screens print, in the order
    /// the roster lists them.
    pub fn labels(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.labor {
            out.push("勞保投保薪資");
        }
        if self.health {
            out.push("健保投保金額");
        }
        if self.pension {
            out.push("勞退提繳工資");
        }
        out
    }
}

/// Report the brackets an employed person has no filed figure for.
///
/// Zero is "not set", not "no premium": someone still working and drawing a
/// salary has to be enrolled, so a zero bracket is an enrolment that was
/// never filed, and a premium of zero computed from it is not a fact about
/// the person. Two cases are therefore excluded: whoever has left is no
/// longer enrolled here, and a base salary of zero means the pay is not
/// agreed yet (0028), so there is nothing to file. `leaving` is employed
/// until the leaving date and is judged like `active`.
///
/// An owner is judged on health insurance alone. Labour insurance is not
/// compulsory for them (Labour Insurance Act art. 8 makes it a choice for an
/// employer who actually works), and no pension is contributed for them at
/// all, so a blank in either is a fact about the enrolment rather than a
/// filing nobody made.
///
/// This says nothing about what the bracket should be. The insured salary
/// bracket table is not in the rule library, and a guessed figure would be
/// worse than none.
pub(crate) fn missing_brackets(e: &Employee) -> MissingBrackets {
    if e.status == "left" || e.base_salary <= Money::ZERO {
        return MissingBrackets::default();
    }
    let owner = e.insured_as == INSURED_OWNER;
    MissingBrackets {
        labor: !owner && e.labor_insured_salary.is_zero(),
        health: e.health_insured_salary.is_zero(),
        pension: !owner && e.pension_wage.is_zero(),
    }
}

/// Fixture shared with the compliance tests, which judge the same rule from
/// the other screen.
#[cfg(test)]
pub(crate) fn test_employee(
    status: &str,
    base_salary: Money,
    labor_insured_salary: Money,
    health_insured_salary: Money,
    pension_wage: Money,
) -> Employee {
    Employee {
        id: 1,
        emp_no: "E001".into(),
        name: "測試員工".into(),
        dept_id: None,
        dept_code: None,
        dept_label: None,
        title: "測試職稱".into(),
        employment_type: "full_time".into(),
        pay_basis: BASIS_MONTHLY.into(),
        insured_as: INSURED_EMPLOYEE.into(),
        employment_insured: true,
        birthday: None,
        hired_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        left_on: None,
        status: status.into(),
        has_login: false,
        base_salary,
        labor_insured_salary,
        health_insured_salary,
        pension_wage,
        id_no: String::new(),
        bank_accounts: Vec::new(),
        dependents: Vec::new(),
        note: String::new(),
    }
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
        "讀勞保、就保、職災、健保、勞退與墊償基金：公司負擔、員工自付、逐人投保級距、健保眷口數與各項保費。費率與在保眷屬以指定日期所在月份的月底計，與該月薪資單相同；墊償基金依全公司投保薪資合計計算，不分攤到個人。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::AsOfInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: crate::AsOfInput = parse(input)?;
        // The whole month, judged on its last day as run_payroll judges the
        // period: a dependant enrolled later in the month is charged on that
        // month's payslip, and so is someone hired or leaving in it by the day,
        // so each row has to count them already.
        let day = i.as_of.unwrap_or(ctx.today);
        let (month_start, as_of) = month_span(&day.format("%Y-%m").to_string());
        // Whoever left during the month is still charged for its first days,
        // as the payslip charges them, whether or not the roster has marked
        // them as left yet; whoever left before it is not on the page.
        let staff: Vec<Employee> = ctx
            .store
            .employees(true, None)
            .await?
            .into_iter()
            .filter(|e| e.status != "left" || e.left_on.is_some_and(|left| left >= month_start))
            .collect();

        let accident_rate = accident_rate(ctx).await?;

        let mut rows = Vec::with_capacity(staff.len());
        let mut labor_er = Money::ZERO;
        let mut labor_ee = Money::ZERO;
        let mut employment_er = Money::ZERO;
        let mut employment_ee = Money::ZERO;
        let mut accident_er = Money::ZERO;
        let mut health_er = Money::ZERO;
        let mut health_ee = Money::ZERO;
        let mut pension_er = Money::ZERO;
        let mut capped = 0usize;
        let mut accident_capped = 0usize;
        let mut not_employment_insured = 0usize;
        let mut unfiled = Vec::new();
        let mut arrears = Vec::with_capacity(staff.len());
        let mut owner_set: Option<Value> = None;
        let mut partial = 0usize;

        for e in &staff {
            let missing = missing_brackets(e);
            let covered = covered_dependants(e, as_of);
            let cover = Coverage::of_employee(e, month_start, as_of)?;
            if cover != Coverage::WHOLE && !cover.is_none() {
                partial += 1;
            }
            let p = premiums_on(as_of, &Enrolment::of(e, as_of), cover, accident_rate)?;
            if owner_set.is_none() {
                owner_set = p.owner_rs.map(ruleset_json);
            }
            labor_er += p.labor.employer;
            labor_ee += p.labor.employee;
            employment_er += p.employment.employer;
            employment_ee += p.employment.employee;
            accident_er += p.accident.unwrap_or(Money::ZERO);
            health_er += p.health.employer;
            health_ee += p.health.employee;
            pension_er += p.pension;
            arrears.push(p.arrears);
            if p.labor_capped {
                capped += 1;
            }
            let over_accident_cap = e.labor_insured_salary > p.accident_base;
            if over_accident_cap {
                accident_capped += 1;
            }
            if e.insured_as != INSURED_OWNER && !p.employment_insured {
                not_employment_insured += 1;
            }
            if missing.any() {
                unfiled.push(json!({
                    "emp_no": e.emp_no,
                    "name": e.name,
                    "missing": missing.labels(),
                }));
            }

            rows.push(json!({
                "emp_no": e.emp_no,
                "name": e.name,
                "dept_label": e.dept_label,
                "title": e.title,
                "status": e.status,
                "insured_as": e.insured_as,
                "insured_as_label": insured_as_label(&e.insured_as),
                "labor_insured_salary": e.labor_insured_salary.to_string(),
                "labor_charged_on": p.labor_base.to_string(),
                "labor_capped": p.labor_capped,
                "labor_bracket_missing": missing.labor,
                "health_bracket_missing": missing.health,
                "pension_wage_missing": missing.pension,
                "brackets_missing": missing.any(),
                "labor_employer": p.labor.employer.to_string(),
                "labor_employee": p.labor.employee.to_string(),
                "labor_government": p.labor.government.to_string(),
                "employment_insured": p.employment_insured,
                "employment_employer": p.employment.employer.to_string(),
                "employment_employee": p.employment.employee.to_string(),
                // Null while the company has no industry rate.
                "accident_employer": p.accident.map(|a| a.to_string()),
                "accident_charged_on": p.accident_base.to_string(),
                "accident_capped": over_accident_cap,
                "health_insured_salary": e.health_insured_salary.to_string(),
                // Charged after the cap, and covered on file, so the screen can
                // say when the two differ.
                "health_dependents": p.health_dependants,
                "health_dependents_covered": covered,
                "health_employer": p.health.employer.to_string(),
                "health_employee": p.health.employee.to_string(),
                "pension_wage": e.pension_wage.to_string(),
                "pension_employer": p.pension.to_string(),
                "employer_total": p.employer_total().to_string(),
                "employee_total": p.employee_total().to_string(),
                // Days charged in a month of hiring or leaving, null for the
                // whole month; health insurance is the month or nothing.
                "labor_days": cover.labor.days(),
                "pension_days": cover.pension.days(),
                "health_charged": cover.health,
            }));
        }

        // The rules in force are named even with nobody on the roster.
        let p = premiums_on(
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
            accident_rate,
        )?;
        let (labor_rs, employment_rs, accident_rs) = (p.labor_rs, p.employment_rs, p.accident_rs);
        let (health_rs, pension_rs, dependants_rs) = (p.health_rs, p.pension_rs, p.dependants_rs);
        let (wage_arrears, arrears_rs) = wage_arrears_on(as_of, arrears)?;
        let cap = labor_rs.decimal("max_insured_salary")?;
        let accident_cap = accident_rs.decimal("max_insured_salary")?;
        let commuting = accident_rs.decimal("commuting_rate")?;
        let avg = health_rs.decimal("avg_dependents")?;
        let dependants_cap = laysi_regulation::dependants_charged(dependants_rs, u32::MAX)?;
        let employer_total =
            labor_er + employment_er + accident_er + health_er + pension_er + wage_arrears;
        let employee_total = labor_ee + employment_ee + health_ee;

        // The explanatory lines on screen. Every number in them is read from
        // the payload or the setting, so a rate change updates the wording too.
        let not_insured = match not_employment_insured {
            0 => String::new(),
            n => format!("{n} 位受僱者不參加就業保險。"),
        };
        let accident_body = match accident_rate {
            Some(industry) => format!(
                "行業別災害費率 {}% 加上下班災害費率 {}%，合計 {}%，全額由公司負擔。投保薪資高於 {} 的以上限計費，目前 {accident_capped} 位。",
                percent(industry),
                percent(commuting),
                percent(industry + commuting),
                money(accident_cap),
            ),
            None => format!(
                "上下班災害費率 {}% 加上公司的行業別災害費率，全額由公司負擔。投保薪資高於 {} 的以上限計費。",
                percent(commuting),
                money(accident_cap),
            ),
        };
        let mut notes = json!([
            {
                "title": "勞保投保薪資有上限",
                "body": format!(
                    "勞保、就保與墊償基金以 {} 為上限計費，目前 {} 位高於上限。普通事故保險費率 {}%，公司負擔 {}%、員工自付 {}%、政府補助 {}%。",
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
                "title": "就業保險與普通事故分開計算",
                "body": format!(
                    "費率 {}%，公司負擔 {}%、員工自付 {}%、政府補助 {}%，兩者各自取整。{not_insured}",
                    percent(employment_rs.decimal("rate")?),
                    percent(employment_rs.decimal("employer_share")?),
                    percent(employment_rs.decimal("employee_share")?),
                    percent(employment_rs.decimal("government_share")?),
                ),
                "source": employment_rs.source_note,
                "version": employment_rs.version,
            },
            {
                "title": "職災保險",
                "body": accident_body,
                "source": accident_rs.source_note,
                "version": accident_rs.version,
            },
            {
                "title": "健保依眷口數計費",
                "body": format!(
                    "員工自付本人一口，另加每位在保眷屬一口，眷屬最多計 {dependants_cap} 口；公司負擔一律依全體平均眷口數 {avg} 口分攤。一般保險費率 {}%。",
                    percent(health_rs.decimal("rate")?),
                ),
                "source": format!("{}；{}", health_rs.source_note, dependants_rs.source_note),
                "version": health_rs.version,
            },
            {
                "title": "勞退全額由雇主提繳",
                "body": format!(
                    "提繳率 {}%，全額提繳至員工個人專戶，列為公司成本，不列入薪資單的代扣項目。",
                    percent(pension_rs.decimal("employer_rate")?),
                ),
                "source": pension_rs.source_note,
                "version": pension_rs.version,
            },
            {
                "title": "墊償基金依投保薪資合計提繳",
                "body": format!(
                    "受僱者的勞保投保薪資合計乘 {}%，由公司提繳，不分攤到個人。",
                    percent(arrears_rs.decimal("rate")?),
                ),
                "source": arrears_rs.source_note,
                "version": arrears_rs.version,
            },
        ]);

        // Only when somebody on the roster is insured that way. A page where
        // nobody is has no use for the paragraph.
        if let Some(owner_rs) = staff
            .iter()
            .any(|e| e.insured_as == INSURED_OWNER)
            .then(|| laysi_regulation::resolve("owner_insurance", as_of))
            .transpose()?
        {
            let owners = staff
                .iter()
                .filter(|e| e.insured_as == INSURED_OWNER)
                .count();
            notes.as_array_mut().expect("剛才建的是陣列").push(json!({
                "title": "負責人的健保與勞退分開計算",
                "body": format!(
                    "{owners} 位以負責人身分投保：健保保費自付 {}%，公司不分攤、政府不補助；勞退提繳率 {}%；不參加就業保險，也不提繳墊償基金。勞保與職災的分擔方式與受僱者相同。",
                    percent(owner_rs.decimal("health_employee_share")?),
                    percent(owner_rs.decimal("pension_employer_rate")?),
                ),
                "source": owner_rs.source_note,
                "version": owner_rs.version,
            }));
        }

        // Only in a month somebody is insured for part of it, for the same
        // reason. A hire on the 1st or a leaver on the last day pays a whole
        // month and is not counted.
        if partial > 0 {
            let days_rs = laysi_regulation::resolve("partial_month", as_of)?;
            let labor_days = days_rs.decimal("labor_days_per_month")?;
            let pension_days = days_rs.decimal("pension_days_per_month")?;
            let by_day = match labor_days == pension_days {
                true => format!("勞保、就保、職災與勞退按日計，每月以 {labor_days} 日計"),
                false => format!(
                    "勞保、就保與職災按日計，每月以 {labor_days} 日計；勞退按日計，每月以 {pension_days} 日計"
                ),
            };
            notes.as_array_mut().expect("剛才建的是陣列").push(json!({
                "title": "到職與離職當月",
                "body": format!(
                    "{partial} 位本月投保不足月：{by_day}；健保於投保當月收全月，退保當月免繳。"
                ),
                "source": days_rs.source_note,
                "version": days_rs.version,
            }));
        }

        // A count on its own reads as if everyone is enrolled, so the number
        // of people with no filed bracket travels with it.
        let unfiled_count = unfiled.len();
        let unfiled_note = (unfiled_count > 0).then(|| {
            format!("{unfiled_count} 位員工尚未設定投保級距，請在「員工名冊」的編輯抽屜填入。")
        });
        let unfiled_tag = match unfiled_count {
            0 => String::new(),
            n => format!("（{n} 人尚未設定投保級距）"),
        };
        let accident_summary = match accident_rate {
            Some(_) => money(accident_er),
            None => "未設定費率".to_string(),
        };

        let summary = format!(
            "讀勞健保與勞退 · {} 人{unfiled_tag} · 公司負擔 {}（勞保 {}／就保 {}／職災 {accident_summary}／健保 {}／勞退 {}／墊償 {}）· 依勞保 {}、就保 {}、職災 {}、健保 {}、勞退 {}、墊償 {} 規則",
            rows.len(),
            money(employer_total),
            money(labor_er),
            money(employment_er),
            money(health_er),
            money(pension_er),
            money(wage_arrears),
            labor_rs.version,
            employment_rs.version,
            accident_rs.version,
            health_rs.version,
            pension_rs.version,
            arrears_rs.version,
        );

        Ok(OpOutcome::new(
            json!({
                "as_of": as_of,
                "rows": rows,
                "headcount": rows.len(),
                "capped_count": capped,
                "missing_bracket_count": unfiled_count,
                "missing_brackets": unfiled,
                "missing_bracket_note": unfiled_note,
                "labor_employer": labor_er.to_string(),
                "labor_employee": labor_ee.to_string(),
                "employment_employer": employment_er.to_string(),
                "employment_employee": employment_ee.to_string(),
                // Null while the company has no industry rate: nothing was
                // computed, which is not the same as zero.
                "accident_employer": accident_rate.map(|_| accident_er.to_string()),
                "accident_rate": accident_rate.map(|industry| json!({
                    "industry_percent": percent(industry),
                    "commuting_percent": percent(commuting),
                    "total_percent": percent(industry + commuting),
                })),
                "accident_rate_note": accident_rate.is_none().then_some(
                    "職災保險費尚未計入：請系統管理員在「基礎設定」填入勞保局核定的職災保險行業別災害費率。"
                ),
                "health_employer": health_er.to_string(),
                "health_employee": health_ee.to_string(),
                "pension_employer": pension_er.to_string(),
                "wage_arrears": wage_arrears.to_string(),
                "employer_total": employer_total.to_string(),
                "employee_total": employee_total.to_string(),
                "notes": notes,
                "rulesets": {
                    "labor": ruleset_json(labor_rs),
                    "employment": ruleset_json(employment_rs),
                    "accident": ruleset_json(accident_rs),
                    "health": ruleset_json(health_rs),
                    "pension": ruleset_json(pension_rs),
                    "wage_arrears": ruleset_json(arrears_rs),
                    "owner": owner_set,
                    "dependents": ruleset_json(dependants_rs),
                },
                // The trial calculator offers this many dependants and no more:
                // past the cap every count prices the same.
                "dependents_cap": dependants_cap,
                // The employer share is not in the ledger yet. State that rather
                // than letting it look like a booked expense.
                "posted_to_ledger": false,
                "posting_note": "公司負擔的保費與提繳尚未產生分錄，薪資傳票僅含應發、代扣與實發三段。",
            }),
            summary,
        ))
    }
}

// Single bracket estimate

struct ComputeInsurance;

#[derive(Deserialize, JsonSchema)]
pub struct ComputeInsuranceInput {
    /// Labour insurance salary as filed. Labour and employment insurance
    /// charge it up to the labour ceiling, occupational accident insurance up
    /// to its own.
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
    /// `employee` or `owner`. An owner bears the health premium in full and
    /// has no pension contributed for them. Defaults to `employee`.
    #[serde(default)]
    pub insured_as: Option<String>,
    /// Dependants covered under this person's health insurance. Each is
    /// charged a head of their own, up to the cap. Defaults to none.
    #[serde(default)]
    pub dependents: u32,
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
        "試算一個投保級距的勞保、就保、職災、健保與勞退金額，並回報依哪一版規則，以及公司與員工各分攤多少。費率依適用日期解析；職災依公司設定的行業別災害費率，未設定時不計。"
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
        let insured_as = i.insured_as.as_deref().unwrap_or(INSURED_EMPLOYEE);
        if !matches!(insured_as, INSURED_EMPLOYEE | INSURED_OWNER) {
            return Err(OpError::BadInput("投保身分只能是受僱者或負責人".into()));
        }
        let health = i.health_insured_salary.unwrap_or(i.insured_salary);
        let pension_wage = i.pension_wage.unwrap_or(health);
        let industry = accident_rate(ctx).await?;
        let p = premiums_on(
            as_of,
            &Enrolment {
                insured_as,
                employment_insured: true,
                labor_insured: i.insured_salary,
                health_insured: health,
                pension_wage,
                covered_dependants: i.dependents,
            },
            Coverage::WHOLE,
            industry,
        )?;
        let commuting = p.accident_rs.decimal("commuting_rate")?;

        let accident_summary = match p.accident {
            Some(a) => money(a),
            None => "未設定費率".to_string(),
        };
        let summary = format!(
            "試算保費 · {} · 勞保級距 {} 健保級距 {} · 公司負擔 {}（勞保 {}／就保 {}／職災 {accident_summary}／健保 {}／勞退 {}）本人自付 {} · 依勞保 {}、就保 {}、職災 {}、健保 {}、勞退 {} 規則",
            insured_as_label(insured_as),
            money(i.insured_salary),
            money(health),
            money(p.employer_total()),
            money(p.labor.employer),
            money(p.employment.employer),
            money(p.health.employer),
            money(p.pension),
            money(p.employee_total()),
            p.labor_rs.version,
            p.employment_rs.version,
            p.accident_rs.version,
            p.health_rs.version,
            p.pension_rs.version
        );

        Ok(OpOutcome::new(
            json!({
                "as_of": as_of,
                "insured_as": insured_as,
                "insured_as_label": insured_as_label(insured_as),
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
                // Zero for an owner, who is not in employment insurance.
                "employment": {
                    "insured": p.employment_insured,
                    "employer": p.employment.employer.to_string(),
                    "employee": p.employment.employee.to_string(),
                    "government": p.employment.government.to_string(),
                    "rate_percent": percent(p.employment_rs.decimal("rate")?),
                    "ruleset": ruleset_json(p.employment_rs),
                },
                // `employer` is null while the company has no industry rate.
                "accident": {
                    "employer": p.accident.map(|a| a.to_string()),
                    "charged_on": p.accident_base.to_string(),
                    "max_insured_salary": p.accident_rs.decimal("max_insured_salary")?.to_string(),
                    "industry_rate_percent": industry.map(percent),
                    "commuting_rate_percent": percent(commuting),
                    "rate_percent": industry.map(|rate| percent(rate + commuting)),
                    "ruleset": ruleset_json(p.accident_rs),
                },
                "health": {
                    "insured_salary": health.to_string(),
                    "employer": p.health.employer.to_string(),
                    "employee": p.health.employee.to_string(),
                    "government": p.health.government.to_string(),
                    "rate_percent": percent(p.health_rs.decimal("rate")?),
                    "dependents_charged": p.health_dependants,
                    "dependents_ruleset": ruleset_json(p.dependants_rs),
                    // Not applied to an owner: it scales an employer share
                    // their split does not have.
                    "avg_dependents": match p.owner_rs {
                        Some(_) => None,
                        None => Some(p.health_rs.decimal("avg_dependents")?.to_string()),
                    },
                    "ruleset": ruleset_json(p.health_rs),
                    "owner_ruleset": p.owner_rs.map(ruleset_json),
                },
                "pension": {
                    "contribution_wage": pension_wage.to_string(),
                    "employer": p.pension.to_string(),
                    "rate_percent": match p.owner_rs {
                        Some(owner) => percent(owner.decimal("pension_employer_rate")?),
                        None => percent(p.pension_rs.decimal("employer_rate")?),
                    },
                    "ruleset": ruleset_json(p.pension_rs),
                    "owner_ruleset": p.owner_rs.map(ruleset_json),
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

    /// One whole month for someone in employment insurance, with no
    /// occupational accident rate on file.
    fn priced(
        on: NaiveDate,
        insured_as: &str,
        labor: &str,
        health: &str,
        pension: &str,
        dependants: u32,
    ) -> Result<Premiums, OpError> {
        premiums_on(
            on,
            &Enrolment {
                insured_as,
                employment_insured: true,
                labor_insured: n(labor),
                health_insured: n(health),
                pension_wage: n(pension),
                covered_dependants: dependants,
            },
            Coverage::WHOLE,
            None,
        )
    }

    /// The five regulation reference values. A mismatch means this module has
    /// wired the rules up incorrectly.
    ///
    /// Test conditions: monthly salary 53,000, labour bracket 45,800, health
    /// bracket and pension wage both 53,000.
    #[test]
    fn the_five_reference_benchmarks_come_out_of_the_ruleset() {
        let p = priced(
            d(2026, 7, 26),
            INSURED_EMPLOYEE,
            "45800",
            "53000",
            "53000",
            0,
        )
        .unwrap();
        assert_eq!(p.labor.employer, n("3687"), "公司負擔勞保");
        assert_eq!(p.labor.employee, n("1053"), "員工自付勞保");
        assert_eq!(
            p.health.employer,
            n("2565"),
            "公司負擔健保（含平均眷口 0.56）"
        );
        assert_eq!(p.health.employee, n("822"), "員工自付健保");
        assert_eq!(p.pension, n("3180"), "公司提繳勞退");
        assert_eq!(p.employment.employer, n("321"), "公司負擔就保");
        assert_eq!(p.employment.employee, n("92"), "員工自付就保");
        assert_eq!(p.accident, None, "沒有行業別費率就不計職災");

        assert_eq!(p.employer_total(), n("9753"), "3687 + 321 + 2565 + 3180");
        assert_eq!(p.employee_total(), n("1967"), "1053 + 92 + 822");
    }

    /// Occupational accident insurance is charged on the filed salary up to
    /// its own ceiling, while labour and employment insurance stop at theirs.
    /// The industry rate is the company's; the commuting rate is the rules'.
    #[test]
    fn occupational_accident_is_charged_on_its_own_ceiling() {
        let on = d(2026, 7, 26);
        let at = |labor: &str, insured_as: &str| {
            premiums_on(
                on,
                &Enrolment {
                    insured_as,
                    employment_insured: true,
                    labor_insured: n(labor),
                    health_insured: n("53000"),
                    pension_wage: n("53000"),
                    covered_dependants: 0,
                },
                Coverage::WHOLE,
                Some(n("0.0007")),
            )
            .unwrap()
        };
        let p = at("53000", INSURED_EMPLOYEE);
        assert_eq!(p.labor_base, n("45800"));
        assert_eq!(p.employment.employee, n("92"), "就保同樣按勞保上限");
        assert_eq!(p.accident_base, n("53000"));
        assert_eq!(p.accident, Some(n("74")), "53,000 × 0.14% = 74.2");
        assert_eq!(
            p.employer_total(),
            n("3687") + n("321") + n("74") + n("2565") + n("3180")
        );

        let p = at("100000", INSURED_EMPLOYEE);
        assert_eq!(p.accident_base, n("72800"));
        assert_eq!(p.accident, Some(n("102")), "按職災上限 72,800");

        let boss = at("45800", INSURED_OWNER);
        assert_eq!(boss.accident, Some(n("64")), "負責人的職災同樣由公司負擔");
    }

    /// Employment insurance follows the roster: someone marked out of it pays
    /// nothing and costs nothing, and an owner never pays whatever the roster
    /// says. The arrears fund counts everyone but the owner.
    #[test]
    fn employment_insurance_and_the_arrears_fund_follow_the_identity() {
        let on = d(2026, 7, 26);
        let at = |insured_as: &str, employment_insured: bool| {
            premiums_on(
                on,
                &Enrolment {
                    insured_as,
                    employment_insured,
                    labor_insured: n("53000"),
                    health_insured: n("53000"),
                    pension_wage: n("53000"),
                    covered_dependants: 0,
                },
                Coverage::WHOLE,
                None,
            )
            .unwrap()
        };
        let out = at(INSURED_EMPLOYEE, false);
        assert!(!out.employment_insured);
        assert_eq!(out.employment, Contribution::ZERO);
        assert_eq!(out.labor.employee, n("1053"), "勞保照收");
        assert_eq!(
            out.arrears,
            Some((n("45800"), MonthPart::WHOLE)),
            "墊償照計"
        );

        let boss = at(INSURED_OWNER, true);
        assert!(!boss.employment_insured, "負責人不參加就保，不論名冊");
        assert_eq!(boss.employment, Contribution::ZERO);
        assert_eq!(boss.arrears, None, "負責人不提繳墊償");

        let (fund, _) = wage_arrears_on(
            on,
            [
                out.arrears,
                boss.arrears,
                at(INSURED_EMPLOYEE, true).arrears,
            ],
        )
        .unwrap();
        assert_eq!(fund, n("23"), "91,600 × 0.025% = 22.9");
    }

    /// A period paid in two runs records the fund of the whole month, not two
    /// funds rounded apart. Someone hired on the 9th of June is insured 22
    /// days: 33,586.6667 × 0.025% = 8.40 on its own, 8; with a whole month of
    /// 45,800 beside it, 79,386.6667 × 0.025% = 19.85, 20. The late run
    /// records the 12 that makes 20, where on its own it would round to 11.
    #[test]
    fn the_runs_of_a_period_add_up_to_one_arrears_fund() {
        let on = d(2026, 6, 30);
        let hired = Coverage::of_month(d(2026, 6, 1), on, d(2026, 6, 9), on)
            .unwrap()
            .labor;
        assert_eq!(hired.days(), Some(22));
        let first = Some((n("45800"), hired));
        let late = Some((n("45800"), MonthPart::WHOLE));

        let recorded = period_arrears_on(on, [], Money::ZERO, [first]).unwrap();
        assert_eq!(recorded, n("8"));
        let after = period_arrears_on(on, [first], recorded, [late]).unwrap();
        assert_eq!(after, n("12"));
        assert_eq!(
            recorded + after,
            wage_arrears_on(on, [first, late]).unwrap().0
        );

        // An earlier run that recorded more than the total now comes to (the
        // rate fell in between) leaves the late run at zero, not below it.
        assert_eq!(
            period_arrears_on(on, [first], n("50"), [late]).unwrap(),
            Money::ZERO
        );
    }

    /// The setting is a percentage as the bureau prints it; blank is not set.
    #[test]
    fn the_accident_rate_setting_reads_as_a_percentage() {
        assert_eq!(parse_accident_rate("0.07").unwrap(), Some(n("0.0007")));
        assert_eq!(parse_accident_rate(" 0.11 ").unwrap(), Some(n("0.0011")));
        assert_eq!(parse_accident_rate("").unwrap(), None);
        for bad in ["0", "-0.1", "100", "abc", "0.07%"] {
            assert!(parse_accident_rate(bad).is_err(), "{bad}");
        }
    }

    /// An owner is not an employee of their own company: they bear the whole
    /// health premium and nothing is contributed to their pension account.
    /// Labour insurance splits the same way for both, so it must not move.
    #[test]
    fn an_owner_is_priced_on_the_owner_shares() {
        let staff = priced(
            d(2026, 7, 26),
            INSURED_EMPLOYEE,
            "45800",
            "53000",
            "53000",
            0,
        )
        .unwrap();
        let boss = priced(d(2026, 7, 26), INSURED_OWNER, "45800", "53000", "53000", 0).unwrap();

        assert_eq!(boss.labor, staff.labor, "勞保分擔比率兩者相同");
        assert_eq!(boss.health.employer, Money::ZERO, "健保沒有公司負擔那一份");
        assert_eq!(boss.health.employee, n("2740"), "健保全額自付");
        assert_eq!(boss.pension, Money::ZERO, "勞退不提繳");
        assert_eq!(boss.employer_total(), n("3687"), "公司只剩勞保那一筆");
        assert_eq!(boss.employment, Contribution::ZERO, "負責人不參加就保");
        assert!(boss.owner_rs.is_some(), "要說得出依哪一版算的");
        assert!(staff.owner_rs.is_none());
    }

    /// Labour insurance and pension are blank by design for an owner, so a
    /// zero there is not an unfiled enrolment. Health insurance still is.
    #[test]
    fn an_owner_is_only_chased_for_the_health_bracket() {
        let mut boss = test_employee("active", n("80000"), Money::ZERO, Money::ZERO, Money::ZERO);
        boss.insured_as = INSURED_OWNER.into();
        let m = missing_brackets(&boss);
        assert_eq!(m.labels(), vec!["健保投保金額"]);

        boss.health_insured_salary = n("80000");
        assert!(!missing_brackets(&boss).any(), "填了健保就沒有待辦");
    }

    /// Above the ceiling, labour insurance still charges the ceiling, and the
    /// ceiling comes from the rules rather than from here.
    #[test]
    fn a_salary_above_the_cap_is_charged_at_the_cap() {
        let capped = priced(
            d(2026, 7, 26),
            INSURED_EMPLOYEE,
            "120000",
            "53000",
            "53000",
            0,
        )
        .unwrap();
        let at_cap = priced(
            d(2026, 7, 26),
            INSURED_EMPLOYEE,
            "45800",
            "53000",
            "53000",
            0,
        )
        .unwrap();
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
        let before = laysi_regulation::labor_insurance(base, n("45800"), MonthPart::WHOLE).unwrap();
        assert_eq!(before.employer, n("3687"));

        // Simulate 2027: rate raised to 12%, ceiling raised to 48,200.
        let mut next = base.clone();
        next.version = "2027.1".into();
        next.effective_from = d(2027, 1, 1);
        next.payload["premium_rate"] = json!("0.12");
        next.payload["max_insured_salary"] = json!("48200");

        let after = laysi_regulation::labor_insurance(&next, n("48200"), MonthPart::WHOLE).unwrap();
        // 48,200 × 0.12 × 0.70 = 4,048.8 → 4,049
        assert_eq!(after.employer, n("4049"));
        assert_ne!(after.employer, before.employer, "換規則就要換答案");

        // The same employee under the old rules is unchanged.
        assert_eq!(
            laysi_regulation::labor_insurance(base, n("45800"), MonthPart::WHOLE)
                .unwrap()
                .employer,
            n("3687")
        );

        // Health insurance and pension behave the same way: change the payload,
        // change the answer.
        let hbase = laysi_regulation::resolve("health_insurance", d(2026, 7, 26)).unwrap();
        let mut hnext = hbase.clone();
        hnext.payload["rate"] = json!("0.055");
        let deps = laysi_regulation::resolve("health_dependents", d(2026, 7, 26)).unwrap();
        let h = laysi_regulation::health_insurance(&hnext, deps, n("53000"), 0).unwrap();
        // 53,000 × 0.055 × 0.60 × 1.56 = 2,728.44 → 2,728
        assert_eq!(h.employer, n("2728"));
        assert_ne!(h.employer, n("2565"));

        let pbase = laysi_regulation::resolve("pension", d(2026, 7, 26)).unwrap();
        let mut pnext = pbase.clone();
        pnext.payload["employer_rate"] = json!("0.08");
        assert_eq!(
            laysi_regulation::pension(&pnext, n("53000"), MonthPart::WHOLE).unwrap(),
            n("4240")
        );
    }

    /// The percentage label is derived, not written out separately.
    #[test]
    fn the_percent_label_is_derived_from_the_rate() {
        assert_eq!(percent(n("0.11")), "11");
        assert_eq!(percent(n("0.115")), "11.5");
        assert_eq!(percent(n("0.0517")), "5.17");
        assert_eq!(percent(n("0.06")), "6");
        assert_eq!(percent(n("0.70")), "70");
    }

    /// Working, paid, and no bracket on file: the enrolment was never filed,
    /// and the three items are reported one by one.
    #[test]
    fn an_employed_person_on_pay_with_no_bracket_is_reported() {
        let m = missing_brackets(&test_employee(
            "active",
            n("45000"),
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
        ));
        assert!(m.any());
        assert_eq!(
            m.labels(),
            vec!["勞保投保薪資", "健保投保金額", "勞退提繳工資"]
        );
    }

    /// One bracket can be filed and another not, so each is judged alone.
    #[test]
    fn only_the_bracket_left_at_zero_is_reported() {
        let m = missing_brackets(&test_employee(
            "active",
            n("45000"),
            n("45800"),
            n("45800"),
            Money::ZERO,
        ));
        assert!(!m.labor);
        assert!(!m.health);
        assert!(m.pension);
        assert_eq!(m.labels(), vec!["勞退提繳工資"]);
    }

    #[test]
    fn a_fully_filed_person_is_not_reported() {
        let m = missing_brackets(&test_employee(
            "active",
            n("53000"),
            n("45800"),
            n("53000"),
            n("53000"),
        ));
        assert!(!m.any());
        assert!(m.labels().is_empty());
    }

    /// Someone who has left is no longer enrolled here; someone whose pay is
    /// not agreed yet has nothing to file.
    #[test]
    fn a_departed_person_and_an_unset_salary_are_not_reported() {
        let departed = missing_brackets(&test_employee(
            "left",
            n("45000"),
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
        ));
        assert!(!departed.any());

        let no_salary_yet = missing_brackets(&test_employee(
            "active",
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
        ));
        assert!(!no_salary_yet.any());
    }

    /// Notice was given but the person is still on the payroll, so the
    /// enrolment is still required.
    #[test]
    fn a_person_working_out_notice_is_still_reported() {
        let m = missing_brackets(&test_employee(
            "leaving",
            n("45000"),
            Money::ZERO,
            n("45800"),
            n("45800"),
        ));
        assert!(m.any());
        assert_eq!(m.labels(), vec!["勞保投保薪資"]);
    }

    fn dependant(from: NaiveDate, until: Option<NaiveDate>) -> laysi_store::EmployeeDependent {
        laysi_store::EmployeeDependent {
            id: 0,
            name: "眷屬".into(),
            relation: "child".into(),
            id_no: String::new(),
            birthday: None,
            health_enrolled_from: from,
            health_enrolled_until: until,
        }
    }

    /// Who is covered on the last day of the month is who the month is
    /// charged for. The last day covered still counts; a dependant enrolled
    /// after it does not.
    #[test]
    fn dependants_are_counted_on_the_last_day_of_the_month() {
        let mut e = test_employee("active", n("53000"), n("45800"), n("53000"), n("53000"));
        let end = d(2026, 9, 30);
        e.dependents = vec![
            dependant(d(2026, 3, 1), None),
            dependant(d(2026, 9, 30), None),
            dependant(d(2026, 1, 1), Some(d(2026, 9, 30))),
            dependant(d(2026, 1, 1), Some(d(2026, 9, 29))),
            dependant(d(2026, 10, 1), None),
        ];
        assert_eq!(covered_dependants(&e, end), 3, "前三位在 9/30 在保");
        assert_eq!(
            covered_dependants(&e, d(2026, 2, 28)),
            2,
            "2 月底是第三、四位"
        );
        assert_eq!(covered_dependants(&e, d(2026, 10, 31)), 3, "10 月底");
    }

    /// A month of hiring or leaving is charged by the day, and health
    /// insurance only when the employee is still insured on its last day. A
    /// month the employee is insured on no day is neither.
    #[test]
    fn coverage_follows_the_hiring_and_leaving_dates() {
        let mut e = test_employee("active", n("53000"), n("45800"), n("53000"), n("53000"));
        let (start, end) = (d(2026, 10, 1), d(2026, 10, 31));
        let cover = |e: &Employee| Coverage::of_employee(e, start, end).unwrap();
        assert_eq!(cover(&e), Coverage::WHOLE);

        e.hired_on = d(2026, 10, 20);
        let c = cover(&e);
        assert_eq!(
            (c.labor.days(), c.pension.days(), c.health),
            (Some(11), Some(11), true)
        );
        assert!(!c.is_none());

        (e.hired_on, e.left_on) = (d(2026, 1, 1), Some(d(2026, 10, 15)));
        let c = cover(&e);
        assert_eq!(
            (c.labor.days(), c.pension.days(), c.health),
            (Some(15), Some(15), false)
        );
        assert!(!c.is_none());

        (e.hired_on, e.left_on) = (d(2026, 11, 2), None);
        let c = cover(&e);
        assert!(c.is_none(), "下個月才到職");
        assert!(!c.health);
    }

    /// Dependants are covered through the employee, so a month that ends after
    /// the employee left, or before they joined, counts none of them. Leaving
    /// on the last day still counts the month.
    #[test]
    fn dependants_go_with_the_employee() {
        let mut e = test_employee("left", n("53000"), n("45800"), n("53000"), n("53000"));
        e.dependents = vec![dependant(d(2026, 1, 1), None)];
        e.left_on = Some(d(2026, 10, 15));
        assert_eq!(covered_dependants(&e, d(2026, 10, 31)), 0, "10/15 離職");
        assert_eq!(covered_dependants(&e, d(2026, 9, 30)), 1, "9 月底仍在職");
        e.left_on = Some(d(2026, 10, 31));
        assert_eq!(covered_dependants(&e, d(2026, 10, 31)), 1, "月底離職");
        e.left_on = None;
        e.hired_on = d(2026, 11, 2);
        assert_eq!(covered_dependants(&e, d(2026, 10, 31)), 0, "11 月才到職");
    }

    /// The employee's health deduction is a head per covered dependant on top
    /// of their own; the employer share and labour insurance do not move.
    #[test]
    fn a_covered_dependant_adds_a_head_to_the_health_deduction() {
        let alone = priced(
            d(2026, 7, 26),
            INSURED_EMPLOYEE,
            "45800",
            "53000",
            "53000",
            0,
        )
        .unwrap();
        let two = priced(
            d(2026, 7, 26),
            INSURED_EMPLOYEE,
            "45800",
            "53000",
            "53000",
            2,
        )
        .unwrap();
        assert_eq!(two.health.employee, n("2466"), "822 × 3");
        assert_eq!(two.health.employer, alone.health.employer);
        assert_eq!(two.labor, alone.labor);
        assert_eq!(two.health_dependants, 2);

        let five = priced(
            d(2026, 7, 26),
            INSURED_EMPLOYEE,
            "45800",
            "53000",
            "53000",
            5,
        )
        .unwrap();
        assert_eq!(five.health_dependants, 3, "超過上限以上限計");
        assert_eq!(five.health.employee, n("3288"), "822 × 4");
    }

    /// No applicable version is an error; premiums are never computed from a
    /// guessed rate.
    #[test]
    fn a_date_with_no_ruleset_is_refused_never_guessed() {
        let err = priced(
            d(2025, 12, 31),
            INSURED_EMPLOYEE,
            "45800",
            "53000",
            "53000",
            0,
        )
        .unwrap_err();
        assert!(matches!(err, OpError::Regulation(_)));
        assert!(err.to_string().contains("新增涵蓋該日期的版本"));
    }
}
