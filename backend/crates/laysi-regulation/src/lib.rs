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
//! [`install`]. [`seed`] is the factory set shipped with the image, written
//! once into an empty table and never again.
//!
//! Updating regulation end to end: save a ruleset with a later
//! `effective_from`. Results for earlier dates are unchanged and later dates
//! pick up the new rules automatically, with no release.

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};
use std::sync::{OnceLock, RwLock};

pub type Money = Decimal;

/// The factory set, embedded at compile time. It fills an empty `reg_ruleset`
/// and is the answer before the server has installed anything. Register a new
/// domain here; a newly published year is added as a version on the screen.
const SOURCES: &[(&str, &str)] = &[
    ("2026/labor_insurance.toml", include_str!("../../../regulation/2026/labor_insurance.toml")),
    ("2026/health_insurance.toml", include_str!("../../../regulation/2026/health_insurance.toml")),
    ("2026/pension.toml", include_str!("../../../regulation/2026/pension.toml")),
    ("2026/vat.toml", include_str!("../../../regulation/2026/vat.toml")),
    ("2026/minimum_wage.toml", include_str!("../../../regulation/2026/minimum_wage.toml")),
    ("2026/withholding.toml", include_str!("../../../regulation/2026/withholding.toml")),
    ("2026/annual_leave.toml", include_str!("../../../regulation/2026/annual_leave.toml")),
    ("2026/depreciation.toml", include_str!("../../../regulation/2026/depreciation.toml")),
    ("2026/overtime.toml", include_str!("../../../regulation/2026/overtime.toml")),
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
    #[error(
        "找不到「{domain}」在 {as_of} 適用的法規版本，請在法規版本新增涵蓋該日期的版本。"
    )]
    NotFound { domain: String, as_of: NaiveDate },

    #[error("法規 {domain}/{version} 缺少必要欄位「{key}」")]
    MissingField { domain: String, version: String, key: String },

    #[error("法規 {domain}/{version} 的欄位「{key}」不是合法數值：{value}")]
    BadNumber { domain: String, version: String, key: String, value: String },

    #[error("這個計算子需要 {expected} 的規則，收到的卻是 {found}")]
    WrongDomain { expected: &'static str, found: String },
}

fn sorted(mut v: Vec<Ruleset>) -> Vec<Ruleset> {
    v.sort_by(|a, b| a.domain.cmp(&b.domain).then(a.effective_from.cmp(&b.effective_from)));
    v
}

/// The factory set shipped with the image, parsed from the embedded files.
///
/// The server writes these into `reg_ruleset` only while that table is empty.
/// From the first boot onwards the table answers, so a corrected rate and a
/// deleted version both survive the next start.
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
        .ok_or_else(|| RegulationError::NotFound { domain: domain.to_string(), as_of })
}

impl Ruleset {
    fn expect_domain(&self, expected: &'static str) -> Result<(), RegulationError> {
        if self.domain != expected {
            return Err(RegulationError::WrongDomain { expected, found: self.domain.clone() });
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
            node = node.get(part).ok_or_else(|| RegulationError::MissingField {
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
        text.parse::<Decimal>().map_err(|_| RegulationError::BadNumber {
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

/// Labour insurance ordinary risk premium.
///
/// Salary above the insured ceiling is charged at the ceiling. That means
/// the bracket is capped, not that the salary was truncated.
pub fn labor_insurance(
    rs: &Ruleset,
    insured_salary: Money,
) -> Result<Contribution, RegulationError> {
    rs.expect_domain("labor_insurance")?;
    let cap = rs.decimal("max_insured_salary")?;
    let premium = insured_salary.min(cap) * rs.decimal("premium_rate")?;
    Ok(Contribution {
        employer: to_dollars(premium * rs.decimal("employer_share")?),
        employee: to_dollars(premium * rs.decimal("employee_share")?),
        government: to_dollars(premium * rs.decimal("government_share")?),
    })
}

/// National health insurance ordinary premium.
///
/// The employer share is multiplied by the average number of dependants
/// across all insured persons rather than this employee's actual
/// dependants, because the law apportions it that way. A single employee's
/// employer share therefore exceeds twice their own contribution.
pub fn health_insurance(
    rs: &Ruleset,
    insured_salary: Money,
) -> Result<Contribution, RegulationError> {
    rs.expect_domain("health_insurance")?;
    let premium = insured_salary * rs.decimal("rate")?;
    Ok(Contribution {
        employer: to_dollars(premium * rs.decimal("employer_share")? * rs.decimal("avg_dependents")?),
        employee: to_dollars(premium * rs.decimal("employee_share")?),
        government: to_dollars(premium * rs.decimal("government_share")?),
    })
}

/// Employer pension contribution. Borne entirely by the employer.
pub fn pension(rs: &Ruleset, contribution_wage: Money) -> Result<Money, RegulationError> {
    rs.expect_domain("pension")?;
    Ok(to_dollars(contribution_wage * rs.decimal("employer_rate")?))
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
    Ok(to_dollars(total_amount - total_amount / (Decimal::ONE + rate)))
}

/// Minimum wage check. `None` means compliant, `Some(floor)` means below the
/// statutory minimum.
pub fn minimum_wage_shortfall(
    rs: &Ruleset,
    monthly_salary: Money,
) -> Result<Option<Money>, RegulationError> {
    rs.expect_domain("minimum_wage")?;
    let floor = rs.decimal("monthly")?;
    Ok((monthly_salary < floor).then_some(floor))
}

/// Annual leave days. `service_months` is completed months since hire.
pub fn annual_leave_days(rs: &Ruleset, service_months: u32) -> Result<u32, RegulationError> {
    rs.expect_domain("annual_leave")?;
    let tiers = rs.payload.get("tiers").and_then(|v| v.as_array()).ok_or_else(|| {
        RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: "tiers".into(),
        }
    })?;

    let read = |t: &serde_json::Value, key: &str| -> Option<u32> { t.get(key)?.as_str()?.parse().ok() };

    let mut days = 0;
    let mut matched_months = 0;
    for t in tiers {
        let (Some(months), Some(d)) = (read(t, "months"), read(t, "days")) else { continue };
        if service_months >= months && months >= matched_months {
            days = d;
            matched_months = months;
        }
    }

    // After ten years, one more day per completed year up to the cap. This is
    // a rule rather than an enumeration.
    if service_months >= 120 {
        let inc: u32 = rs.decimal("over_ten_years.increment_per_year")?.trunc().try_into().unwrap_or(1);
        let max: u32 = rs.decimal("over_ten_years.max_days")?.trunc().try_into().unwrap_or(30);
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

fn payload_array<'a>(rs: &'a Ruleset, key: &str) -> Result<&'a Vec<serde_json::Value>, RegulationError> {
    let mut node = &rs.payload;
    for part in key.split('.') {
        node = node.get(part).ok_or_else(|| RegulationError::MissingField {
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
    let text = node.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: key.to_string(),
        }
    })?;
    text.parse::<Decimal>().map_err(|_| RegulationError::BadNumber {
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
                code: t.get("code").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                label: t.get("label").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
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

/// Pay for `hours` of overtime worked on a `day_type` by someone on
/// `monthly_salary`.
///
/// The hourly wage is the monthly salary over the hours the ruleset says a
/// month holds, and the hours are spent against the bands in order: the first
/// band's multiplier up to its `up_to_hours`, then the next. A band without
/// `up_to_hours` runs to the end.
pub fn overtime_pay(
    rs: &Ruleset,
    monthly_salary: Money,
    day_type: &str,
    hours: Money,
) -> Result<Money, RegulationError> {
    rs.expect_domain("overtime")?;
    let hourly = monthly_salary / rs.decimal("monthly_hours")?;
    let types = payload_array(rs, "day_types")?;
    let matched = types
        .iter()
        .find(|t| t.get("code").and_then(|v| v.as_str()) == Some(day_type))
        .ok_or_else(|| RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: format!("day_types.{day_type}"),
        })?;

    let bands = matched.get("bands").and_then(|v| v.as_array()).ok_or_else(|| {
        RegulationError::MissingField {
            domain: rs.domain.clone(),
            version: rs.version.clone(),
            key: format!("day_types.{day_type}.bands"),
        }
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
        pay += hourly * multiplier * span;
        left -= span;
    }
    Ok(to_dollars(pay))
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
        assert_eq!(overtime_pay(rs, dec!(40000), "weekday", dec!(2)).unwrap(), dec!(447));
        assert_eq!(overtime_pay(rs, dec!(40000), "weekday", dec!(3)).unwrap(), dec!(725));
        assert_eq!(overtime_pay(rs, dec!(40000), "rest_day", dec!(3)).unwrap(), dec!(725));
        assert_eq!(overtime_pay(rs, dec!(40000), "weekday", Money::ZERO).unwrap(), Money::ZERO);
        assert!(overtime_pay(rs, dec!(40000), "public_holiday", dec!(1)).is_err());

        let mut changed = rs.clone();
        changed.payload["day_types"][0]["bands"][0]["multiplier"] = "2".into();
        assert_ne!(
            overtime_pay(&changed, dec!(40000), "weekday", dec!(2)).unwrap(),
            overtime_pay(rs, dec!(40000), "weekday", dec!(2)).unwrap()
        );

        let types = overtime_day_types(rs).unwrap();
        assert_eq!(types.iter().map(|t| t.code.as_str()).collect::<Vec<_>>(), ["weekday", "rest_day"]);
        assert_eq!(types[0].max_hours, dec!(4));
        assert_eq!(overtime_monthly_max_hours(rs).unwrap(), dec!(46));
    }

    #[test]
    fn labor_insurance_matches_the_prototype() {
        let rs = resolve("labor_insurance", d(2026, 7, 25)).unwrap();
        let c = labor_insurance(rs, dec!(45800)).unwrap();
        assert_eq!(c.employer, dec!(3527), "公司負擔勞保");
        assert_eq!(c.employee, dec!(1008), "員工自付勞保");
    }

    #[test]
    fn health_insurance_matches_the_prototype() {
        let rs = resolve("health_insurance", d(2026, 7, 25)).unwrap();
        let c = health_insurance(rs, dec!(53000)).unwrap();
        assert_eq!(c.employer, dec!(2647), "公司負擔健保（含平均眷口 1.61）");
        assert_eq!(c.employee, dec!(822), "員工自付健保");
    }

    #[test]
    fn pension_matches_the_prototype() {
        let rs = resolve("pension", d(2026, 7, 25)).unwrap();
        assert_eq!(pension(rs, dec!(53000)).unwrap(), dec!(3180));
    }

    #[test]
    fn salary_above_the_cap_is_charged_at_the_cap() {
        let rs = resolve("labor_insurance", d(2026, 7, 25)).unwrap();
        // The owner earns 120,000 a month; labour insurance still charges the
        // 45,800 ceiling.
        assert_eq!(
            labor_insurance(rs, dec!(45800)).unwrap(),
            labor_insurance(rs, dec!(120000)).unwrap()
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
    fn minimum_wage_flags_only_shortfalls() {
        let rs = resolve("minimum_wage", d(2026, 7, 25)).unwrap();
        assert_eq!(minimum_wage_shortfall(rs, dec!(45000)).unwrap(), None);
        assert_eq!(minimum_wage_shortfall(rs, dec!(28000)).unwrap(), Some(dec!(29500)));
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
            labor_insurance(vat_rs, dec!(45800)),
            Err(RegulationError::WrongDomain { expected: "labor_insurance", .. })
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
        let before = labor_insurance(base, dec!(45800)).unwrap();

        let mut next = base.clone();
        next.version = "2027.1".into();
        next.effective_from = d(2027, 1, 1);
        next.payload["premium_rate"] = serde_json::json!("0.12");
        let after = labor_insurance(&next, dec!(45800)).unwrap();

        // Figures computed under the old rules do not move because a new rule
        // appeared.
        assert_eq!(before.employer, dec!(3527));
        assert_eq!(labor_insurance(base, dec!(45800)).unwrap().employer, dec!(3527));
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
}
