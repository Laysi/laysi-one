//! The regulation engine.
//!
//! Insurance brackets, health insurance rates, pension contribution rates,
//! VAT rates, the minimum wage: these change every year, and they change
//! retroactively. An amendment in July may require recomputing from January.
//! If `45800` or `0.0517` were scattered through the payroll module, every
//! amendment would mean editing code and redeploying, and there would be no
//! way to answer which version last year's figure was computed under.
//!
//! Hence the rule: regulation values may exist only in the `reg_ruleset`
//! table and must never appear in Rust source. The code describes how to
//! compute, never what numbers to compute with.
//!
//! Three consequences:
//!
//! 1. Every calculator takes a [`Ruleset`], resolved from an `as_of` date.
//!    There is no such thing as "the current rate", only the rate applicable
//!    on a date.
//! 2. [`resolve`] returns an error when no rule applies, never a default.
//!    The system refuses to compute rather than computing someone's pay from
//!    a guess.
//! 3. Every computed amount is stored with its ruleset version, so an audit
//!    can say which version produced it.
//!
//! This crate stays synchronous and knows nothing about SQL: the server
//! reads `reg_ruleset` at boot and after every edit and hands the rows to
//! [`install`]. [`seed`] is the factory set shipped with the image, written for
//! each domain the table holds no version of and never again for that domain.
//!
//! Updating regulation end to end: save a ruleset with a later
//! `effective_from`. Results for earlier dates are unchanged and later dates
//! pick up the new rules automatically, with no release.

use chrono::{Datelike, NaiveDate};
use rust_decimal::{Decimal, RoundingStrategy};
use std::sync::{OnceLock, RwLock};

pub type Money = Decimal;

/// The factory set, embedded at compile time. It fills an empty `reg_ruleset`
/// and is the answer before the server has installed anything. Register a new
/// domain here; a newly published year is added as a version on the screen.
const SOURCES: &[(&str, &str)] = &[
    (
        "2026/labor_insurance.toml",
        include_str!("../../../regulation/2026/labor_insurance.toml"),
    ),
    (
        "2026/health_insurance.toml",
        include_str!("../../../regulation/2026/health_insurance.toml"),
    ),
    (
        "2026/pension.toml",
        include_str!("../../../regulation/2026/pension.toml"),
    ),
    (
        "2026/vat.toml",
        include_str!("../../../regulation/2026/vat.toml"),
    ),
    (
        "2026/minimum_wage.toml",
        include_str!("../../../regulation/2026/minimum_wage.toml"),
    ),
    (
        "2026/withholding.toml",
        include_str!("../../../regulation/2026/withholding.toml"),
    ),
    (
        "2026/annual_leave.toml",
        include_str!("../../../regulation/2026/annual_leave.toml"),
    ),
    (
        "2026/depreciation.toml",
        include_str!("../../../regulation/2026/depreciation.toml"),
    ),
    (
        "2026/overtime.toml",
        include_str!("../../../regulation/2026/overtime.toml"),
    ),
    (
        "2026/owner_insurance.toml",
        include_str!("../../../regulation/2026/owner_insurance.toml"),
    ),
    (
        "2026/leave_pay.toml",
        include_str!("../../../regulation/2026/leave_pay.toml"),
    ),
    (
        "2013/health_dependents.toml",
        include_str!("../../../regulation/2013/health_dependents.toml"),
    ),
    (
        "2005/partial_month.toml",
        include_str!("../../../regulation/2005/partial_month.toml"),
    ),
];

/// Regulation domains the system knows. A rule file's `domain` must be one
/// of these.
pub const DOMAINS: &[&str] = &[
    "labor_insurance",
    "health_insurance",
    "pension",
    "vat",
    "minimum_wage",
    "withholding",
    "annual_leave",
    "depreciation",
    "overtime",
    "owner_insurance",
    "leave_pay",
    "health_dependents",
    "partial_month",
];

#[derive(Debug, Clone, serde::Deserialize)]
struct RawRuleset {
    domain: String,
    version: String,
    effective_from: NaiveDate,
    #[serde(default)]
    effective_to: Option<NaiveDate>,
    source_note: String,
    payload: serde_json::Value,
}

/// The rules applicable to one domain over a period.
#[derive(Debug, Clone)]
pub struct Ruleset {
    pub domain: String,
    pub version: String,
    pub effective_from: NaiveDate,
    /// `None` means still in force.
    pub effective_to: Option<NaiveDate>,
    /// Legal source. Audits and reports must be able to cite the article.
    pub source_note: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum RegulationError {
    #[error("查無「{domain}」在 {as_of} 適用的法規版本，請在法規版本新增涵蓋該日期的版本。")]
    NotFound { domain: String, as_of: NaiveDate },

    #[error("法規 {domain}/{version} 缺少必要欄位「{key}」")]
    MissingField {
        domain: String,
        version: String,
        key: String,
    },

    #[error("法規 {domain}/{version} 的欄位「{key}」不是合法數值：{value}")]
    BadNumber {
        domain: String,
        version: String,
        key: String,
        value: String,
    },

    #[error("法規 {domain}/{version} 的欄位「{key}」要介於 {min} 到 {max}，收到 {value}")]
    OutOfRange {
        domain: String,
        version: String,
        key: String,
        value: Decimal,
        min: Decimal,
        max: Decimal,
    },

    #[error("法規 {domain}/{version} 的欄位「{key}」要是 0 以上的整數，收到 {value}")]
    NotWholeNumber {
        domain: String,
        version: String,
        key: String,
        value: Decimal,
    },

    #[error("這個計算子需要 {expected} 的規則，收到的卻是 {found}")]
    WrongDomain {
        expected: &'static str,
        found: String,
    },
}

fn sorted(mut v: Vec<Ruleset>) -> Vec<Ruleset> {
    v.sort_by(|a, b| {
        a.domain
            .cmp(&b.domain)
            .then(a.effective_from.cmp(&b.effective_from))
    });
    v
}

/// The factory set shipped with the image, parsed from the embedded files.
///
/// The server writes a domain's versions into `reg_ruleset` only while the
/// table holds none for that domain: a new install, or a domain a new release
/// brings. From then on the table answers, so a corrected rate and a deleted
/// version both survive the next start.
pub fn seed() -> Vec<Ruleset> {
    sorted(
        SOURCES
            .iter()
            .map(|(file, text)| {
                let raw: RawRuleset =
                    toml::from_str(text).unwrap_or_else(|e| panic!("規則檔 {file} 無法解析：{e}"));
                assert!(
                    DOMAINS.contains(&raw.domain.as_str()),
                    "規則檔 {file} 的 domain「{}」不在本系統認得的清單中",
                    raw.domain
                );
                Ruleset {
                    domain: raw.domain,
                    version: raw.version,
                    effective_from: raw.effective_from,
                    effective_to: raw.effective_to,
                    source_note: raw.source_note,
                    payload: raw.payload,
                }
            })
            .collect(),
    )
}

static INSTALLED: RwLock<Option<&'static [Ruleset]>> = RwLock::new(None);

/// Replace the rules in force with what the database holds. Called at boot
/// and after every edit, so a saved rate applies without a restart.
///
/// ponytail: the vector is leaked so `resolve` can hand back `&'static` and
/// no calculator needs an `Arc`. Regulation is edited a few times a year, so
/// this leaks kilobytes a year; swap in `arc-swap` if that stops being true.
pub fn install(rulesets: Vec<Ruleset>) {
    let leaked: &'static [Ruleset] = Vec::leak(sorted(rulesets));
    *INSTALLED.write().expect("法規快取") = Some(leaked);
}

fn rulesets() -> &'static [Ruleset] {
    if let Some(installed) = *INSTALLED.read().expect("法規快取") {
        return installed;
    }
    static FACTORY: OnceLock<Vec<Ruleset>> = OnceLock::new();
    FACTORY.get_or_init(seed)
}

/// Every ruleset in force. Used by the `list_regulations` operation.
pub fn all() -> &'static [Ruleset] {
    rulesets()
}

/// Resolve the rules applicable to a domain on `as_of`.
///
/// When effective ranges overlap, the latest `effective_from` wins: the most
/// recently published rule takes precedence.
pub fn resolve(domain: &str, as_of: NaiveDate) -> Result<&'static Ruleset, RegulationError> {
    rulesets()
        .iter()
        .filter(|r| r.domain == domain)
        .filter(|r| r.effective_from <= as_of)
        .filter(|r| r.effective_to.is_none_or(|end| as_of <= end))
        .max_by_key(|r| r.effective_from)
        .ok_or_else(|| RegulationError::NotFound {
            domain: domain.to_string(),
            as_of,
        })
}

impl Ruleset {
    fn expect_domain(&self, expected: &'static str) -> Result<(), RegulationError> {
        if self.domain != expected {
            return Err(RegulationError::WrongDomain {
                expected,
                found: self.domain.clone(),
            });
        }
        Ok(())
    }

    /// Read a decimal from the payload, supporting nested `a.b` paths.
    ///
    /// Values are always written as strings in the TOML: TOML floats would
    /// introduce binary error, and this computes people's pay.
    pub fn decimal(&self, key: &str) -> Result<Money, RegulationError> {
        let mut node = &self.payload;
        for part in key.split('.') {
            node = node
                .get(part)
                .ok_or_else(|| RegulationError::MissingField {
                    domain: self.domain.clone(),
                    version: self.version.clone(),
                    key: key.to_string(),
                })?;
        }
        let text = node.as_str().ok_or_else(|| RegulationError::BadNumber {
            domain: self.domain.clone(),
            version: self.version.clone(),
            key: key.to_string(),
            value: node.to_string(),
        })?;
        text.parse::<Decimal>()
            .map_err(|_| RegulationError::BadNumber {
                domain: self.domain.clone(),
                version: self.version.clone(),
                key: key.to_string(),
                value: text.to_string(),
            })
    }
}

/// Round to whole dollars.
///
/// Premiums and taxes round half away from zero, not banker's rounding: the
/// default `Decimal::round()` would turn 3526.5 into 3526, which disagrees
/// with the authorities' arithmetic.
fn to_dollars(v: Money) -> Money {
    v.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

/// A premium split three ways.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contribution {
    pub employer: Money,
    pub employee: Money,
    pub government: Money,
}

impl Contribution {
    pub fn total(&self) -> Money {
        self.employer + self.employee + self.government
    }
}

/// How much of a month a labour insurance premium or a pension contribution
/// is charged for. Built by [`month_part`]; health insurance has no part of a
/// month, only a month charged or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthPart(Option<(u32, u32)>);

impl MonthPart {
    /// Covered from the first of the month through its last day.
    pub const WHOLE: MonthPart = MonthPart(None);

    /// Days charged, or `None` for the whole month.
    pub fn days(self) -> Option<u32> {
        self.0.map(|(days, _)| days)
    }

    /// `amount` for this part of the month, before rounding.
    fn of(self, amount: Money) -> Money {
        match self.0 {
            None => amount,
            Some((days, per_month)) => amount * Money::from(days) / Money::from(per_month),
        }
    }
}

/// The part of a month a person covered from `from` to `to`, both counted, is
/// charged for, on a month counted as `key` days in `partial_month`.
///
/// Covered on the month's first and last day is the whole month. Otherwise an
/// end inside the month counts from its day of the month, capped at the count
/// so the 31st counts as the last counted day, and an end at or past the
/// month's last day counts as the last counted day itself: withdrawn on 28
/// February is a whole month, enrolled that day is three days of thirty.
/// Coverage that misses the month entirely is zero days.
pub fn month_part(
    rs: &Ruleset,
    key: &str,
    month_start: NaiveDate,
    month_end: NaiveDate,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<MonthPart, RegulationError> {
    rs.expect_domain("partial_month")?;
    let count = rs.decimal(key)?;
    // A month is never counted shorter or longer than a calendar month.
    let (min, max) = (Money::from(28), Money::from(31));
    if count != count.trunc() || count < min || count > max {
        return Err(RegulationError::OutOfRange {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: key.to_string(),
            value: count,
            min,
            max,
        });
    }
    let per_month: u32 = count.trunc().try_into().unwrap_or(u32::MAX);
    let (from, to) = (from.max(month_start), to.min(month_end));
    if from > to {
        return Ok(MonthPart(Some((0, per_month))));
    }
    if from == month_start && to == month_end {
        return Ok(MonthPart::WHOLE);
    }
    let first = match from == month_start {
        true => 1,
        false => from.day().min(per_month),
    };
    let last = match to == month_end {
        true => per_month,
        false => to.day().min(per_month),
    };
    let days = (last + 1).saturating_sub(first);
    Ok(match days >= per_month {
        true => MonthPart::WHOLE,
        false => MonthPart(Some((days, per_month))),
    })
}

/// Labour insurance ordinary risk premium for `part` of a month.
///
/// Salary above the insured ceiling is charged at the ceiling. That means
/// the bracket is capped, not that the salary was truncated. A part of a
/// month is taken before rounding, each share on its own.
pub fn labor_insurance(
    rs: &Ruleset,
    insured_salary: Money,
    part: MonthPart,
) -> Result<Contribution, RegulationError> {
    rs.expect_domain("labor_insurance")?;
    let cap = rs.decimal("max_insured_salary")?;
    let premium = insured_salary.min(cap) * rs.decimal("premium_rate")?;
    Ok(Contribution {
        employer: to_dollars(part.of(premium * rs.decimal("employer_share")?)),
        employee: to_dollars(part.of(premium * rs.decimal("employee_share")?)),
        government: to_dollars(part.of(premium * rs.decimal("government_share")?)),
    })
}

/// How many of a person's covered dependants are charged: all of them, up to
/// the cap in `health_dependents`.
pub fn dependants_charged(rs: &Ruleset, covered: u32) -> Result<u32, RegulationError> {
    rs.expect_domain("health_dependents")?;
    let cap = rs.decimal("max_counted")?;
    if cap < Money::ZERO || cap != cap.trunc() {
        return Err(RegulationError::NotWholeNumber {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: "max_counted".to_string(),
            value: cap,
        });
    }
    let cap: u32 = cap.trunc().try_into().unwrap_or(u32::MAX);
    Ok(covered.min(cap))
}

/// National health insurance ordinary premium.
///
/// The employer share is multiplied by the average number of dependants
/// across all insured persons rather than this employee's actual
/// dependants, because the law apportions it that way. A single employee's
/// employer share therefore exceeds twice their own contribution.
///
/// The employee pays one head for themselves and one for each covered
/// dependant, up to the cap in `dependants_rs`; the government subsidises the
/// same heads. Each head is rounded before the heads are added up, which is
/// how the insurer bills it: multiplying first and rounding once differs by a
/// dollar on some brackets.
pub fn health_insurance(
    rs: &Ruleset,
    dependants_rs: &Ruleset,
    insured_salary: Money,
    covered_dependants: u32,
) -> Result<Contribution, RegulationError> {
    rs.expect_domain("health_insurance")?;
    let heads = Money::from(1 + dependants_charged(dependants_rs, covered_dependants)?);
    let premium = insured_salary * rs.decimal("rate")?;
    Ok(Contribution {
        employer: to_dollars(
            premium * rs.decimal("employer_share")? * rs.decimal("avg_dependents")?,
        ),
        employee: to_dollars(premium * rs.decimal("employee_share")?) * heads,
        government: to_dollars(premium * rs.decimal("government_share")?) * heads,
    })
}

/// National health insurance premium for a person insured as the employer.
///
/// The rate is the same one everybody pays, so it comes from the health
/// insurance ruleset; only the split differs, and that comes from
/// `owner_insurance`. The average-dependants multiplier is not applied,
/// because it scales an employer share this split does not have. Covered
/// dependants are charged per head as in [`health_insurance`]: an owner bears
/// their family's premium in full, as their own.
pub fn health_insurance_owner(
    health_rs: &Ruleset,
    owner_rs: &Ruleset,
    dependants_rs: &Ruleset,
    insured_salary: Money,
    covered_dependants: u32,
) -> Result<Contribution, RegulationError> {
    health_rs.expect_domain("health_insurance")?;
    owner_rs.expect_domain("owner_insurance")?;
    let heads = Money::from(1 + dependants_charged(dependants_rs, covered_dependants)?);
    let premium = insured_salary * health_rs.decimal("rate")?;
    let head = |key: &str| -> Result<Money, RegulationError> {
        Ok(to_dollars(premium * owner_rs.decimal(key)?) * heads)
    };
    Ok(Contribution {
        employer: head("health_employer_share")?,
        employee: head("health_employee_share")?,
        government: head("health_government_share")?,
    })
}

/// Employer pension contribution for `part` of a month. Borne entirely by
/// the employer.
pub fn pension(
    rs: &Ruleset,
    contribution_wage: Money,
    part: MonthPart,
) -> Result<Money, RegulationError> {
    rs.expect_domain("pension")?;
    Ok(to_dollars(
        part.of(contribution_wage * rs.decimal("employer_rate")?),
    ))
}

/// Pension contributed by the company for a person insured as the employer.
///
/// A separate rate rather than a hardcoded zero: what an owner is owed is a
/// regulated figure like any other, and an amendment that starts requiring a
/// contribution must be a ruleset edit, not a release.
pub fn pension_owner(
    owner_rs: &Ruleset,
    contribution_wage: Money,
    part: MonthPart,
) -> Result<Money, RegulationError> {
    owner_rs.expect_domain("owner_insurance")?;
    Ok(to_dollars(part.of(
        contribution_wage * owner_rs.decimal("pension_employer_rate")?
    )))
}

/// VAT amount.
pub fn vat(rs: &Ruleset, net_amount: Money) -> Result<Money, RegulationError> {
    rs.expect_domain("vat")?;
    Ok(to_dollars(net_amount * rs.decimal("rate")?))
}

/// The tax contained in a tax-inclusive amount; the inverse of [`vat`].
///
/// Quoted prices in Taiwan are usually tax inclusive, so a receivable often
/// starts from the total rather than the net. Rounding is applied once, to
/// the tax, and the net is whatever is left, so net plus tax always equals
/// the total the customer was quoted.
pub fn vat_from_total(rs: &Ruleset, total_amount: Money) -> Result<Money, RegulationError> {
    rs.expect_domain("vat")?;
    let rate = rs.decimal("rate")?;
    Ok(to_dollars(
        total_amount - total_amount / (Decimal::ONE + rate),
    ))
}

/// Minimum wage check. `None` means compliant, `Some(floor)` means below the
/// statutory minimum.
///
/// `basis` names the payload key the wage is measured against, `monthly` or
/// `hourly`: the two floors are published together and neither one bounds the
/// other, so an hourly rate compared against the monthly floor fails every
/// time and a monthly salary compared against the hourly floor never fails.
/// An unknown basis is a missing-field error rather than a fallback, for the
/// same reason [`resolve`] never guesses.
pub fn minimum_wage_shortfall(
    rs: &Ruleset,
    wage: Money,
    basis: &str,
) -> Result<Option<Money>, RegulationError> {
    rs.expect_domain("minimum_wage")?;
    let floor = rs.decimal(basis)?;
    Ok((wage < floor).then_some(floor))
}

/// The `annual_leave` tiers as `(months, days)`. Rows missing either number
/// are skipped.
fn annual_leave_tiers(rs: &Ruleset) -> Result<Vec<(u32, u32)>, RegulationError> {
    rs.expect_domain("annual_leave")?;
    let tiers = rs
        .payload
        .get("tiers")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: "tiers".into(),
        })?;

    let read =
        |t: &serde_json::Value, key: &str| -> Option<u32> { t.get(key)?.as_str()?.parse().ok() };
    Ok(tiers
        .iter()
        .filter_map(|t| Some((read(t, "months")?, read(t, "days")?)))
        .collect())
}

/// The annual leave year that `service_months` falls in, as `[start, end)` in
/// completed months of service.
///
/// A year begins at every tier threshold and at every completed year of
/// service, so the stretch before the first tier and the part of a tier that
/// ends at the first anniversary are years of their own. The entitlement is
/// constant inside one: `annual_leave_days(start)` is the year's.
pub fn annual_leave_year(rs: &Ruleset, service_months: u32) -> Result<(u32, u32), RegulationError> {
    let tiers = annual_leave_tiers(rs)?;
    let year = service_months / 12;
    let start = tiers
        .iter()
        .map(|(months, _)| *months)
        .filter(|months| *months <= service_months)
        .fold(year * 12, u32::max);
    let end = tiers
        .iter()
        .map(|(months, _)| *months)
        .filter(|months| *months > service_months)
        .fold((year + 1) * 12, u32::min);
    Ok((start, end))
}

/// Annual leave days. `service_months` is completed months since hire.
pub fn annual_leave_days(rs: &Ruleset, service_months: u32) -> Result<u32, RegulationError> {
    let tiers = annual_leave_tiers(rs)?;

    let mut days = 0;
    let mut matched_months = 0;
    for (months, d) in tiers {
        if service_months >= months && months >= matched_months {
            days = d;
            matched_months = months;
        }
    }

    // After ten years, one more day per completed year up to the cap. This is
    // a rule rather than an enumeration.
    if service_months >= 120 {
        let inc: u32 = rs
            .decimal("over_ten_years.increment_per_year")?
            .trunc()
            .try_into()
            .unwrap_or(1);
        let max: u32 = rs
            .decimal("over_ten_years.max_days")?
            .trunc()
            .try_into()
            .unwrap_or(30);
        days = (days + (service_months - 120) / 12 * inc).min(max);
    }
    Ok(days)
}

/// One kind of day overtime can fall on, as the ruleset lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvertimeDayType {
    pub code: String,
    pub label: String,
    /// Longest overtime the law allows on such a day.
    pub max_hours: Money,
}

fn payload_array<'a>(
    rs: &'a Ruleset,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>, RegulationError> {
    let mut node = &rs.payload;
    for part in key.split('.') {
        node = node
            .get(part)
            .ok_or_else(|| RegulationError::MissingField {
                domain: rs.domain.clone(),
                version: rs.version.clone(),
                key: key.to_string(),
            })?;
    }
    node.as_array().ok_or_else(|| RegulationError::BadNumber {
        domain: rs.domain.clone(),
        version: rs.version.clone(),
        key: key.to_string(),
        value: node.to_string(),
    })
}

fn field(rs: &Ruleset, node: &serde_json::Value, key: &str) -> Result<Money, RegulationError> {
    let text =
        node.get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RegulationError::MissingField {
                domain: rs.domain.clone(),
                version: rs.version.clone(),
                key: key.to_string(),
            })?;
    text.parse::<Decimal>()
        .map_err(|_| RegulationError::BadNumber {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: key.to_string(),
            value: text.to_string(),
        })
}

/// The kinds of day the overtime rules distinguish, in the order listed.
pub fn overtime_day_types(rs: &Ruleset) -> Result<Vec<OvertimeDayType>, RegulationError> {
    rs.expect_domain("overtime")?;
    payload_array(rs, "day_types")?
        .iter()
        .map(|t| {
            Ok(OvertimeDayType {
                code: t
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                label: t
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                max_hours: field(rs, t, "max_hours")?,
            })
        })
        .collect()
}

/// Extended working time allowed in one month, across every kind of day.
pub fn overtime_monthly_max_hours(rs: &Ruleset) -> Result<Money, RegulationError> {
    rs.expect_domain("overtime")?;
    rs.decimal("monthly_max_hours")
}

/// The ordinary hourly wage of someone on `monthly_salary`: the salary over
/// the hours the ruleset says a month holds.
///
/// Someone already paid by the hour does not go through here; their rate is
/// the ordinary hourly wage already, and dividing it would price their
/// overtime below their ordinary time.
pub fn ordinary_hourly_wage(rs: &Ruleset, monthly_salary: Money) -> Result<Money, RegulationError> {
    rs.expect_domain("overtime")?;
    Ok(monthly_salary / rs.decimal("monthly_hours")?)
}

/// Pay for `hours` of overtime worked on a `day_type` by someone whose
/// ordinary wage is `hourly_wage`.
///
/// The hours are spent against the bands in order: the first band's
/// multiplier up to its `up_to_hours`, then the next. A band without
/// `up_to_hours` runs to the end. Monthly-paid callers convert with
/// [`ordinary_hourly_wage`] first.
pub fn overtime_pay(
    rs: &Ruleset,
    hourly_wage: Money,
    day_type: &str,
    hours: Money,
) -> Result<Money, RegulationError> {
    rs.expect_domain("overtime")?;
    let types = payload_array(rs, "day_types")?;
    let matched = types
        .iter()
        .find(|t| t.get("code").and_then(|v| v.as_str()) == Some(day_type))
        .ok_or_else(|| RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: format!("day_types.{day_type}"),
        })?;

    let bands = matched
        .get("bands")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: format!("day_types.{day_type}.bands"),
        })?;

    let mut left = hours;
    let mut pay = Money::ZERO;
    for band in bands {
        if left <= Money::ZERO {
            break;
        }
        let multiplier = field(rs, band, "multiplier")?;
        let span = match band.get("up_to_hours") {
            Some(_) => field(rs, band, "up_to_hours")?.min(left),
            None => left,
        };
        pay += hourly_wage * multiplier * span;
        left -= span;
    }
    Ok(to_dollars(pay))
}

/// What one kind of leave pays, as the `leave_pay` ruleset states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeavePay {
    /// Share of the ordinary wage paid for a day of this leave.
    pub ratio: Money,
    /// Days a calendar year paid at `ratio`, and the share paid beyond them.
    pub yearly: Option<(Money, Money)>,
    /// Days a calendar year that stand apart before the rest count towards
    /// sickness leave's yearly days.
    pub separate_days: Option<Money>,
    /// Months of service before `ratio` applies, and the share paid before.
    pub short_service: Option<(u32, Money)>,
}

/// The kind of leave whose yearly days other leave can count towards.
const SICK: &str = "sick";

/// The `leave_pay` rule for one kind of leave. A kind the ruleset does not
/// list is an error, not unpaid: that would be the system deciding a wage.
pub fn leave_pay(rs: &Ruleset, kind: &str) -> Result<LeavePay, RegulationError> {
    rs.expect_domain("leave_pay")?;
    let node = rs
        .payload
        .get(kind)
        .ok_or_else(|| RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: kind.to_string(),
        })?;
    // A share is of one day's wage and a count is of days or months: outside
    // these bounds payroll would take more than a day, or pay for more hours
    // than were on leave.
    let bounded = |key: &str, max: Option<Money>| -> Result<Money, RegulationError> {
        let value = field(rs, node, key)?;
        if value < Money::ZERO || max.is_some_and(|max| value > max) {
            return Err(RegulationError::OutOfRange {
                domain: rs.domain.clone(),
                version: rs.version.clone(),
                key: format!("{kind}.{key}"),
                value,
                min: Money::ZERO,
                max: max.unwrap_or(Money::MAX),
            });
        }
        Ok(value)
    };
    let share = |key: &str| bounded(key, Some(Money::ONE));
    let count = |key: &str| match node.get(key) {
        Some(_) => bounded(key, None).map(Some),
        None => Ok(None),
    };
    let yearly = match count("yearly_days")? {
        Some(days) => Some((days, share("beyond_ratio")?)),
        None => None,
    };
    let short_service = match count("min_service_months")? {
        Some(months) => Some((
            months.trunc().try_into().unwrap_or(u32::MAX),
            share("short_service_ratio")?,
        )),
        None => None,
    };
    Ok(LeavePay {
        ratio: share("ratio")?,
        yearly,
        separate_days: count("separate_days")?,
        short_service,
    })
}

/// One date's share of an approved leave request.
#[derive(Debug, Clone)]
pub struct LeaveDay {
    pub date: NaiveDate,
    pub kind: String,
    /// Working days of the request that fall on this date.
    pub days: Money,
    /// Completed months of service on this date.
    pub service_months: u32,
}

/// Days of wage paid for each leave day, in the order given.
///
/// `days` must be in date order and cover the calendar year up to the last
/// date that matters: sickness leave's yearly allowance is counted from
/// 1 January, so a day in June is only priced right knowing February. Each
/// date resolves the ruleset in force on it.
pub fn leave_paid_days(days: &[LeaveDay]) -> Result<Vec<Money>, RegulationError> {
    use chrono::Datelike;
    use std::collections::HashMap;

    let mut year = None;
    // Days used this year against a yearly allowance, and days set apart.
    let mut used: HashMap<&str, Money> = HashMap::new();
    let mut apart: HashMap<&str, Money> = HashMap::new();
    let mut paid = Vec::with_capacity(days.len());
    for day in days {
        if year != Some(day.date.year()) {
            year = Some(day.date.year());
            used.clear();
            apart.clear();
        }
        let rule = leave_pay(resolve("leave_pay", day.date)?, &day.kind)?;
        let ratio = match rule.short_service {
            Some((months, short)) if day.service_months < months => short,
            _ => rule.ratio,
        };
        paid.push(match rule.yearly {
            Some((allowance, beyond)) => {
                let so_far = used.entry(day.kind.as_str()).or_default();
                let within = day.days.min((allowance - *so_far).max(Money::ZERO));
                *so_far += day.days;
                within * ratio + (day.days - within) * beyond
            }
            None => day.days * ratio,
        });
        if let Some(separate) = rule.separate_days {
            let set_apart = apart.entry(day.kind.as_str()).or_default();
            let here = day.days.min((separate - *set_apart).max(Money::ZERO));
            *set_apart += here;
            *used.entry(SICK).or_default() += day.days - here;
        }
    }
    Ok(paid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn every_declared_domain_has_a_rule_file() {
        for domain in DOMAINS {
            resolve(domain, d(2026, 7, 25))
                .unwrap_or_else(|e| panic!("{domain} 缺少 2026 年的規則：{e}"));
        }
    }

    // The reference values below are the figures actually displayed on the
    // insurance and payroll screens. A mismatch means the calculator is
    // wrong.

    /// Article 24 pays the first two hours at one rate and the rest at
    /// another, so three hours is not three times the first hour. Nothing in
    /// this crate knows what those rates are: replace the payload and the
    /// same three hours must cost something else.
    #[test]
    fn overtime_walks_the_bands_and_reads_them_from_the_payload() {
        let rs = resolve("overtime", d(2026, 7, 25)).unwrap();
        // 40000 over 240 hours is 166.6667 an hour.
        let wage = ordinary_hourly_wage(rs, dec!(40000)).unwrap();
        assert_eq!(
            overtime_pay(rs, wage, "weekday", dec!(2)).unwrap(),
            dec!(447)
        );
        assert_eq!(
            overtime_pay(rs, wage, "weekday", dec!(3)).unwrap(),
            dec!(725)
        );
        assert_eq!(
            overtime_pay(rs, wage, "rest_day", dec!(3)).unwrap(),
            dec!(725)
        );
        assert_eq!(
            overtime_pay(rs, wage, "weekday", Money::ZERO).unwrap(),
            Money::ZERO
        );
        assert!(overtime_pay(rs, wage, "public_holiday", dec!(1)).is_err());

        let mut changed = rs.clone();
        changed.payload["day_types"][0]["bands"][0]["multiplier"] = "2".into();
        assert_ne!(
            overtime_pay(&changed, wage, "weekday", dec!(2)).unwrap(),
            overtime_pay(rs, wage, "weekday", dec!(2)).unwrap()
        );

        let types = overtime_day_types(rs).unwrap();
        assert_eq!(
            types.iter().map(|t| t.code.as_str()).collect::<Vec<_>>(),
            ["weekday", "rest_day"]
        );
        assert_eq!(types[0].max_hours, dec!(4));
        assert_eq!(overtime_monthly_max_hours(rs).unwrap(), dec!(46));
    }

    #[test]
    fn labor_insurance_matches_the_prototype() {
        let rs = resolve("labor_insurance", d(2026, 7, 25)).unwrap();
        let c = labor_insurance(rs, dec!(45800), MonthPart::WHOLE).unwrap();
        assert_eq!(c.employer, dec!(3527), "公司負擔勞保");
        assert_eq!(c.employee, dec!(1008), "員工自付勞保");
    }

    #[test]
    fn health_insurance_matches_the_prototype() {
        let rs = resolve("health_insurance", d(2026, 7, 25)).unwrap();
        let deps = resolve("health_dependents", d(2026, 7, 25)).unwrap();
        let c = health_insurance(rs, deps, dec!(53000), 0).unwrap();
        assert_eq!(c.employer, dec!(2647), "公司負擔健保（含平均眷口 1.61）");
        assert_eq!(c.employee, dec!(822), "員工自付健保");
    }

    #[test]
    fn pension_matches_the_prototype() {
        let rs = resolve("pension", d(2026, 7, 25)).unwrap();
        assert_eq!(
            pension(rs, dec!(53000), MonthPart::WHOLE).unwrap(),
            dec!(3180)
        );
    }

    /// The bureau's day count: a month counts as thirty days whatever its
    /// length, and both ends are counted.
    #[test]
    fn a_part_of_a_month_counts_on_a_thirty_day_month() {
        let rs = resolve("partial_month", d(2026, 7, 25)).unwrap();
        let days = |month: (NaiveDate, NaiveDate), from, to| {
            month_part(rs, "labor_days_per_month", month.0, month.1, from, to)
                .unwrap()
                .days()
        };
        let oct = (d(2026, 10, 1), d(2026, 10, 31));
        assert_eq!(days(oct, d(2026, 1, 1), d(2026, 12, 31)), None, "整月");
        assert_eq!(days(oct, d(2026, 10, 10), oct.1), Some(21), "30 − 10 + 1");
        assert_eq!(days(oct, d(2026, 10, 30), oct.1), Some(1), "30 日加保");
        assert_eq!(
            days(oct, d(2026, 10, 31), oct.1),
            Some(1),
            "31 日加保同樣 1 日"
        );
        assert_eq!(
            days(oct, d(2026, 1, 1), d(2026, 10, 15)),
            Some(15),
            "15 日退保"
        );
        assert_eq!(
            days(oct, d(2026, 1, 1), d(2026, 10, 30)),
            None,
            "30 日退保已滿 30 日"
        );
        assert_eq!(
            days(oct, d(2026, 10, 5), d(2026, 10, 20)),
            Some(16),
            "同月到職又離職"
        );
        assert_eq!(
            days(oct, d(2026, 11, 2), d(2026, 12, 31)),
            Some(0),
            "下個月才到職"
        );
        let feb = (d(2026, 2, 1), d(2026, 2, 28));
        assert_eq!(days(feb, d(2026, 2, 10), feb.1), Some(21), "2/10 加保");
        assert_eq!(days(feb, d(2026, 2, 28), feb.1), Some(3), "2/28 加保");
        assert_eq!(days(feb, d(2025, 1, 1), feb.1), None, "2 月底退保是整月");
        assert_eq!(days(feb, d(2025, 1, 1), d(2026, 2, 27)), Some(27));
    }

    /// The part is taken before rounding, each share on its own, so half a
    /// month is not half of a rounded premium.
    #[test]
    fn a_part_of_a_month_is_taken_before_rounding() {
        let on = d(2026, 10, 31);
        let parts = resolve("partial_month", on).unwrap();
        let part = |key| {
            month_part(
                parts,
                key,
                d(2026, 10, 1),
                on,
                d(2026, 1, 1),
                d(2026, 10, 15),
            )
            .unwrap()
        };
        let labor = resolve("labor_insurance", on).unwrap();
        let c = labor_insurance(labor, dec!(45800), part("labor_days_per_month")).unwrap();
        assert_eq!(c.employee, dec!(504), "1,007.6 × 15/30 = 503.8");
        assert_eq!(c.employer, dec!(1763), "3,526.6 × 15/30 = 1,763.3");
        let pension_rs = resolve("pension", on).unwrap();
        assert_eq!(
            pension(pension_rs, dec!(53000), part("pension_days_per_month")).unwrap(),
            dec!(1590)
        );
    }

    #[test]
    fn a_month_counted_outside_a_calendar_month_is_refused() {
        let factory = resolve("partial_month", d(2026, 7, 25)).unwrap().clone();
        for bad in ["0", "27", "30.5", "32"] {
            let mut rs = factory.clone();
            rs.payload["labor_days_per_month"] = bad.into();
            let (start, end) = (d(2026, 10, 1), d(2026, 10, 31));
            assert!(
                matches!(
                    month_part(
                        &rs,
                        "labor_days_per_month",
                        start,
                        end,
                        start,
                        d(2026, 10, 15)
                    ),
                    Err(RegulationError::OutOfRange { .. })
                ),
                "{bad} 應被擋下"
            );
        }
    }

    #[test]
    fn salary_above_the_cap_is_charged_at_the_cap() {
        let rs = resolve("labor_insurance", d(2026, 7, 25)).unwrap();
        // The owner earns 120,000 a month; labour insurance still charges the
        // 45,800 ceiling.
        assert_eq!(
            labor_insurance(rs, dec!(45800), MonthPart::WHOLE).unwrap(),
            labor_insurance(rs, dec!(120000), MonthPart::WHOLE).unwrap()
        );
    }

    #[test]
    fn vat_is_five_percent() {
        let rs = resolve("vat", d(2026, 7, 25)).unwrap();
        assert_eq!(vat(rs, dec!(2486300)).unwrap(), dec!(124315));

        // A quoted tax-inclusive price splits back to the same pair.
        assert_eq!(vat_from_total(rs, dec!(2610615)).unwrap(), dec!(124315));
        assert_eq!(vat_from_total(rs, dec!(47250)).unwrap(), dec!(2250));
    }

    #[test]
    fn rounding_is_half_away_from_zero_not_bankers() {
        assert_eq!(to_dollars(dec!(3526.5)), dec!(3527));
        assert_eq!(to_dollars(dec!(2646.9366)), dec!(2647));
    }

    #[test]
    fn annual_leave_follows_the_statute() {
        let rs = resolve("annual_leave", d(2026, 7, 25)).unwrap();
        assert_eq!(annual_leave_days(rs, 3).unwrap(), 0, "未滿六個月沒有特休");
        assert_eq!(annual_leave_days(rs, 6).unwrap(), 3);
        assert_eq!(annual_leave_days(rs, 12).unwrap(), 7);
        assert_eq!(annual_leave_days(rs, 60).unwrap(), 15);
        assert_eq!(annual_leave_days(rs, 120).unwrap(), 16);
        assert_eq!(annual_leave_days(rs, 132).unwrap(), 17, "滿十一年加一日");
        assert_eq!(annual_leave_days(rs, 600).unwrap(), 30, "上限 30 日");
    }

    #[test]
    fn an_annual_leave_year_runs_from_one_threshold_to_the_next() {
        let rs = resolve("annual_leave", d(2026, 7, 25)).unwrap();
        assert_eq!(annual_leave_year(rs, 0).unwrap(), (0, 6), "未滿六個月");
        assert_eq!(annual_leave_year(rs, 5).unwrap(), (0, 6));
        assert_eq!(annual_leave_year(rs, 6).unwrap(), (6, 12), "六個月到一年");
        assert_eq!(annual_leave_year(rs, 11).unwrap(), (6, 12));
        assert_eq!(annual_leave_year(rs, 12).unwrap(), (12, 24));
        assert_eq!(annual_leave_year(rs, 25).unwrap(), (24, 36));
        assert_eq!(
            annual_leave_year(rs, 40).unwrap(),
            (36, 48),
            "三到五年每年一個年度"
        );
        assert_eq!(annual_leave_year(rs, 59).unwrap(), (48, 60));
        assert_eq!(annual_leave_year(rs, 133).unwrap(), (132, 144));
        for months in 0..240 {
            let (start, end) = annual_leave_year(rs, months).unwrap();
            assert_eq!(
                annual_leave_days(rs, months).unwrap(),
                annual_leave_days(rs, start).unwrap(),
                "年資 {months} 個月的額度要等於所在年度的額度"
            );
            assert!(start <= months && months < end);
        }
    }

    #[test]
    fn minimum_wage_flags_only_shortfalls() {
        let rs = resolve("minimum_wage", d(2026, 7, 25)).unwrap();
        assert_eq!(
            minimum_wage_shortfall(rs, dec!(45000), "monthly").unwrap(),
            None
        );
        assert_eq!(
            minimum_wage_shortfall(rs, dec!(28000), "monthly").unwrap(),
            Some(dec!(29500))
        );
    }

    /// An hourly rate measured against the monthly floor fails every time,
    /// and a monthly salary measured against the hourly floor never does.
    /// The basis is what makes either answer mean anything.
    #[test]
    fn the_minimum_wage_floor_follows_the_pay_basis() {
        let rs = resolve("minimum_wage", d(2026, 7, 25)).unwrap();
        assert_eq!(
            minimum_wage_shortfall(rs, dec!(200), "hourly").unwrap(),
            None
        );
        assert_eq!(
            minimum_wage_shortfall(rs, dec!(180), "hourly").unwrap(),
            Some(dec!(196))
        );
        assert_eq!(
            minimum_wage_shortfall(rs, dec!(200), "monthly").unwrap(),
            Some(dec!(29500)),
            "時薪拿去比月薪下限一定不合格"
        );
        assert!(matches!(
            minimum_wage_shortfall(rs, dec!(200), "daily"),
            Err(RegulationError::MissingField { .. })
        ));
    }

    /// An owner bears the whole health insurance premium and has no pension
    /// contributed for them. Both figures come from the ruleset, so the split
    /// changes without a release.
    #[test]
    fn an_owner_pays_the_whole_health_premium_and_gets_no_pension() {
        let health = resolve("health_insurance", d(2026, 7, 25)).unwrap();
        let owner = resolve("owner_insurance", d(2026, 7, 25)).unwrap();

        let deps = resolve("health_dependents", d(2026, 7, 25)).unwrap();
        let staff = health_insurance(health, deps, dec!(53000), 0).unwrap();
        let boss = health_insurance_owner(health, owner, deps, dec!(53000), 0).unwrap();
        assert_eq!(boss.employer, Money::ZERO, "雇主身分沒有公司負擔那一半");
        assert_eq!(boss.government, Money::ZERO, "政府不補助");
        assert_eq!(boss.employee, dec!(2740), "53000 × 5.17% 全額自付");
        assert_eq!(staff.employee, dec!(822), "受僱者只自付三成");

        assert_eq!(
            pension_owner(owner, dec!(53000), MonthPart::WHOLE).unwrap(),
            Money::ZERO
        );
        assert!(
            pension_owner(health, dec!(53000), MonthPart::WHOLE).is_err(),
            "領域要對得上"
        );
    }

    /// The employee pays a head for each covered dependant, up to the cap;
    /// the employer share is apportioned by the average and does not move.
    #[test]
    fn dependants_are_charged_per_head_up_to_the_cap() {
        let health = resolve("health_insurance", d(2026, 7, 25)).unwrap();
        let deps = resolve("health_dependents", d(2026, 7, 25)).unwrap();
        let one = health_insurance(health, deps, dec!(53000), 0).unwrap();
        assert_eq!(one.employee, dec!(822));

        for (covered, heads) in [(0, 1), (1, 2), (3, 4), (4, 4)] {
            let c = health_insurance(health, deps, dec!(53000), covered).unwrap();
            assert_eq!(
                c.employee,
                one.employee * Decimal::from(heads),
                "眷口 {covered} 應計 {heads} 口"
            );
            assert_eq!(c.employer, dec!(2647), "公司負擔仍依平均眷口數");
            assert_eq!(c.government, one.government * Decimal::from(heads));
        }
    }

    /// Each head is rounded on its own and then added up: 38,200 × 5.17% ×
    /// 30% is 592.482, so four heads are 4 × 592 = 2,368, where rounding the
    /// product once would give 2,370.
    #[test]
    fn each_head_is_rounded_before_the_heads_are_added() {
        let health = resolve("health_insurance", d(2026, 7, 25)).unwrap();
        let deps = resolve("health_dependents", d(2026, 7, 25)).unwrap();
        let c = health_insurance(health, deps, dec!(38200), 3).unwrap();
        assert_eq!(c.employee, dec!(2368));
    }

    /// The cap is read from the ruleset, and a cap that is not a whole number
    /// of people is refused rather than rounded.
    #[test]
    fn the_dependant_cap_comes_from_the_ruleset() {
        let health = resolve("health_insurance", d(2026, 7, 25)).unwrap();
        let deps = resolve("health_dependents", d(2026, 7, 25)).unwrap();
        let one = health_insurance(health, deps, dec!(53000), 0).unwrap();

        let mut two = deps.clone();
        two.payload["max_counted"] = "2".into();
        let c = health_insurance(health, &two, dec!(53000), 3).unwrap();
        assert_eq!(
            c.employee,
            one.employee * dec!(3),
            "上限 2 時眷口 3 只計 2 口"
        );

        for written in ["3.0", "3.00"] {
            let mut same = deps.clone();
            same.payload["max_counted"] = written.into();
            assert_eq!(dependants_charged(&same, 5).unwrap(), 3, "{written}");
        }
        for bad in ["2.5", "-1"] {
            let mut odd = deps.clone();
            odd.payload["max_counted"] = bad.into();
            assert!(
                matches!(
                    dependants_charged(&odd, 1),
                    Err(RegulationError::NotWholeNumber { .. })
                ),
                "{bad}"
            );
        }
        assert!(
            dependants_charged(health, 1).is_err(),
            "要 health_dependents 的規則"
        );
    }

    /// An owner bears each covered dependant's premium in full, as their own.
    #[test]
    fn an_owner_pays_for_each_dependant_too() {
        let health = resolve("health_insurance", d(2026, 7, 25)).unwrap();
        let owner = resolve("owner_insurance", d(2026, 7, 25)).unwrap();
        let deps = resolve("health_dependents", d(2026, 7, 25)).unwrap();
        let boss = health_insurance_owner(health, owner, deps, dec!(53000), 2).unwrap();
        assert_eq!(boss.employee, dec!(2740) * dec!(3));
        assert_eq!(boss.employer, Money::ZERO);
    }

    /// The cap has been law since the current act came into force; before
    /// that there is no version, and that is an error rather than no cap.
    #[test]
    fn a_date_before_the_dependant_rule_has_no_cap_to_guess() {
        assert!(resolve("health_dependents", d(2013, 1, 1)).is_ok());
        assert!(matches!(
            resolve("health_dependents", d(2012, 12, 31)),
            Err(RegulationError::NotFound { .. })
        ));
    }

    #[test]
    fn missing_period_is_an_error_never_a_guess() {
        // The system refuses to compute rather than computing someone's pay
        // from a guess.
        let err = resolve("labor_insurance", d(2025, 12, 31)).unwrap_err();
        assert!(matches!(err, RegulationError::NotFound { .. }));
        assert!(err.to_string().contains("新增涵蓋該日期的版本"));
    }

    #[test]
    fn a_calculator_refuses_a_ruleset_from_another_domain() {
        let vat_rs = resolve("vat", d(2026, 7, 25)).unwrap();
        assert!(matches!(
            labor_insurance(vat_rs, dec!(45800), MonthPart::WHOLE),
            Err(RegulationError::WrongDomain {
                expected: "labor_insurance",
                ..
            })
        ));
    }

    /// Rule substitutability, the reason this crate exists, guarded by a test.
    ///
    /// Simulate a higher labour insurance rate in 2027: after adding a rule
    /// with `effective_from = 2027-01-01`, an `as_of` in 2026 is completely
    /// unchanged and an `as_of` in 2027 picks up the new rate.
    #[test]
    fn a_later_ruleset_takes_over_without_touching_old_results() {
        let base = resolve("labor_insurance", d(2026, 7, 25)).unwrap();
        let before = labor_insurance(base, dec!(45800), MonthPart::WHOLE).unwrap();

        let mut next = base.clone();
        next.version = "2027.1".into();
        next.effective_from = d(2027, 1, 1);
        next.payload["premium_rate"] = serde_json::json!("0.12");
        let after = labor_insurance(&next, dec!(45800), MonthPart::WHOLE).unwrap();

        // Figures computed under the old rules do not move because a new rule
        // appeared.
        assert_eq!(before.employer, dec!(3527));
        assert_eq!(
            labor_insurance(base, dec!(45800), MonthPart::WHOLE)
                .unwrap()
                .employer,
            dec!(3527)
        );
        // New rule: 45800 x 0.12 x 0.70 = 3847.2, rounded to 3847.
        assert_eq!(after.employer, dec!(3847));
    }

    #[test]
    fn rulesets_expose_their_source_for_audit() {
        for rs in all() {
            assert!(!rs.source_note.is_empty(), "{} 沒有標註法源", rs.domain);
            assert!(rs.payload.is_object(), "{} 的規則內容不是物件", rs.domain);
        }
    }

    fn leave(date: NaiveDate, kind: &str, days: Decimal) -> LeaveDay {
        LeaveDay {
            date,
            kind: kind.into(),
            days,
            service_months: 12,
        }
    }

    /// Personal leave is unpaid, annual and bereavement leave paid in full,
    /// and sickness leave half paid until its yearly days run out.
    #[test]
    fn leave_pays_its_statutory_share_and_sickness_leave_runs_out() {
        let days = [
            leave(d(2026, 5, 4), "personal", dec!(1)),
            leave(d(2026, 5, 5), "annual", dec!(1)),
            leave(d(2026, 5, 6), "bereavement", dec!(0.5)),
            leave(d(2026, 5, 26), "sick", dec!(29.5)),
            leave(d(2026, 6, 1), "sick", dec!(1)),
            leave(d(2026, 6, 2), "sick", dec!(1)),
            leave(d(2027, 1, 4), "sick", dec!(1)),
        ];
        let paid = leave_paid_days(&days).unwrap();
        assert_eq!(paid[0], dec!(0), "事假不給薪");
        assert_eq!(paid[1], dec!(1), "特休照給");
        assert_eq!(paid[2], dec!(0.5), "喪假照給");
        assert_eq!(paid[3], dec!(14.75), "三十日內折半");
        assert_eq!(
            paid[4],
            dec!(0.25),
            "跨過第三十日的那一天，半天折半、半天不給"
        );
        assert_eq!(paid[5], dec!(0), "超過三十日不給");
        assert_eq!(paid[6], dec!(0.5), "新的一年重新起算");
    }

    /// Menstrual leave is half paid throughout; past its separate days it
    /// uses up sickness leave's yearly days.
    #[test]
    fn menstrual_leave_past_its_separate_days_counts_towards_sickness_leave() {
        let mut days: Vec<LeaveDay> = (1..=5)
            .map(|m| leave(d(2026, m, 5), "menstrual", dec!(1)))
            .collect();
        days.push(leave(d(2026, 6, 1), "sick", dec!(29)));
        days.push(leave(d(2026, 7, 1), "sick", dec!(1)));
        let paid = leave_paid_days(&days).unwrap();
        assert!(paid[..5].iter().all(|p| *p == dec!(0.5)), "生理假一律減半");
        assert_eq!(
            paid[5],
            dec!(14),
            "第 4、5 次生理假已用掉兩天病假額度，剩 28 天折半"
        );
        assert_eq!(paid[6], dec!(0), "病假額度已滿");
    }

    #[test]
    fn maternity_leave_is_half_paid_before_six_months_of_service() {
        let mut short = leave(d(2026, 5, 4), "maternity", dec!(1));
        short.service_months = 5;
        let long = leave(d(2026, 5, 4), "maternity", dec!(1));
        assert_eq!(leave_paid_days(&[short]).unwrap(), [dec!(0.5)]);
        assert_eq!(leave_paid_days(&[long]).unwrap(), [dec!(1)]);
    }

    #[test]
    fn a_leave_kind_the_ruleset_does_not_list_is_an_error() {
        assert!(leave_paid_days(&[leave(d(2026, 5, 4), "sabbatical", dec!(1))]).is_err());
    }

    /// A share is of one day's wage: outside 0 to 1 payroll would take more
    /// than a day or pay for more hours than were on leave.
    #[test]
    fn a_leave_share_outside_zero_to_one_is_refused() {
        let factory = seed()
            .into_iter()
            .find(|r| r.domain == "leave_pay")
            .expect("出廠版本應有請假工資");
        for bad in ["1.5", "-0.5"] {
            let mut rs = factory.clone();
            rs.payload["sick"]["ratio"] = bad.into();
            assert!(
                matches!(
                    leave_pay(&rs, "sick"),
                    Err(RegulationError::OutOfRange { .. })
                ),
                "比例 {bad} 應被擋下"
            );
        }
        let mut negative = factory.clone();
        negative.payload["sick"]["yearly_days"] = "-1".into();
        assert!(leave_pay(&negative, "sick").is_err(), "日數不能是負的");
        let mut full = factory;
        full.payload["sick"]["ratio"] = "1".into();
        assert_eq!(
            leave_pay(&full, "sick").unwrap().ratio,
            dec!(1),
            "病假全薪在範圍內"
        );
    }
}
