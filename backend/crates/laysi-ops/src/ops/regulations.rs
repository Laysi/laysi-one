//! Regulation versions: read, save and delete the rows of `reg_ruleset`.
//!
//! `reg_ruleset` is where every rate, bracket, ceiling and statutory day
//! count lives, and the calculators read nothing else. Editing a rate here is
//! what an amendment costs: no release, no restart. The embedded rule files
//! are only the factory set for a fresh install (see `laysi_regulation::seed`).
//!
//! Amendments arrive the same way rather than in a new image: `refresh_regulations`
//! fetches what the vendor published and writes the versions this database has
//! not seen. It carries payloads, not statements, so the feed supplies values
//! and cannot reach past `reg_ruleset`.
//!
//! Two things guard the money paths. A saved payload is run through
//! [`probe`] before it is written, so a version missing a field the
//! calculator needs is refused at the point someone can still fix it rather
//! than on the day payroll runs. And [`DeleteRegulation`] refuses the last
//! version of a domain: `resolve` returns an error rather than a default, so
//! an empty domain stops payroll, VAT or depreciation outright.
//!
//! Every write ends with `Store::reload_regulations`, which reads the table
//! back and installs it. The table is the authority; the process cache is
//! only what it was last seen to hold.

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_regulation::{Money, MonthPart, Ruleset};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListRegulations),
        Box::new(SaveRegulation),
        Box::new(DeleteRegulation),
        Box::new(RefreshRegulations),
    ]
}

/// Display name of a regulation domain.
fn domain_label(domain: &str) -> &'static str {
    match domain {
        "labor_insurance" => "勞工保險",
        "health_insurance" => "全民健康保險",
        "pension" => "勞工退休金",
        "vat" => "營業稅",
        "minimum_wage" => "最低工資",
        "withholding" => "各類所得扣繳",
        "annual_leave" => "特別休假",
        "depreciation" => "固定資產折舊",
        "overtime" => "延長工時",
        "owner_insurance" => "負責人投保",
        "leave_pay" => "請假工資",
        "health_dependents" => "健保眷屬計費",
        "partial_month" => "不足月計費",
        other => {
            debug_assert!(false, "法規領域 {other} 沒有顯示名稱");
            "其他"
        }
    }
}

/// Display name of every payload key, in the order a reader wants them.
///
/// Keyed on the last path segment, so a value inside an array item reads the
/// same as one at the top level. Identifiers must not reach the screen, and
/// the rule files carry their explanations in comments, which the payload
/// does not keep. Position is also the sort order: `jsonb` gives keys back
/// alphabetically, which puts a share percentage above the ceiling it applies
/// to.
const FIELDS: &[(&str, &str)] = &[
    ("max_insured_salary", "投保薪資上限"),
    ("premium_rate", "普通事故保險費率"),
    ("rate", "費率"),
    ("avg_dependents", "平均眷口數"),
    ("max_counted", "計費眷口數上限"),
    ("labor_days_per_month", "勞保：每月計費日數"),
    ("pension_days_per_month", "勞退：每月提繳日數"),
    ("employer_rate", "雇主提繳率"),
    ("employer_share", "雇主負擔比率"),
    ("employee_share", "員工負擔比率"),
    ("government_share", "政府補助比率"),
    ("health_employer_share", "健保：公司負擔比率"),
    ("health_employee_share", "健保：本人負擔比率"),
    ("health_government_share", "健保：政府補助比率"),
    ("pension_employer_rate", "勞退：公司提繳率"),
    ("period_months", "每期月數"),
    ("monthly", "每月最低工資"),
    ("hourly", "每小時最低工資"),
    ("code", "所得格式代號"),
    ("name", "名稱"),
    ("label", "名稱"),
    ("threshold", "起扣點"),
    ("codes", "所得類別"),
    ("supplementary_health", "補充保險費"),
    ("months", "年資（滿幾個月）"),
    ("days", "特休日數"),
    ("tiers", "年資級距"),
    ("increment_per_year", "每滿一年增加日數"),
    ("max_days", "特休日數上限"),
    ("over_ten_years", "滿十年之後"),
    ("carry_over_months", "遞延期限（月）"),
    ("settlement", "年度結算"),
    ("default_method", "預設折舊方法"),
    ("methods", "可用的折舊方法"),
    ("residual_rate", "殘值率"),
    ("proration", "不足一期的計算方式"),
    ("useful_lives", "耐用年數"),
    ("monthly_hours", "月工時基數"),
    ("monthly_max_hours", "每月延長工時上限"),
    ("max_hours", "單日延長工時上限"),
    ("up_to_hours", "適用至幾小時"),
    ("multiplier", "倍數"),
    ("bands", "時數級距"),
    ("day_types", "日別"),
    ("annual", "特休"),
    ("personal", "事假"),
    ("sick", "病假"),
    ("menstrual", "生理假"),
    ("official", "公假"),
    ("marriage", "婚假"),
    ("bereavement", "喪假"),
    ("maternity", "產假"),
    ("ratio", "工資給付比例"),
    ("yearly_days", "每年日數上限"),
    ("beyond_ratio", "超過上限的給付比例"),
    ("separate_days", "不併入病假的日數"),
    ("min_service_months", "工資照給所需年資（月）"),
    ("short_service_ratio", "年資未滿時的給付比例"),
    ("note", "說明"),
];

/// Sort rank and display name of one payload key.
fn field_meta(key: &str) -> (usize, &'static str) {
    match FIELDS.iter().position(|(k, _)| *k == key) {
        Some(i) => (i, FIELDS[i].1),
        None => (FIELDS.len(), super::assets::category_label(key)),
    }
}

fn field_label(key: &str) -> &'static str {
    field_meta(key).1
}

/// One payload node, flattened for the screen.
///
/// A value node carries `value` and a `path` the save call writes back to; a
/// group node carries `children`. Only text values are editable: a list of
/// method names is a set the calculators understand, not a number anyone
/// amends.
fn fields(node: &Value, prefix: &str) -> Vec<Value> {
    let path = |key: &str| {
        if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match node {
        Value::Object(map) => {
            let mut keyed: Vec<(usize, &String, &Value)> = map
                .iter()
                .map(|(key, child)| (field_meta(key).0, key, child))
                .collect();
            keyed.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
            keyed
                .into_iter()
                .map(|(_, key, child)| {
                    let here = path(key);
                    match child {
                        Value::String(text) => json!({
                            "path": here, "label": field_label(key),
                            "value": text, "editable": true,
                        }),
                        Value::Array(items) if items.iter().all(Value::is_string) => json!({
                            "path": here, "label": field_label(key), "editable": false,
                            "value": items.iter().filter_map(Value::as_str)
                                .collect::<Vec<_>>().join("、"),
                        }),
                        other => json!({
                            "path": here, "label": field_label(key),
                            "children": fields(other, &here),
                        }),
                    }
                })
                .collect()
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let here = path(&i.to_string());
                let label = ["label", "name", "code"]
                    .iter()
                    .find_map(|k| item.get(k).and_then(Value::as_str))
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("第 {} 項", i + 1));
                json!({ "path": here, "label": label, "children": fields(item, &here) })
            })
            .collect(),
        _ => Vec::new(),
    }
}

struct ListRegulations;

#[async_trait]
impl Operation for ListRegulations {
    fn name(&self) -> &'static str {
        "list_regulations"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀法規版本：勞健保費率、勞退提繳、營業稅、最低工資等每一版的內容、法源與生效日"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::AsOfInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: crate::AsOfInput = parse(input)?;
        let as_of = i.as_of.unwrap_or(ctx.today);
        let domains: Vec<Value> = laysi_regulation::DOMAINS
            .iter()
            .map(|domain| {
                // A domain with no rule on that date is a fact worth showing,
                // not an error.
                let in_force = laysi_regulation::resolve(domain, as_of).ok();
                let versions: Vec<Value> = laysi_regulation::all()
                    .iter()
                    .filter(|r| &r.domain.as_str() == domain)
                    .map(|r| {
                        json!({
                            "version": r.version,
                            "effective_from": r.effective_from,
                            "effective_to": r.effective_to,
                            "source_note": r.source_note,
                            "in_force": in_force.is_some_and(|f| f.version == r.version),
                            "payload": r.payload,
                            "fields": fields(&r.payload, ""),
                        })
                    })
                    .collect();
                json!({
                    "domain": domain,
                    "label": domain_label(domain),
                    "applicable": in_force.is_some(),
                    "version": in_force.map(|r| r.version.clone()),
                    "versions": versions,
                })
            })
            .collect();
        let summary = format!("讀取法規版本 · {} 個領域 · 適用日 {as_of}", domains.len());
        Ok(OpOutcome::new(
            json!({ "as_of": as_of, "domains": domains }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct SaveRegulationInput {
    /// Regulation domain, one of the codes `list_regulations` returns.
    pub domain: String,
    /// Version label, unique within the domain. Saving an existing one
    /// replaces it.
    pub version: String,
    /// First date this version applies to.
    pub effective_from: NaiveDate,
    /// Last date it applies to. Leave empty while it is still in force.
    #[serde(default)]
    pub effective_to: Option<NaiveDate>,
    /// The article or schedule this comes from.
    pub source_note: String,
    /// The rules themselves. Every number is a string.
    pub payload: Value,
}

/// Run the domain's own calculators over a candidate payload.
///
/// The generic checks cannot tell a payload missing `premium_rate` from one
/// that never needed it, so each domain is asked to compute something. The
/// inputs are arbitrary; only whether it can compute at all is being asked.
fn probe(rs: &Ruleset) -> Result<(), OpError> {
    let one = Money::ONE;
    match rs.domain.as_str() {
        "labor_insurance" => {
            laysi_regulation::labor_insurance(rs, one, MonthPart::WHOLE)?;
        }
        "health_insurance" => {
            let dependants = laysi_regulation::resolve("health_dependents", rs.effective_from)?;
            laysi_regulation::health_insurance(rs, dependants, one, 1)?;
        }
        "health_dependents" => {
            laysi_regulation::dependants_charged(rs, 1)?;
        }
        "partial_month" => {
            let (start, end) = (rs.effective_from, rs.effective_from);
            for key in ["labor_days_per_month", "pension_days_per_month"] {
                laysi_regulation::month_part(rs, key, start, end, start, end)?;
            }
        }
        "pension" => {
            laysi_regulation::pension(rs, one, MonthPart::WHOLE)?;
        }
        "vat" => {
            laysi_regulation::vat(rs, one)?;
            laysi_regulation::vat_from_total(rs, one)?;
        }
        "minimum_wage" => {
            laysi_regulation::minimum_wage_shortfall(rs, one, "monthly")?;
            laysi_regulation::minimum_wage_shortfall(rs, one, "hourly")?;
        }
        "owner_insurance" => {
            let health = laysi_regulation::resolve("health_insurance", rs.effective_from)?;
            let dependants = laysi_regulation::resolve("health_dependents", rs.effective_from)?;
            laysi_regulation::health_insurance_owner(health, rs, dependants, one, 1)?;
            laysi_regulation::pension_owner(rs, one, MonthPart::WHOLE)?;
        }
        "annual_leave" => {
            laysi_regulation::annual_leave_days(rs, 132)?;
            rs.decimal("settlement.carry_over_months")?;
        }
        "leave_pay" => {
            // Every kind a request can carry must be priced: a kind the
            // payload forgot would stop payroll on the first such request.
            for kind in super::attendance::LEAVE_KINDS {
                laysi_regulation::leave_pay(rs, kind)?;
            }
        }
        "overtime" => {
            let types = laysi_regulation::overtime_day_types(rs)?;
            if types.is_empty() {
                return Err(OpError::BadInput("延長工時至少要有一種日別。".into()));
            }
            for t in types {
                laysi_regulation::overtime_pay(rs, one, &t.code, one)?;
            }
            laysi_regulation::overtime_monthly_max_hours(rs)?;
            // The band walk no longer reads it, so this is what keeps a payload
            // that dropped it from saving.
            laysi_regulation::ordinary_hourly_wage(rs, one)?;
        }
        "withholding" => {
            let codes = rs
                .payload
                .get("codes")
                .and_then(Value::as_array)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| OpError::BadInput("扣繳率標準至少要有一個所得類別。".into()))?
                .clone();
            for c in codes {
                let code = c.get("code").and_then(Value::as_str).unwrap_or_default();
                super::tax::code_rule(rs, code)?;
            }
            rs.decimal("supplementary_health.rate")?;
            rs.decimal("supplementary_health.threshold")?;
        }
        "depreciation" => {
            rs.decimal("residual_rate")?;
            let lives = rs
                .payload
                .get("useful_lives")
                .and_then(Value::as_object)
                .filter(|m| !m.is_empty())
                .ok_or_else(|| OpError::BadInput("耐用年數表至少要有一個類別。".into()))?
                .clone();
            for key in lives.keys() {
                let years = rs.decimal(&format!("useful_lives.{key}"))?;
                if years <= Money::ZERO {
                    return Err(OpError::BadInput(format!(
                        "「{}」的耐用年數為 {years}，請填大於 0 的年數。",
                        field_label(key)
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

struct SaveRegulation;

#[async_trait]
impl Operation for SaveRegulation {
    fn name(&self) -> &'static str {
        "save_regulation"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "存法規版本：新增或改寫一個領域某一版的費率、級距與上限，存檔後立即適用"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SaveRegulationInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SaveRegulationInput = parse(input)?;
        if !laysi_regulation::DOMAINS.contains(&i.domain.as_str()) {
            return Err(OpError::BadInput(format!(
                "沒有「{}」這個法規領域。",
                i.domain
            )));
        }
        let version = i.version.trim().to_string();
        if version.is_empty() {
            return Err(OpError::BadInput("請填版本名稱，例如 2027.1。".into()));
        }
        if !i.payload.is_object() {
            return Err(OpError::BadInput("法規內容的格式不正確。".into()));
        }
        if let Some(end) = i.effective_to {
            if end < i.effective_from {
                return Err(OpError::BadInput("終止日早於生效日，請重新填寫。".into()));
            }
        }
        let source_note = i.source_note.trim().to_string();
        if source_note.is_empty() {
            return Err(OpError::BadInput(
                "請填法源，報表與稽核要引用得到條文。".into(),
            ));
        }

        let candidate = Ruleset {
            domain: i.domain.clone(),
            version: version.clone(),
            effective_from: i.effective_from,
            effective_to: i.effective_to,
            source_note,
            payload: i.payload,
        };
        probe(&candidate)?;

        ctx.store.write_regulation(&candidate, true).await?;
        let n = ctx.store.reload_regulations().await?;
        let label = domain_label(&i.domain);
        let summary = format!(
            "儲存法規版本 · {label} {version} · 自 {} 起適用",
            i.effective_from
        );
        Ok(OpOutcome::new(
            json!({
                "domain": i.domain,
                "label": label,
                "version": version,
                "effective_from": i.effective_from,
                "rulesets": n,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteRegulationInput {
    /// Regulation domain.
    pub domain: String,
    /// Version to remove.
    pub version: String,
}

struct DeleteRegulation;

#[async_trait]
impl Operation for DeleteRegulation {
    fn name(&self) -> &'static str {
        "delete_regulation"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "刪除法規版本：移除一個領域的某一版，該領域至少要留下一版"
    }
    fn input_schema(&self) -> Value {
        schema_of::<DeleteRegulationInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DeleteRegulationInput = parse(input)?;
        let label = domain_label(&i.domain);
        let remaining = laysi_regulation::all()
            .iter()
            .filter(|r| r.domain == i.domain)
            .count();
        if remaining <= 1 {
            return Err(OpError::BadInput(format!(
                "{label}只剩這一版，刪掉之後薪資、申報與折舊都無法計算。請先新增下一版。"
            )));
        }
        if ctx.store.delete_regulation(&i.domain, &i.version).await? == 0 {
            return Err(OpError::BadInput(format!(
                "{label}沒有「{}」這一版。",
                i.version
            )));
        }
        let n = ctx.store.reload_regulations().await?;
        let summary = format!("刪除法規版本 · {label} {}", i.version);
        Ok(OpOutcome::new(
            json!({ "domain": i.domain, "label": label, "version": i.version, "rulesets": n }),
            summary,
        ))
    }
}

/// Where amended rates are published.
///
/// An amendment is a number the legislature changed, so it must not cost a
/// new image: this is where an installation goes to get one. The address is a
/// constant rather than a setting because the publisher is the vendor, not
/// the company, and a wrong value here would be a rate nobody can trace.
const FEED_URL: &str =
    "https://raw.githubusercontent.com/Laysi/laysi-one-regulation-feed/main/manifest.json";

const FEED_SOURCE: &str = "法規更新來源";

/// The feed, as it is published.
///
/// It carries payloads, not statements: the server does its own insert, so
/// whoever publishes supplies values and cannot reach past the table they
/// belong in. Every payload still goes through [`probe`] before it is
/// written, the same check a hand-typed one gets.
#[derive(serde::Deserialize)]
struct Feed {
    #[serde(default)]
    updates: Vec<Update>,
}

/// One published amendment. `file` is what `reg_update` remembers, so it must
/// stay stable once published: change it and every installation applies the
/// amendment a second time, including the ones that deleted it on purpose.
#[derive(serde::Deserialize)]
struct Update {
    file: String,
    domain: String,
    version: String,
    effective_from: NaiveDate,
    #[serde(default)]
    effective_to: Option<NaiveDate>,
    source_note: String,
    payload: Value,
}

struct RefreshRegulations;

#[async_trait]
impl Operation for RefreshRegulations {
    fn name(&self) -> &'static str {
        "refresh_regulations"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "檢查法規更新：向廠商取修法後的費率與級距並寫入，已有或曾刪除的版本不動"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let body = super::fetch_text(FEED_URL, FEED_SOURCE).await?;
        let feed: Feed = serde_json::from_str(&body)
            .map_err(|_| OpError::BadInput(format!("{FEED_SOURCE}的內容無法解讀，請稍後再試。")))?;

        let mut applied = Vec::new();
        for u in &feed.updates {
            if !laysi_regulation::DOMAINS.contains(&u.domain.as_str()) {
                return Err(OpError::BadInput(format!(
                    "{FEED_SOURCE}提供了本版本不認得的法規領域「{}」，請先更新系統。",
                    u.domain
                )));
            }
            let candidate = Ruleset {
                domain: u.domain.clone(),
                version: u.version.clone(),
                effective_from: u.effective_from,
                effective_to: u.effective_to,
                source_note: u.source_note.clone(),
                payload: u.payload.clone(),
            };
            probe(&candidate)?;
            if ctx
                .store
                .apply_regulation_update(&u.file, &candidate)
                .await?
            {
                // A later payload's probe can need an earlier one in the same
                // feed: owner_insurance resolves health_insurance, and resolve()
                // reads the installed set, not the table. Without this the whole
                // refresh dies on owner_insurance whenever the process has no
                // applicable health_insurance.
                ctx.store.reload_regulations().await?;
                applied.push(json!({
                    "domain": u.domain,
                    "label": domain_label(&u.domain),
                    "version": u.version,
                    "effective_from": u.effective_from,
                }));
            }
        }

        let n = ctx.store.reload_regulations().await?;
        let summary = if applied.is_empty() {
            format!("檢查法規更新 · 讀取 {} 版 · 沒有新版本", feed.updates.len())
        } else {
            format!(
                "檢查法規更新 · 讀取 {} 版 · 新增 {} 版",
                feed.updates.len(),
                applied.len()
            )
        };
        Ok(OpOutcome::new(
            json!({
                "read": feed.updates.len(),
                "applied": applied,
                "rulesets": n,
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factory(domain: &str) -> Ruleset {
        laysi_regulation::seed()
            .into_iter()
            .find(|r| r.domain == domain)
            .unwrap()
    }

    /// Every shipped ruleset passes the check a saved one has to pass.
    /// A domain whose probe reads a field the factory set does not have
    /// would refuse every edit to it.
    #[test]
    fn the_factory_set_passes_its_own_probe() {
        for rs in laysi_regulation::seed() {
            probe(&rs).unwrap_or_else(|e| panic!("{} 的出廠版本通不過檢查：{e}", rs.domain));
        }
    }

    /// The reason the probe exists: dropping a field the calculator needs is
    /// refused when it is saved, not on the day payroll runs.
    #[test]
    fn a_payload_missing_a_field_is_refused() {
        let mut rs = factory("labor_insurance");
        rs.payload.as_object_mut().unwrap().remove("premium_rate");
        assert!(probe(&rs).is_err());

        let mut overtime = factory("overtime");
        overtime.payload["day_types"] = json!([]);
        assert!(probe(&overtime).is_err());

        let mut dep = factory("depreciation");
        dep.payload["useful_lives"]["vehicle"] = "0".into();
        assert!(probe(&dep).is_err());
    }

    /// Identifiers must not reach the screen: every leaf of every shipped
    /// payload has a display name.
    #[test]
    fn every_shipped_field_has_a_label() {
        fn walk(node: &Value, seen: &mut Vec<String>) {
            match node {
                Value::Object(map) => {
                    for (key, child) in map {
                        seen.push(key.clone());
                        walk(child, seen);
                    }
                }
                Value::Array(items) => items.iter().for_each(|i| walk(i, seen)),
                _ => {}
            }
        }
        let mut keys = Vec::new();
        for rs in laysi_regulation::seed() {
            walk(&rs.payload, &mut keys);
        }
        for key in keys {
            assert_ne!(field_label(&key), "其他", "欄位「{key}」沒有顯示名稱");
        }
    }

    /// The feed's shape, held against what the generator writes. A rename on
    /// either side would otherwise surface as a refresh that reads zero
    /// versions and reports success.
    #[test]
    fn the_feed_parses_into_rulesets_the_probe_accepts() {
        let vat = factory("vat");
        let body = json!({
            "updates": [{
                "file": "2026-01-01_vat.sql",
                "domain": vat.domain,
                "version": vat.version,
                "effective_from": vat.effective_from,
                "effective_to": null,
                "source_note": vat.source_note,
                "payload": vat.payload,
            }]
        })
        .to_string();

        let feed: Feed = serde_json::from_str(&body).expect("發布格式應解得開");
        let u = feed.updates.first().expect("應有一版");
        assert_eq!(u.file, "2026-01-01_vat.sql");
        probe(&Ruleset {
            domain: u.domain.clone(),
            version: u.version.clone(),
            effective_from: u.effective_from,
            effective_to: u.effective_to,
            source_note: u.source_note.clone(),
            payload: u.payload.clone(),
        })
        .expect("發布的版本應通得過寫入前的檢查");
    }

    /// A field the feed omits must not fail the whole refresh: only
    /// `effective_to` is optional, and it means still in force.
    #[test]
    fn an_open_ended_version_needs_no_end_date() {
        let feed: Feed = serde_json::from_str(
            r#"{"updates":[{"file":"f","domain":"vat","version":"v",
                 "effective_from":"2026-01-01","source_note":"s","payload":{}}]}"#,
        )
        .expect("沒有終止日的版本應解得開");
        assert_eq!(feed.updates[0].effective_to, None);
    }

    #[test]
    fn every_domain_has_a_display_name() {
        for domain in laysi_regulation::DOMAINS {
            assert_ne!(domain_label(domain), "其他", "{domain} 沒有顯示名稱");
        }
    }

    /// Every kind of leave a request can carry must be priced before a
    /// version saves: a missing one would stop payroll on the first such
    /// request, weeks later.
    #[test]
    fn a_leave_pay_version_missing_a_kind_does_not_save() {
        let factory = laysi_regulation::seed()
            .into_iter()
            .find(|r| r.domain == "leave_pay")
            .expect("出廠版本應有請假工資");
        assert!(probe(&factory).is_ok());
        let mut missing = factory.clone();
        missing.payload.as_object_mut().unwrap().remove("maternity");
        assert!(probe(&missing).is_err(), "少了產假應存不進去");
        let mut half = factory;
        half.payload["sick"]
            .as_object_mut()
            .unwrap()
            .remove("beyond_ratio");
        assert!(
            probe(&half).is_err(),
            "有每年上限卻沒有超過後的比例應存不進去"
        );
    }
}
