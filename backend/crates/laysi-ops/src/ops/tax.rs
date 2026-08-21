//! Tax: the VAT (form 401) working paper and withholding on various income.
//!
//! Every figure on the return comes from the ledger, not from summing source
//! documents. Output sales read 4111, output tax reads 2211, input tax reads
//! 1284 — all period movements of GL accounts, counting posted vouchers only.
//! Summing the receivable and payable subsidiaries instead would create a
//! second set of output figures, and the two would eventually disagree with no
//! way to tell which is right. [`GetVatReturn`] therefore computes both "net
//! sales x rate" and "period movement of 2211" and returns them together;
//! agreement is a conclusion, not an assumption.
//!
//! Unposted vouchers are reported in a separate column. The return cannot
//! include them because the ledger has not moved, but the screen still needs
//! to show how much is waiting to be posted.
//!
//! The filing period length is regulation too: `period_months` in `vat.toml`,
//! not a constant here.
//!
//! Withholding rates and thresholds come from `payload.codes` of the
//! `withholding` ruleset, and the second-generation health insurance surcharge
//! from `payload.supplementary_health`. Salary is the exception: the Income
//! Tax Act allows the withholding table to be applied per person per bracket,
//! which is a table rather than a rate and belongs to the payroll module.
//! Those records are marked `basis = 'table'` and carry the tax computed by
//! the source system.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_regulation::Ruleset;
use rust_decimal::{Decimal, RoundingStrategy};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(GetVatReturn), Box::new(ListWithholding), Box::new(ComputeWithholding)]
}

/// A document amount split into its net and tax halves.
///
/// Receivables and payables both need this: the return reads the period
/// movement of 2211 and 1284, so a document that never splits its tax out
/// leaves the return short by exactly that amount.
pub(crate) struct VatSplit {
    pub net: Money,
    pub tax: Money,
    pub total: Money,
    pub version: String,
}

/// Complete a net / tax / total triple against the rules in force on `on`.
///
/// Any one or two of the three may be given; the rest follow. The rate itself
/// is never named here — a caller that gives only one figure gets the missing
/// two from the ruleset resolved on the document date, and a caller that gives
/// two gets the third by subtraction, so a zero-rated export is expressed by
/// sending a zero tax rather than by a flag.
pub(crate) fn split_vat(
    on: NaiveDate,
    net: Option<Money>,
    tax: Option<Money>,
    total: Option<Money>,
) -> Result<VatSplit, OpError> {
    let rs = laysi_regulation::resolve("vat", on)?;
    let (net, tax, total) = match (net, tax, total) {
        (Some(net), Some(tax), Some(total)) => {
            if net + tax != total {
                return Err(OpError::BadInput(format!(
                    "未稅 {} 加營業稅 {} 應等於含稅 {}",
                    money(net),
                    money(tax),
                    money(total)
                )));
            }
            (net, tax, total)
        }
        (Some(net), Some(tax), None) => (net, tax, net + tax),
        (Some(net), None, Some(total)) => (net, total - net, total),
        (None, Some(tax), Some(total)) => (total - tax, tax, total),
        (Some(net), None, None) => {
            let tax = laysi_regulation::vat(rs, net)?;
            (net, tax, net + tax)
        }
        (None, None, Some(total)) => {
            let tax = laysi_regulation::vat_from_total(rs, total)?;
            (total - tax, tax, total)
        }
        (None, Some(_), None) | (None, None, None) => {
            return Err(OpError::BadInput("請填未稅金額或含稅金額".into()))
        }
    };
    if total <= Money::ZERO {
        return Err(OpError::BadInput("含稅金額必須大於零".into()));
    }
    if net <= Money::ZERO {
        return Err(OpError::BadInput("未稅金額必須大於零".into()));
    }
    if tax < Money::ZERO {
        return Err(OpError::BadInput("營業稅不能是負數；零稅率與免稅填 0".into()));
    }
    Ok(VatSplit { net, tax, total, version: rs.version.clone() })
}

/// Round to whole dollars. Tax rounds half away from zero, not banker's
/// rounding.
///
/// This is a rounding mode, not a regulated rate; no rate lives in this file.
fn to_dollars(v: Money) -> Money {
    v.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

/// ROC year. Calendar conversion, not a regulation value.
fn roc_year(year: i32) -> i32 {
    year - 1911
}

/// Render a rate as a percentage string (`0.05` becomes `5`) for labels.
///
/// Derived from the rate rather than written out as "5%", so the label
/// follows a rate change instead of contradicting the calculation.
pub(crate) fn percent(rate: Money) -> String {
    (rate * Decimal::ONE_HUNDRED).normalize().to_string()
}

/// The VAT rules a document created now would use, as JSON for a screen.
///
/// The rate has one source, `vat.toml`. A screen that needs to fill in the
/// third of net / tax / total gets it from here rather than carrying a copy.
pub(crate) fn vat_json(on: NaiveDate) -> Result<Value, OpError> {
    let rs = laysi_regulation::resolve("vat", on)?;
    let rate = rs.decimal("rate")?;
    Ok(json!({
        "version": rs.version,
        "rate": rate.to_string(),
        "rate_percent": percent(rate),
        "effective_from": rs.effective_from,
        "source_note": rs.source_note,
    }))
}

/// Which filing period a date falls in. The period length comes from the
/// rules, not from here.
///
/// Returns (period start, period end, display label).
fn vat_period(as_of: NaiveDate, period_months: u32) -> Option<(NaiveDate, NaiveDate, String)> {
    let months = period_months.max(1);
    let index = (as_of.month() - 1) / months;
    let first = index * months + 1;
    let last = (first + months - 1).min(12);

    let from = NaiveDate::from_ymd_opt(as_of.year(), first, 1)?;
    // Period end is the day before the first of the following month, carrying
    // the year when it wraps.
    let (ny, nm) = if last == 12 { (as_of.year() + 1, 1) } else { (as_of.year(), last + 1) };
    let to = NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()?;

    let label = if first == last {
        format!("{} 年 {first} 月期", roc_year(as_of.year()))
    } else {
        format!("{} 年 {first}-{last} 月期", roc_year(as_of.year()))
    };
    Some((from, to, label))
}

struct GetVatReturn;

#[async_trait]
impl Operation for GetVatReturn {
    fn name(&self) -> &'static str {
        "get_vat_return"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "試算 401 營業稅申報：銷項、進項、上期留抵與本期應納稅額，全部由已過帳傳票推導，並與 2211 代收營業稅對帳"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::AsOfInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: crate::AsOfInput = parse(input)?;
        let as_of = i.as_of.unwrap_or(ctx.today);

        // Both the rate and the period length come from the rules. With no
        // applicable version the working paper refuses to compute: filing a
        // return from a guessed rate is worse than not computing one.
        let rs = laysi_regulation::resolve("vat", as_of)?;
        let rate = rs.decimal("rate")?;
        let period_months: u32 =
            rs.decimal("period_months")?.trunc().try_into().unwrap_or(1);
        let (from, to, label) = vat_period(as_of, period_months)
            .ok_or_else(|| OpError::BadInput(format!("{as_of} 無法推算申報期別")))?;

        let f = ctx.store.vat_figures(from, to).await?;

        // The creditable input base is derived back from the input tax:
        // exempt purchases never reach 1284, so the derivation yields exactly
        // the creditable portion.
        let input_base =
            if rate.is_zero() { Money::ZERO } else { to_dollars(f.input_tax / rate) };

        // What output tax should be (4111 times the rate) against what the
        // books actually carry in 2211. Computed separately, difference
        // reported as found.
        let expected_output = to_dollars(f.sales * rate);
        let output_gap = expected_output - f.output_tax;

        // Output above input is payable, the reverse is a credit carried
        // forward. Neither is ever negative.
        let net = f.output_tax - f.input_tax - f.prior_credit;
        let payable = if net > Money::ZERO { net } else { Money::ZERO };
        let carry_forward = if net < Money::ZERO { -net } else { Money::ZERO };

        // The six rows of the return. `kind` sets the level: sub for tax
        // amounts, total for conclusions.
        let rows = json!([
            { "label": "銷項金額（應稅）",   "value": f.sales.to_string(),        "kind": "" },
            { "label": "銷項稅額",           "value": f.output_tax.to_string(),   "kind": "sub" },
            { "label": "進項金額",           "value": input_base.to_string(),     "kind": "" },
            { "label": "進項稅額（可扣抵）", "value": f.input_tax.to_string(),    "kind": "sub" },
            { "label": "上期累積留抵稅額",   "value": f.prior_credit.to_string(), "kind": "" },
            { "label": "本期應納稅額",       "value": payable.to_string(),        "kind": "total" },
        ]);

        let summary = format!(
            "試算 401 · {label} · 銷項稅額 {} 進項稅額 {} 應納 {} · 依 vat {}",
            money(f.output_tax),
            money(f.input_tax),
            money(payable),
            rs.version
        );
        Ok(OpOutcome::new(
            json!({
                "period_label": label,
                "period_from": from,
                "period_to": to,
                "period_months": period_months,
                "rows": rows,
                "sales": f.sales.to_string(),
                "output_tax": f.output_tax.to_string(),
                "input_base": input_base.to_string(),
                "input_tax": f.input_tax.to_string(),
                "prior_credit": f.prior_credit.to_string(),
                "payable": payable.to_string(),
                "carry_forward": carry_forward.to_string(),
                // Reconciliation: net sales times the rate should equal the
                // period movement of 2211.
                "expected_output_tax": expected_output.to_string(),
                "output_tax_gap": output_gap.to_string(),
                "reconciled": output_gap.is_zero(),
                // Unposted vouchers: the ledger has not moved, so the return
                // cannot count them, but they must be visible.
                "pending_sales": f.pending_sales.to_string(),
                "pending_output_tax": f.pending_output_tax.to_string(),
                "pending_input_tax": f.pending_input_tax.to_string(),
                "pending_vouchers": f.pending_vouchers,
                "ruleset": {
                    "domain": rs.domain,
                    "version": rs.version,
                    "rate": rate.to_string(),
                    "rate_percent": percent(rate),
                    "effective_from": rs.effective_from,
                    "source_note": rs.source_note,
                },
            }),
            summary,
        ))
    }
}

// Withholding on various income

/// The rule for one income code under one version of the withholding
/// standard.
#[derive(Debug)]
pub(crate) struct CodeRule {
    pub(crate) name: String,
    pub(crate) rate: Money,
    threshold: Money,
    note: String,
}

/// Find an income code in the `payload.codes` array.
///
/// `Ruleset::decimal` walks object paths, so an array is searched here. The
/// value still comes only from the payload; this function does not know
/// what any rate looks like.
pub(crate) fn code_rule(rs: &Ruleset, code: &str) -> Result<CodeRule, OpError> {
    let read = |v: &Value, key: &str| -> Option<Money> {
        Decimal::from_str_exact(v.get(key)?.as_str()?).ok()
    };
    let found = rs
        .payload
        .get("codes")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.iter().find(|c| c.get("code").and_then(|v| v.as_str()) == Some(code)))
        .ok_or_else(|| {
            OpError::BadInput(format!(
                "扣繳率標準 {} 未收錄所得格式代號「{code}」。可用代號請查詢 list_regulations。",
                rs.version
            ))
        })?;

    Ok(CodeRule {
        name: found.get("name").and_then(|v| v.as_str()).unwrap_or(code).to_string(),
        rate: read(found, "rate").ok_or_else(|| {
            OpError::BadInput(format!("扣繳率標準 {} 的代號 {code} 未設定 rate", rs.version))
        })?,
        threshold: read(found, "threshold").unwrap_or(Money::ZERO),
        note: found.get("note").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
    })
}

/// How much to withhold from one payment.
///
/// The threshold decides whether to withhold at all, and is not an
/// allowance: below it the payment is exempt entirely, at or above it the
/// full amount is subject. Both numbers come from the rules.
pub(crate) fn withhold(rule: &CodeRule, gross: Money) -> Money {
    if gross < rule.threshold {
        Money::ZERO
    } else {
        to_dollars(gross * rule.rate)
    }
}

/// Second-generation health insurance supplementary premium. `is_base`
/// classifies the payment (whether it falls in the six chargeable
/// categories) and is decided by whoever raises it; the rate and threshold
/// come from the rules.
fn supplementary(rs: &Ruleset, gross: Money, is_base: bool) -> Result<Money, OpError> {
    if !is_base {
        return Ok(Money::ZERO);
    }
    let threshold = rs.decimal("supplementary_health.threshold")?;
    if gross < threshold {
        return Ok(Money::ZERO);
    }
    Ok(to_dollars(gross * rs.decimal("supplementary_health.rate")?))
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListWithholdingInput {
    /// Payment month (YYYY-MM). Defaults to this month; the rate resolves on
    /// the payment date.
    #[serde(default)]
    pub month: Option<String>,
}

/// Split `YYYY-MM` into the first and last day of that month.
fn month_bounds(month: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (y, m) = month.split_once('-')?;
    let (y, m): (i32, u32) = (y.parse().ok()?, m.parse().ok()?);
    let from = NaiveDate::from_ymd_opt(y, m, 1)?;
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Some((from, NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()?))
}

struct ListWithholding;

#[async_trait]
impl Operation for ListWithholding {
    fn name(&self) -> &'static str {
        "list_withholding"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀各類所得扣繳明細：給付對象、所得格式代號、給付金額與應扣繳稅款，扣繳率與二代健保補充保費依給付日適用的規則計算"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListWithholdingInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListWithholdingInput = parse(input)?;
        let month = i.month.unwrap_or_else(|| ctx.today.format("%Y-%m").to_string());
        let (from, to) = month_bounds(&month)
            .ok_or_else(|| OpError::BadInput(format!("月份格式請用 YYYY-MM，收到「{month}」")))?;

        let records = ctx.store.withholding_records(from, to).await?;
        let mut rows = Vec::with_capacity(records.len());
        let mut tax_total = Money::ZERO;
        let mut supp_total = Money::ZERO;
        let mut versions: Vec<String> = Vec::new();

        for r in &records {
            let rs = laysi_regulation::resolve("withholding", r.paid_on)?;
            let rule = code_rule(rs, &r.income_code)?;

            // basis = table: the tax was computed by the payroll module from the
            // withholding table and is carried through, not recomputed.
            let (tax, basis_label) = match r.basis.as_str() {
                "table" => (r.withheld_amount.unwrap_or(Money::ZERO), "依薪資所得扣繳稅額表"),
                _ => (withhold(&rule, r.gross_amount), "依扣繳率標準"),
            };
            let supp = supplementary(rs, r.gross_amount, r.supplementary_base)?;
            tax_total += tax;
            supp_total += supp;
            if !versions.contains(&rs.version) {
                versions.push(rs.version.clone());
            }

            rows.push(json!({
                "paid_on": r.paid_on,
                "payee": r.payee,
                "payee_id": r.payee_id,
                "income_code": r.income_code,
                "income_name": rule.name,
                "code_label": format!("{} {}", r.income_code, rule.name),
                "gross_amount": r.gross_amount.to_string(),
                "withheld_amount": tax.to_string(),
                "supplementary_amount": supp.to_string(),
                "rate": rule.rate.to_string(),
                "threshold": rule.threshold.to_string(),
                "below_threshold": r.basis == "rate" && r.gross_amount < rule.threshold,
                "basis": r.basis,
                "basis_label": basis_label,
                "note": rule.note,
                "memo": r.memo,
                "voucher_no": r.voucher_no,
                "ruleset_version": rs.version,
            }));
        }

        let summary = format!(
            "查詢 {month} 各類所得扣繳 · {} 筆 · 扣繳稅款 {} · 補充保費 {} · 依扣繳規則 {}",
            rows.len(),
            money(tax_total),
            money(supp_total),
            versions.join("、")
        );
        Ok(OpOutcome::new(
            json!({
                "month": month,
                "rows": rows,
                "tax_total": tax_total.to_string(),
                "supplementary_total": supp_total.to_string(),
                "ruleset_versions": versions,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct ComputeWithholdingInput {
    /// Income code, e.g. 50 salary, 9A professional services, 9B royalties,
    /// 51 rent.
    pub income_code: String,
    /// Gross amount of this payment, before withholding. A string in JSON.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub gross_amount: Money,
    /// Payment date (YYYY-MM-DD). Defaults to today; the rate resolves on it.
    #[serde(default)]
    pub paid_on: Option<NaiveDate>,
    /// Whether this payment falls in a chargeable category for the
    /// supplementary health premium. Regular salary does not.
    #[serde(default)]
    pub supplementary_base: bool,
}

struct ComputeWithholding;

#[async_trait]
impl Operation for ComputeWithholding {
    fn name(&self) -> &'static str {
        "compute_withholding"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "試算一筆給付要扣繳多少稅與二代健保補充保費，並回報用的是哪一版扣繳率標準與起扣點"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ComputeWithholdingInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ComputeWithholdingInput = parse(input)?;
        if i.gross_amount <= Money::ZERO {
            return Err(OpError::BadInput("給付金額要大於零".into()));
        }
        let paid_on = i.paid_on.unwrap_or(ctx.today);
        let rs = laysi_regulation::resolve("withholding", paid_on)?;
        let rule = code_rule(rs, &i.income_code)?;

        let tax = withhold(&rule, i.gross_amount);
        let supp = supplementary(rs, i.gross_amount, i.supplementary_base)?;
        let net = i.gross_amount - tax - supp;
        let below = i.gross_amount < rule.threshold;

        let summary = format!(
            "試算扣繳 · {} {} 給付 {} → 扣繳 {}{} · 依扣繳規則 {}",
            i.income_code,
            rule.name,
            money(i.gross_amount),
            money(tax),
            if supp.is_zero() { String::new() } else { format!("、補充保費 {}", money(supp)) },
            rs.version
        );
        Ok(OpOutcome::new(
            json!({
                "income_code": i.income_code,
                "income_name": rule.name,
                "paid_on": paid_on,
                "gross_amount": i.gross_amount.to_string(),
                "withheld_amount": tax.to_string(),
                "supplementary_amount": supp.to_string(),
                "net_payment": net.to_string(),
                "rate": rule.rate.to_string(),
                "threshold": rule.threshold.to_string(),
                "below_threshold": below,
                "note": rule.note,
                "ruleset": {
                    "domain": rs.domain,
                    "version": rs.version,
                    "effective_from": rs.effective_from,
                    "source_note": rs.source_note,
                },
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

    fn wht(as_of: NaiveDate) -> &'static Ruleset {
        laysi_regulation::resolve("withholding", as_of).unwrap()
    }

    /// The period length comes from the rules. On a two-month cycle July
    /// falls in the July-August period.
    #[test]
    fn the_filing_period_follows_the_ruleset_length() {
        let (from, to, label) = vat_period(d(2026, 7, 25), 2).unwrap();
        assert_eq!(from, d(2026, 7, 1));
        assert_eq!(to, d(2026, 8, 31));
        assert_eq!(label, "115 年 7-8 月期");

        // May falls in the previous period.
        assert_eq!(vat_period(d(2026, 6, 30), 2).unwrap().2, "115 年 5-6 月期");
        // A November period end crosses into December, not next January.
        assert_eq!(vat_period(d(2026, 11, 3), 2).unwrap().1, d(2026, 12, 31));

        // Switching to monthly filing means changing period_months in the TOML.
        assert_eq!(vat_period(d(2026, 7, 25), 1).unwrap().2, "115 年 7 月期");
        assert_eq!(vat_period(d(2026, 7, 25), 1).unwrap().1, d(2026, 7, 31));
    }

    /// Both the rate and the threshold come from the rules: change the rules
    /// and the same payment gives a different answer.
    #[test]
    fn withholding_rates_come_from_the_ruleset_not_from_this_file() {
        let rs = wht(d(2026, 7, 10));
        let r9a = code_rule(rs, "9A").unwrap();
        assert_eq!(withhold(&r9a, n("60000")), n("6000"), "執行業務 60,000");

        let r51 = code_rule(rs, "51").unwrap();
        assert_eq!(withhold(&r51, n("52000")), n("5200"), "租賃 52,000");

        // Simulate the professional services rate rising to 12% in 2027: the
        // same payment gives a different answer without a change to this file.
        let mut next = rs.clone();
        next.version = "2027.1".into();
        next.effective_from = d(2027, 1, 1);
        next.payload["codes"][1]["rate"] = json!("0.12");
        assert_eq!(code_rule(&next, "9A").unwrap().rate, n("0.12"));
        assert_eq!(withhold(&code_rule(&next, "9A").unwrap(), n("60000")), n("7200"));
        // The old rules are unaffected.
        assert_eq!(withhold(&r9a, n("60000")), n("6000"));
    }

    /// The threshold decides whether to withhold: below it nothing, at or
    /// above it the full amount, with no brackets.
    #[test]
    fn the_threshold_decides_whether_to_withhold_at_all() {
        let rs = wht(d(2026, 7, 15));
        let r9b = code_rule(rs, "9B").unwrap();
        assert_eq!(withhold(&r9b, n("18000")), Money::ZERO, "未達起扣點免予扣繳");
        assert_eq!(withhold(&r9b, r9b.threshold), to_dollars(r9b.threshold * r9b.rate));

        // The salary threshold is 0, so any amount is subject. That also comes
        // from the rules, it is not a special case.
        let r50 = code_rule(rs, "50").unwrap();
        assert!(r50.threshold.is_zero());
    }

    /// The supplementary premium applies only to chargeable categories, and
    /// has a threshold of its own.
    #[test]
    fn supplementary_premium_needs_both_a_category_and_a_threshold() {
        let rs = wht(d(2026, 7, 10));
        // Professional services 60,000: chargeable and above the threshold.
        assert_eq!(supplementary(rs, n("60000"), true).unwrap(), n("1266"));
        // The same amount as regular salary is not chargeable.
        assert_eq!(supplementary(rs, n("60000"), false).unwrap(), Money::ZERO);
        // Chargeable but below the threshold is not charged either.
        assert_eq!(supplementary(rs, n("18000"), true).unwrap(), Money::ZERO);
    }

    #[test]
    fn an_unknown_income_code_is_refused_not_guessed() {
        let rs = wht(d(2026, 7, 10));
        let err = code_rule(rs, "99X").unwrap_err();
        assert!(matches!(err, OpError::BadInput(_)));
        assert!(err.to_string().contains("99X"));
    }

    #[test]
    fn months_parse_into_their_own_bounds() {
        assert_eq!(month_bounds("2026-07").unwrap(), (d(2026, 7, 1), d(2026, 7, 31)));
        assert_eq!(month_bounds("2026-02").unwrap(), (d(2026, 2, 1), d(2026, 2, 28)));
        assert_eq!(month_bounds("2026-12").unwrap(), (d(2026, 12, 1), d(2026, 12, 31)));
        assert!(month_bounds("2026/07").is_none());
        assert!(month_bounds("七月").is_none());
    }
}
