//! Fixed assets and depreciation.
//!
//! `fixed_asset` stores only a `category`. Useful lives and salvage rates are
//! regulation and live in `backend/regulation/<year>/depreciation.toml`; this
//! module implements the straight-line arithmetic and nothing else.
//!
//! The ruleset is resolved as of the depreciation period (`preview` and `run`
//! use the period end, listing uses today), not the acquisition date.
//! Supporting the usual "assets acquired before the amendment keep the old
//! life" transition means adding the historical tables under
//! `backend/regulation/` and switching the [`plan_for`] callers to resolve on
//! `asset.acquired_on`.
//!
//! [`RunDepreciation`] records the resolved `useful_life_years` and
//! `ruleset_version` on each entry, so a retroactive amendment can be answered
//! with "these are the rows computed under the old version".
//!
//! Total cost must equal the posted balance of 1601 and accumulated
//! depreciation must equal that of 1602; [`ListFixedAssets`] computes both
//! sides and returns them together. A depreciation run produces an unposted
//! voucher (debit 6152, credit 1602); accumulated depreciation does not move
//! until it is posted.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_regulation::Ruleset;
use laysi_store::{DepreciationLine, FixedAsset};
use rust_decimal::{Decimal, RoundingStrategy};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListFixedAssets),
        Box::new(CreateFixedAsset),
        Box::new(PreviewDepreciation),
        Box::new(RunDepreciation),
    ]
}

/// Where an asset's cost and its accumulated depreciation land by default.
/// Overridable per asset; both are only the starting point of the dropdowns.
const DEFAULT_ASSET_ACCOUNT: &str = "1601";
const DEFAULT_ACCUM_ACCOUNT: &str = "1602";

/// The distinct ledger accounts the register actually uses, sorted. Empty
/// register falls back to the default so the reconciliation still has
/// something to read.
fn account_codes(
    assets: &[FixedAsset],
    pick: impl Fn(&FixedAsset) -> &String,
    fallback: &str,
) -> Vec<String> {
    let mut codes: Vec<String> = assets.iter().map(|a| pick(a).clone()).collect();
    codes.sort();
    codes.dedup();
    if codes.is_empty() {
        codes.push(fallback.to_string());
    }
    codes
}

/// Months in a year. A calendar constant, not a regulation value.
const MONTHS_PER_YEAR: i64 = 12;

/// Round to whole dollars. Depreciation rounds half away from zero, not
/// banker's rounding.
fn to_dollars(v: Money) -> Money {
    v.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

/// Display label for a category. Presentation only; the useful life is
/// always looked up in the ruleset.
fn category_label(category: &str) -> &'static str {
    match category {
        "building_improvement" => "房屋建築及設備",
        "vehicle" => "運輸設備",
        "computer" => "電腦及週邊設備",
        "office_equipment" => "辦公設備",
        "machinery" => "機械及設備",
        "furniture" => "生財器具",
        _ => "其他",
    }
}

/// Depreciation plan for one asset: life in years, monthly charge, and the
/// total depreciable amount.
#[derive(Debug)]
struct Plan {
    life_years: i32,
    monthly: Money,
    /// Depreciable base = cost x (1 - salvage rate). The salvage rate also
    /// comes from the rules.
    depreciable: Money,
    /// Which version of the useful life table the years came from.
    version: String,
}

/// Build the depreciation plan for one asset from the rules.
///
/// An unknown useful life is an error, never a default: depreciation affects
/// taxable income, and a wrong guess costs back tax and penalties.
fn plan_for(rs: &Ruleset, asset: &FixedAsset) -> Result<Plan, OpError> {
    let key = format!("useful_lives.{}", asset.category);
    let years = rs.decimal(&key).map_err(|_| {
        OpError::BadInput(format!(
            "耐用年數表 {} 未收錄「{}」類別（{}）。請先補齊該類別的耐用年數。",
            rs.version, asset.category, asset.name
        ))
    })?;
    let life_years: i32 = years.trunc().try_into().unwrap_or(0);
    if life_years <= 0 {
        return Err(OpError::BadInput(format!(
            "耐用年數表 {} 的「{}」為 {years}，非有效年數",
            rs.version, asset.category
        )));
    }

    let depreciable = asset.cost * (Money::ONE - rs.decimal("residual_rate")?);
    let months = Decimal::from(i64::from(life_years) * MONTHS_PER_YEAR);
    Ok(Plan {
        life_years,
        monthly: to_dollars(depreciable / months),
        depreciable,
        version: rs.version.clone(),
    })
}

/// How much to charge this period.
///
/// Depreciation starts in the month of acquisition (`proration = monthly` in
/// the rule file, a part month counts as a whole) and runs until the
/// accumulated amount reaches the depreciable base, so the final month
/// automatically charges only the remainder.
/// `taken` includes unposted charges: those vouchers will be posted
/// eventually, and treating them as taken avoids charging twice.
fn due_this_period(asset: &FixedAsset, plan: &Plan, period_end: NaiveDate) -> Money {
    if !asset.recognised || asset.status != "in_use" || asset.acquired_on > period_end {
        return Money::ZERO;
    }
    let left = plan.depreciable - asset.accumulated - asset.in_transit;
    if left <= Money::ZERO {
        return Money::ZERO;
    }
    plan.monthly.min(left)
}

/// Split `YYYY-MM` into the first and last day of that month.
fn month_bounds(month: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (y, m) = month.split_once('-')?;
    let (y, m): (i32, u32) = (y.parse().ok()?, m.parse().ok()?);
    let from = NaiveDate::from_ymd_opt(y, m, 1)?;
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Some((from, NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()?))
}

/// Resolved period: the period string and its last day.
fn resolve_period(period: Option<String>, today: NaiveDate) -> Result<(String, NaiveDate), OpError> {
    let period = period.unwrap_or_else(|| today.format("%Y-%m").to_string());
    let (_, end) = month_bounds(&period)
        .ok_or_else(|| OpError::BadInput(format!("期別格式請用 YYYY-MM，收到「{period}」")))?;
    Ok((period, end))
}

// Asset listing

struct ListFixedAssets;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListFixedAssetsInput {
    /// Include disposed assets; excluded by default.
    #[serde(default)]
    pub include_disposed: bool,
}

#[async_trait]
impl Operation for ListFixedAssets {
    fn name(&self) -> &'static str {
        "list_fixed_assets"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀固定資產：成本、累計折舊、帳面淨額與耐用年數（年數查自耐用年數表，不逐筆設定），並與各資產科目與累計折舊科目的總帳餘額對帳"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListFixedAssetsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListFixedAssetsInput = parse(input)?;
        let assets = ctx.store.fixed_assets(i.include_disposed).await?;
        // The life table resolves as of today (see the module docs). With no
        // applicable version the whole listing refuses to answer, rather than
        // returning half the assets with a life and half without.
        let rs = laysi_regulation::resolve("depreciation", ctx.today)?;

        let mut rows = Vec::with_capacity(assets.len());
        let mut cost_total = Money::ZERO;
        let mut accum_total = Money::ZERO;
        let mut waiting = Money::ZERO;

        for a in &assets {
            let p = plan_for(rs, a)?;
            let book = a.cost - a.accumulated;
            let left = p.depreciable - a.accumulated - a.in_transit;
            let months_left = if p.monthly.is_zero() {
                0
            } else {
                ((left / p.monthly).ceil().trunc().try_into().unwrap_or(0i64)).max(0)
            };

            if a.recognised {
                cost_total += a.cost;
                accum_total += a.accumulated;
            } else {
                waiting += a.cost;
            }

            rows.push(json!({
                "code": a.code,
                "name": a.name,
                "category": a.category,
                "category_label": category_label(&a.category),
                "acquired_on": a.acquired_on,
                "cost": a.cost.to_string(),
                "accumulated": a.accumulated.to_string(),
                "in_transit": a.in_transit.to_string(),
                "book_value": book.to_string(),
                "monthly": p.monthly.to_string(),
                "life_years": p.life_years,
                "life_label": format!("{} 年", p.life_years),
                "months_left": months_left,
                "fully_depreciated": left <= Money::ZERO,
                "status": a.status,
                "recognised": a.recognised,
                "origin_voucher_no": a.origin_voucher_no,
                "memo": a.memo,
                // Which version of the life table this asset's years came from.
                "life_ruleset_version": p.version,
            }));
        }

        // Two independent paths: the asset register sums itself, the ledger
        // computes its own balance. Which ledger accounts to compare against
        // comes from the register, not from a constant: an asset may be
        // carried on 1501 rather than 1601, and reading a fixed pair would
        // report a difference that is really "you looked at the wrong
        // account".
        let cost_codes = account_codes(&assets, |a| &a.gl_code, DEFAULT_ASSET_ACCOUNT);
        let accum_codes = account_codes(&assets, |a| &a.accum_gl_code, DEFAULT_ACCUM_ACCOUNT);
        let borrowed: Vec<&str> = cost_codes.iter().map(String::as_str).collect();
        let gl_cost = ctx.store.balance_of(&borrowed, true).await?;
        let borrowed: Vec<&str> = accum_codes.iter().map(String::as_str).collect();
        let gl_accum = ctx.store.balance_of(&borrowed, false).await?;
        let chart = ctx.store.accounts().await?;
        let label_of = |codes: &[String]| {
            codes
                .iter()
                .map(|code| match chart.iter().find(|a| &a.code == code) {
                    Some(a) => format!("{} {}", a.code, a.name),
                    None => code.clone(),
                })
                .collect::<Vec<_>>()
                .join("、")
        };
        let cost_accounts_label = label_of(&cost_codes);
        let accum_accounts_label = label_of(&accum_codes);
        let cost_gap = cost_total - gl_cost;
        let accum_gap = accum_total - gl_accum;
        let reconciled = cost_gap.is_zero() && accum_gap.is_zero();

        let runs = ctx.store.depreciation_runs(12).await?;
        let summary = format!(
            "讀取固定資產 · {} 項成本 {} 累計折舊 {} 淨額 {} · 與資產科目總帳{}",
            rows.len(),
            money(cost_total),
            money(accum_total),
            money(cost_total - accum_total),
            if reconciled { "相符" } else { "不符" }
        );
        Ok(OpOutcome::new(
            json!({
                "assets": rows,
                "cost_total": cost_total.to_string(),
                "accumulated_total": accum_total.to_string(),
                "book_total": (cost_total - accum_total).to_string(),
                "gl_cost": gl_cost.to_string(),
                "gl_accumulated": gl_accum.to_string(),
                "cost_difference": cost_gap.to_string(),
                "accumulated_difference": accum_gap.to_string(),
                "reconciled": reconciled,
                "cost_accounts_label": cost_accounts_label,
                "accum_accounts_label": accum_accounts_label,
                "not_yet_posted": waiting.to_string(),
                "runs": runs,
                "categories": categories(rs),
                "vat": crate::ops::tax::vat_json(ctx.today)?,
            }),
            summary,
        ))
    }
}

// Registering an asset

fn default_asset_account() -> String {
    "1601".into()
}

fn default_accum_account() -> String {
    "1602".into()
}

fn default_input_tax_account() -> String {
    "1284".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateFixedAssetInput {
    /// Asset code. Left out, the next free `FA###` is taken.
    #[serde(default)]
    pub code: Option<String>,
    /// What was bought, as it should read on the register.
    pub name: String,
    /// Which line of the useful life table applies. `list_fixed_assets`
    /// returns the categories in force with their years.
    pub category: String,
    /// Acquisition date. Defaults to today. Depreciation starts in this month
    /// and the useful life table is read as of this date.
    #[serde(default)]
    pub acquired_on: Option<NaiveDate>,
    /// Cost before tax. This is the depreciable base and the debit to the
    /// asset account. A string in JSON.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub cost: Money,
    /// Deductible input tax. Left out it follows the VAT rules in force on
    /// the acquisition date; send `0` for a purchase that carries none.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub tax_amount: Option<Money>,
    /// Which account the money left, e.g. 1102 銀行存款. Writes the
    /// acquisition voucher. Bought on credit, leave this out, enter the
    /// supplier's bill on the payables screen with 1601 as its debit account,
    /// and give that bill's voucher as `origin_voucher_no`.
    #[serde(default)]
    pub paid_from_account_code: Option<String>,
    /// An existing voucher that already carries this purchase.
    #[serde(default)]
    pub origin_voucher_no: Option<String>,
    /// Asset account, default 1601.
    #[serde(default = "default_asset_account")]
    pub gl_code: String,
    /// Accumulated depreciation account, default 1602.
    #[serde(default = "default_accum_account")]
    pub accum_gl_code: String,
    /// Input tax account, default 1284. The VAT return reads the period
    /// movement of this account.
    #[serde(default = "default_input_tax_account")]
    pub input_tax_account_code: String,
    /// Department carrying the asset, so its cost reaches departmental profit
    /// and loss.
    #[serde(default)]
    pub dept_code: Option<String>,
    /// Note on the register and on the voucher.
    #[serde(default)]
    pub memo: String,
}

/// Categories the useful life table in force actually covers, with their
/// years. Read from the ruleset so the list cannot drift from the schedule
/// the charges are computed against.
fn categories(rs: &Ruleset) -> Vec<Value> {
    let Some(lives) = rs.payload.get("useful_lives").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rows: Vec<Value> = lives
        .keys()
        .map(|code| {
            let years = rs.decimal(&format!("useful_lives.{code}")).unwrap_or_default();
            json!({
                "code": code,
                "label": category_label(code),
                "life_years": years.trunc().to_string(),
                "life_label": format!("{} 年", years.trunc()),
            })
        })
        .collect();
    rows.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));
    rows
}

struct CreateFixedAsset;

#[async_trait]
impl Operation for CreateFixedAsset {
    fn name(&self) -> &'static str {
        "create_fixed_asset"
    }
    fn risk(&self) -> Risk {
        // Produces an unposted voucher that a person still has to post, the
        // same tier as create_ap_bill.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "登記固定資產並產生取得傳票（借資產科目與 1284 進項稅額／貸付款科目）。成本填未稅金額，營業稅依取得日適用的稅率拆出，免稅填 0；賒購則改在應付帳款開單、以 1601 為借方科目，再填入該傳票號。耐用年數查自耐用年數表，不逐筆設定。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateFixedAssetInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CreateFixedAssetInput = parse(input)?;
        let acquired_on = i.acquired_on.unwrap_or(ctx.today);
        let name = i.name.trim();
        if name.is_empty() {
            return Err(OpError::BadInput("資產名稱要填寫".into()));
        }

        // The life table is what makes an asset depreciable at all, so an
        // unknown category is refused here rather than at the first
        // depreciation run months later.
        let rs = laysi_regulation::resolve("depreciation", acquired_on)?;
        let category = i.category.trim();
        if rs.decimal(&format!("useful_lives.{category}")).is_err() {
            let known: Vec<String> = categories(rs)
                .iter()
                .map(|c| c["label"].as_str().unwrap_or_default().to_string())
                .collect();
            return Err(OpError::BadInput(format!(
                "耐用年數表 {} 沒有「{}」這個類別。可選：{}",
                rs.version,
                category,
                known.join("、")
            )));
        }

        // Every asset has to point at a voucher, otherwise the register would
        // claim a cost the ledger has never seen and the 1601 reconciliation
        // on this screen would report a difference no one can clear.
        let paid_from = i.paid_from_account_code.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let origin = i.origin_voucher_no.as_deref().map(str::trim).filter(|s| !s.is_empty());
        match (paid_from, origin) {
            (None, None) => {
                return Err(OpError::BadInput(
                    "請填付款科目；賒購請先在應付帳款開單，再填入該帳單的傳票號".into(),
                ))
            }
            (Some(_), Some(_)) => {
                return Err(OpError::BadInput(
                    "付款科目與既有傳票號只能填一個：填了付款科目會產生新傳票".into(),
                ))
            }
            _ => {}
        }

        // The register and the ledger are compared account by account, so the
        // two accounts have to be the kind that comparison assumes: the cost
        // on a debit-normal non-current asset account, the accumulated
        // depreciation on its credit-normal contra account.
        let chart = ctx.store.accounts().await?;
        let fixed_asset_account = |code: &str, side: &str, what: &str| -> Result<(), OpError> {
            match chart.iter().find(|a| a.code == code) {
                Some(a)
                    if a.report_group == "non_current_asset"
                        && a.normal_side == side
                        && a.is_postable
                        && a.is_active =>
                {
                    Ok(())
                }
                _ => Err(OpError::BadInput(format!(
                    "「{code}」不是可入帳的{what}科目"
                ))),
            }
        };
        fixed_asset_account(i.gl_code.trim(), "debit", "固定資產")?;
        fixed_asset_account(i.accum_gl_code.trim(), "credit", "累計折舊")?;

        let split = crate::ops::tax::split_vat(acquired_on, Some(i.cost), i.tax_amount, None)?;
        let asset = ctx
            .store
            .create_fixed_asset(laysi_store::NewFixedAsset {
                code: i.code.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                name,
                category,
                acquired_on,
                cost: split.net,
                tax_amount: split.tax,
                gl_code: i.gl_code.trim(),
                accum_gl_code: i.accum_gl_code.trim(),
                input_tax_account_code: i.input_tax_account_code.trim(),
                paid_from_account_code: paid_from,
                origin_voucher_no: origin,
                dept_code: i.dept_code.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                memo: i.memo.trim(),
                created_by: &ctx.actor,
            })
            .await?;

        let plan = plan_for(rs, &asset)?;
        let summary = format!(
            "登記資產 {} {} · 成本 {}（營業稅 {}，依 vat {}）· 耐用 {} 年、每月折舊 {} · {}",
            asset.code,
            asset.name,
            money(asset.cost),
            money(split.tax),
            split.version,
            plan.life_years,
            money(plan.monthly),
            match &asset.origin_voucher_no {
                Some(no) => format!("取得傳票 {no}"),
                None => "無取得傳票".into(),
            }
        );
        let outcome = OpOutcome::new(
            json!({
                "asset": {
                    "code": asset.code,
                    "name": asset.name,
                    "category": asset.category,
                    "category_label": category_label(&asset.category),
                    "acquired_on": asset.acquired_on,
                    "cost": asset.cost.to_string(),
                    "gl_code": asset.gl_code,
                    "accum_gl_code": asset.accum_gl_code,
                    "origin_voucher_no": asset.origin_voucher_no,
                    "recognised": asset.recognised,
                    "memo": asset.memo,
                },
                "tax_amount": split.tax.to_string(),
                "vat_version": split.version,
                "life_years": plan.life_years,
                "monthly": plan.monthly.to_string(),
                "life_ruleset_version": plan.version,
            }),
            summary,
        );
        Ok(match asset.origin_voucher_no.clone() {
            Some(no) if paid_from.is_some() => outcome.with_voucher(no),
            _ => outcome,
        })
    }
}

// Depreciation preview and run

#[derive(Deserialize, JsonSchema, Default)]
pub struct DepreciationPeriodInput {
    /// Period (YYYY-MM). Defaults to this month.
    #[serde(default)]
    pub period: Option<String>,
}

/// Shared calculation for preview and run: returns the per-asset detail, the
/// total, the journal lines and the life table version used.
///
/// The life table resolves on the period end: the version in force for the
/// period being charged.
async fn compute(
    ctx: &OpCtx,
    period_end: NaiveDate,
) -> Result<(Vec<Value>, Money, Vec<DepreciationLine>, String), OpError> {
    let assets = ctx.store.fixed_assets(false).await?;
    let rs = laysi_regulation::resolve("depreciation", period_end)?;
    let mut detail = Vec::new();
    let mut lines = Vec::new();
    let mut total = Money::ZERO;

    for a in &assets {
        let p = plan_for(rs, a)?;
        let amount = due_this_period(a, &p, period_end);

        // Why this asset is not charged. Stated, rather than dropped from the
        // list.
        let skip = if !a.recognised {
            Some("取得傳票尚未過帳，未進入總帳")
        } else if a.acquired_on > period_end {
            Some("本期尚未取得")
        } else if amount.is_zero() {
            Some("已提列完畢")
        } else {
            None
        };

        detail.push(json!({
            "code": a.code,
            "name": a.name,
            "category_label": category_label(&a.category),
            "acquired_on": a.acquired_on,
            "cost": a.cost.to_string(),
            "accumulated": a.accumulated.to_string(),
            "amount": amount.to_string(),
            "monthly": p.monthly.to_string(),
            "life_years": p.life_years,
            "life_label": format!("{} 年", p.life_years),
            "life_ruleset_version": p.version,
            "skip_reason": skip,
        }));

        if amount > Money::ZERO {
            total += amount;
            lines.push(DepreciationLine {
                asset_id: a.id,
                asset_name: a.name.clone(),
                amount,
                useful_life_years: p.life_years,
                accum_gl_code: a.accum_gl_code.clone(),
            });
        }
    }

    Ok((detail, total, lines, rs.version.clone()))
}

struct PreviewDepreciation;

#[async_trait]
impl Operation for PreviewDepreciation {
    fn name(&self) -> &'static str {
        "preview_depreciation"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "試算某一期的折舊：逐項列出提列金額、適用耐用年數與所依據的年數表版本。僅試算，不產生傳票。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<DepreciationPeriodInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DepreciationPeriodInput = parse(input)?;
        let (period, end) = resolve_period(i.period, ctx.today)?;
        let (detail, total, lines, version) = compute(ctx, end).await?;

        // A period cannot be charged twice; say so up front instead of letting
        // the database reject it.
        let done = ctx
            .store
            .depreciation_runs(60)
            .await?
            .into_iter()
            .find(|r| r.period == period);

        let summary = format!(
            "試算 {period} 折舊 · {} 項合計 {} · 依耐用年數表 {version}{}",
            lines.len(),
            money(total),
            if done.is_some() { "（本期已提列）" } else { "" }
        );
        Ok(OpOutcome::new(
            json!({
                "period": period,
                "period_end": end,
                "assets": detail,
                "total": total.to_string(),
                "count": lines.len(),
                "already_run": done,
                "ruleset_version": version,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct RunDepreciationInput {
    /// Period (YYYY-MM). Defaults to this month.
    #[serde(default)]
    pub period: Option<String>,
    /// Voucher date. Defaults to the last day of the period, since
    /// depreciation is charged at period end.
    #[serde(default)]
    pub run_on: Option<NaiveDate>,
}

struct RunDepreciation;

#[async_trait]
impl Operation for RunDepreciation {
    fn name(&self) -> &'static str {
        "run_depreciation"
    }
    fn risk(&self) -> Risk {
        // Produces an unposted voucher that a person still has to post, the
        // same tier as record_receipt.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "提列某一期的折舊，產生待過帳傳票（借 6152 折舊費用／貸各資產的累計折舊科目）。同一期僅能提列一次，各項都記錄所用耐用年數與規則版本。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RunDepreciationInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RunDepreciationInput = parse(input)?;
        let (period, end) = resolve_period(i.period, ctx.today)?;
        let run_on = i.run_on.unwrap_or(end);
        let (detail, total, lines, version) = compute(ctx, end).await?;

        let run = ctx
            .store
            .record_depreciation_run(&period, run_on, &lines, &version, &ctx.actor)
            .await?;

        // The audit summary names the version: which life table a month was
        // charged under must be visible from the audit trail alone.
        let summary = format!(
            "{period} 折舊提列 · {} 項合計 {} · 依耐用年數表 {version} · {} 待過帳",
            lines.len(),
            money(total),
            run.voucher_no
        );
        let no = run.voucher_no.clone();
        let voucher = ctx.store.voucher_by_no(&no).await?;
        Ok(OpOutcome::new(
            json!({
                "run": run,
                "voucher": voucher,
                "assets": detail,
                "total": total.to_string(),
                "ruleset_version": version,
            }),
            summary,
        )
        .with_voucher(no))
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

    fn dep(as_of: NaiveDate) -> &'static Ruleset {
        laysi_regulation::resolve("depreciation", as_of).unwrap()
    }

    fn asset(category: &str, acquired: NaiveDate, cost: &str, accumulated: &str) -> FixedAsset {
        FixedAsset {
            id: 1,
            code: "FA-001".into(),
            name: "測試資產".into(),
            category: category.into(),
            acquired_on: acquired,
            cost: n(cost),
            method: "straight_line".into(),
            gl_code: "1601".into(),
            accum_gl_code: "1602".into(),
            origin_voucher_no: None,
            status: "in_use".into(),
            memo: String::new(),
            recognised: true,
            accumulated: n(accumulated),
            in_transit: Money::ZERO,
        }
    }

    /// The central property: the useful life comes from the life table, not
    /// from the code.
    ///
    /// The same binary with a different life table gives a different monthly
    /// charge, without a single change to this file.
    #[test]
    fn useful_life_comes_from_the_ruleset_not_from_this_file() {
        let a = asset("computer", d(2025, 1, 20), "70200", "0");

        let rs = dep(d(2026, 7, 31));
        let p = plan_for(rs, &a).unwrap();
        assert_eq!(p.life_years, 3, "電腦及週邊設備 3 年");
        assert_eq!(p.monthly, n("1950"), "70,200 ÷ 36");

        // Simulate the computer life changing to 5 years in 2027.
        let mut next = rs.clone();
        next.version = "2027.1".into();
        next.effective_from = d(2027, 1, 1);
        next.payload["useful_lives"]["computer"] = json!("5");
        let p2 = plan_for(&next, &a).unwrap();
        assert_eq!(p2.life_years, 5);
        assert_eq!(p2.monthly, n("1170"), "70,200 ÷ 60");

        // 2026 charges are unaffected: months already filed do not move.
        assert_eq!(plan_for(rs, &a).unwrap().monthly, n("1950"));
        assert_ne!(p.version, p2.version, "版本要能分辨，否則追不出哪一筆用舊版");
    }

    /// The salvage rate is a rule too: raise it to 10% and both the
    /// depreciable base and the monthly charge follow.
    #[test]
    fn the_residual_rate_comes_from_the_ruleset_too() {
        let a = asset("computer", d(2025, 1, 20), "70200", "0");
        let rs = dep(d(2026, 7, 31));
        assert_eq!(plan_for(rs, &a).unwrap().depreciable, n("70200"), "目前殘值率是 0");

        let mut next = rs.clone();
        next.version = "2027.1".into();
        next.payload["residual_rate"] = json!("0.1");
        let p = plan_for(&next, &a).unwrap();
        assert_eq!(p.depreciable, n("63180"), "70,200 × 0.9");
        assert_eq!(p.monthly, n("1755"), "63,180 ÷ 36");
    }

    /// Monthly charges for the four reference assets, all verifiable by hand.
    #[test]
    fn reference_assets_reproduce_their_monthly_amounts() {
        let rs = dep(d(2026, 7, 31));
        let cases = [
            ("building_improvement", d(2023, 4, 1), "600000", "5000", 10),
            ("vehicle", d(2024, 6, 15), "300000", "5000", 5),
            ("computer", d(2025, 1, 20), "70200", "1950", 3),
            ("office_equipment", d(2025, 9, 1), "60000", "1000", 5),
        ];
        let mut monthly_total = Money::ZERO;
        for (cat, acquired, cost, expect, years) in cases {
            let a = asset(cat, acquired, cost, "0");
            let p = plan_for(rs, &a).unwrap();
            assert_eq!(p.life_years, years, "{cat} 的年數");
            assert_eq!(p.monthly, n(expect), "{cat} 的月折舊");
            monthly_total += p.monthly;
        }
        assert_eq!(monthly_total, n("12950"), "四項合計，與畫面上的試算一致");
    }

    /// The final month charges only the remainder; accumulated depreciation
    /// never exceeds cost.
    #[test]
    fn the_last_month_takes_only_what_is_left() {
        let rs = dep(d(2027, 11, 30));
        // 34 months taken (66,300) leaves 3,900, and 1,950 a month means two
        // more months.
        let a = asset("computer", d(2025, 1, 20), "70200", "68250");
        let p = plan_for(rs, &a).unwrap();
        assert_eq!(due_this_period(&a, &p, d(2027, 11, 30)), n("1950"));

        // With only 900 left, this month charges 900.
        let a = asset("computer", d(2025, 1, 20), "70200", "69300");
        assert_eq!(due_this_period(&a, &p, d(2027, 12, 31)), n("900"));

        // Fully depreciated, nothing more is charged.
        let a = asset("computer", d(2025, 1, 20), "70200", "70200");
        assert_eq!(due_this_period(&a, &p, d(2028, 1, 31)), Money::ZERO);
    }

    /// An asset not yet in the ledger is not depreciated: charging it would
    /// put the register and the ledger out of step.
    #[test]
    fn an_asset_whose_voucher_is_unposted_is_not_depreciated_yet() {
        let rs = dep(d(2026, 7, 31));
        let mut a = asset("computer", d(2026, 7, 22), "68000", "0");
        a.origin_voucher_no = Some("V2607-016".into());
        a.recognised = false;
        let p = plan_for(rs, &a).unwrap();
        assert_eq!(p.monthly, n("1889"), "68,000 ÷ 36 四捨五入");
        assert_eq!(due_this_period(&a, &p, d(2026, 7, 31)), Money::ZERO);

        // Once the source voucher is posted it joins the run.
        a.recognised = true;
        assert_eq!(due_this_period(&a, &p, d(2026, 7, 31)), n("1889"));
    }

    /// Charging starts in the month of acquisition; nothing before it.
    #[test]
    fn depreciation_starts_in_the_month_of_acquisition() {
        let rs = dep(d(2026, 7, 31));
        let a = asset("computer", d(2026, 7, 22), "68000", "0");
        let p = plan_for(rs, &a).unwrap();
        assert_eq!(due_this_period(&a, &p, d(2026, 6, 30)), Money::ZERO, "六月時還沒買");
        assert_eq!(due_this_period(&a, &p, d(2026, 7, 31)), n("1889"), "取得當月照提一整月");
    }

    /// Charges already vouchered but not yet posted also count as taken, or
    /// they would be charged twice.
    #[test]
    fn an_unposted_depreciation_voucher_still_counts_as_taken() {
        let rs = dep(d(2027, 12, 31));
        let mut a = asset("computer", d(2025, 1, 20), "70200", "69300");
        let p = plan_for(rs, &a).unwrap();
        assert_eq!(due_this_period(&a, &p, d(2027, 12, 31)), n("900"));
        a.in_transit = n("900");
        assert_eq!(due_this_period(&a, &p, d(2028, 1, 31)), Money::ZERO);
    }

    /// A category missing from the life table must error, not fall back to a
    /// default life.
    #[test]
    fn an_unknown_category_is_refused_never_guessed() {
        let a = asset("spaceship", d(2026, 1, 1), "1000000", "0");
        let err = plan_for(dep(d(2026, 7, 31)), &a).unwrap_err();
        assert!(matches!(err, OpError::BadInput(_)));
        assert!(err.to_string().contains("請先補齊該類別的耐用年數"));
    }

    #[test]
    fn periods_parse_into_their_own_month_end() {
        assert_eq!(resolve_period(Some("2026-07".into()), d(2026, 1, 1)).unwrap().1, d(2026, 7, 31));
        assert_eq!(resolve_period(Some("2026-02".into()), d(2026, 1, 1)).unwrap().1, d(2026, 2, 28));
        assert_eq!(resolve_period(None, d(2026, 7, 25)).unwrap().0, "2026-07");
        assert!(resolve_period(Some("七月".into()), d(2026, 1, 1)).is_err());
    }
}
