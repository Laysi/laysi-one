//! Budgets and departmental profit and loss.
//!
//! `budget` stores budgeted amounts only; there is no actuals column. The
//! numerator of the utilisation rate is recomputed from `voucher_line` on
//! every call, following the same rule as the financial statements (see
//! `ops/reports.rs`).
//! [`tests::the_actual_column_is_the_ledger_and_nothing_else`] changes one
//! journal line and asserts the rate moves while `budget` does not.
//!
//! Departmental P&L always carries an "unassigned" row for entries with no
//! department, otherwise the departmental total would not reconcile to the
//! income statement. [`department_pl`] always reports `total_profit` so the
//! caller can check that.
//!
//! The alert threshold is `company_setting.budget_alert_percent`, set per
//! company.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_store::{AccountMovement, BudgetRow, DeptMovement};
use rust_decimal::{Decimal, RoundingStrategy};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetBudgetStatus),
        Box::new(GetDepartmentPl),
        Box::new(CreateBudgetVersion),
        Box::new(SetBudget),
    ]
}

/// Denominator of a percentage. Arithmetic, not a policy number.
const PERCENT: i64 = 100;

// Budget utilisation

/// One budget row. `actual` is always derived on the fly.
#[derive(Debug, Clone)]
struct BudgetLine {
    account_code: String,
    account_name: String,
    budget: Money,
    /// Actual spend derived from the ledger. This column does not exist in
    /// the database.
    actual: Money,
    /// Number of journal lines touching this account. Zero means nobody has
    /// spent against it this year.
    line_count: i64,
    note: String,
}

impl BudgetLine {
    fn remaining(&self) -> Money {
        self.budget - self.actual
    }

    /// Utilisation percentage. `None` when the budget is zero: division by
    /// zero has no answer, and "0%" is not the same as "no budget set".
    fn rate(&self) -> Option<Money> {
        (!self.budget.is_zero())
            .then(|| (self.actual * Decimal::from(PERCENT) / self.budget)
                .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero))
    }
}

/// Actual movement of one account over the period.
///
/// Expenses read the net debit and revenue the net credit, the same
/// orientation the income statement uses. An account with no entries is
/// zero, not "not found".
fn actual_of(movements: &[DeptMovement], code: &str) -> (Money, i64) {
    movements
        .iter()
        .filter(|m| m.code == code)
        .fold((Money::ZERO, 0), |(amount, lines), m| {
            let v = if m.kind == "revenue" { m.net_credit() } else { m.net_debit() };
            (amount + v, lines + m.line_count)
        })
}

/// Join budgets to the ledger into one table.
///
/// This is the only place in the module that computes a figure, and it
/// touches no storage other than `budget`.
fn budget_lines(budgets: &[BudgetRow], movements: &[DeptMovement]) -> Vec<BudgetLine> {
    budgets
        .iter()
        // Department budgets are left off the summary: an account with both a
        // company-wide and a department row would be double counted. Department
        // utilisation belongs to the departmental P&L.
        .filter(|b| b.dept_id.is_none())
        .map(|b| {
            let (actual, line_count) = actual_of(movements, &b.account_code);
            BudgetLine {
                account_code: b.account_code.clone(),
                account_name: b.account_name.clone(),
                budget: b.amount,
                actual,
                line_count,
                note: b.note.clone(),
            }
        })
        .collect()
}

// Departmental profit and loss

/// Profit and loss for one department, or for "unassigned".
#[derive(Debug, Clone)]
struct DeptPl {
    code: Option<String>,
    label: String,
    revenue: Money,
    /// Total cost and expense.
    cost: Money,
}

impl DeptPl {
    fn profit(&self) -> Money {
        self.revenue - self.cost
    }

    /// Margin. No revenue means no margin, so `None`; the screen shows a dash
    /// rather than a figure that looks like 0%.
    fn margin(&self) -> Option<Money> {
        (!self.revenue.is_zero())
            .then(|| (self.profit() * Decimal::from(PERCENT) / self.revenue)
                .round_dp_with_strategy(1, RoundingStrategy::MidpointAwayFromZero))
    }
}

/// Split period movements into departmental profit and loss.
///
/// Entries with no department collect into a single "unassigned" row and are
/// never apportioned, so the rows always sum to profit on the income
/// statement. That is the correctness condition for this function, and what
/// its test asserts.
fn department_pl(movements: &[DeptMovement]) -> Vec<DeptPl> {
    let mut rows: Vec<DeptPl> = Vec::new();
    for m in movements {
        if m.kind != "revenue" && m.kind != "expense" {
            continue;
        }
        let idx = match rows.iter().position(|r| r.code == m.dept_code) {
            Some(i) => i,
            None => {
                rows.push(DeptPl {
                    code: m.dept_code.clone(),
                    label: m.dept_label.clone().unwrap_or_else(|| "未指定部門".into()),
                    revenue: Money::ZERO,
                    cost: Money::ZERO,
                });
                rows.len() - 1
            }
        };
        if m.kind == "revenue" {
            rows[idx].revenue += m.net_credit();
        } else {
            rows[idx].cost += m.net_debit();
        }
    }
    // "Unassigned" always sorts last: it is a remainder, not a department.
    rows.sort_by_key(|r| r.code.is_none());
    rows
}

/// Resolve the period, defaulting to the fiscal year to date since budgets
/// are annual.
#[derive(Deserialize, JsonSchema, Default)]
pub struct BudgetPeriodInput {
    /// Period start (YYYY-MM-DD). Defaults to 1 January of that year.
    #[serde(default)]
    pub from: Option<NaiveDate>,
    /// Period end (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub to: Option<NaiveDate>,
}

fn resolve_period(i: &BudgetPeriodInput, today: NaiveDate) -> Result<(NaiveDate, NaiveDate), OpError> {
    let to = i.to.unwrap_or(today);
    let from = i.from.unwrap_or_else(|| NaiveDate::from_ymd_opt(to.year(), 1, 1).unwrap_or(to));
    if from > to {
        return Err(OpError::BadInput(format!("期間起日 {from} 晚於迄日 {to}")));
    }
    Ok((from, to))
}

// Budget utilisation

struct GetBudgetStatus;

#[derive(Deserialize, JsonSchema, Default)]
pub struct BudgetStatusInput {
    /// Budget version code (e.g. `V3`). Defaults to the current version for
    /// the year.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub from: Option<NaiveDate>,
    #[serde(default)]
    pub to: Option<NaiveDate>,
}

#[async_trait]
impl Operation for GetBudgetStatus {
    fn name(&self) -> &'static str {
        "get_budget_status"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀預算執行率：預算數為設定值，實際數一律由已過帳分錄現算。提醒門檻取自公司設定。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<BudgetStatusInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: BudgetStatusInput = parse(input)?;
        let (from, to) = resolve_period(&BudgetPeriodInput { from: i.from, to: i.to }, ctx.today)?;

        let versions = ctx.store.budget_versions().await?;
        let version = match &i.version {
            Some(code) => versions.iter().find(|v| &v.code == code).ok_or_else(|| {
                OpError::BadInput(format!("查無預算版本「{code}」"))
            })?,
            None => match versions
                .iter()
                .find(|v| v.is_current && v.fiscal_year == to.year())
                .or_else(|| versions.iter().find(|v| v.is_current))
            {
                Some(version) => version,
                // Having no budget yet is a state, not a failure. Returning an
                // error here left the screen showing one sentence and no way
                // to act on it.
                None => {
                    return Ok(OpOutcome::new(
                        json!({
                            "version": Value::Null,
                            "version_label": "",
                            "fiscal_year": to.year(),
                            "versions": versions,
                            "from": from,
                            "to": to,
                            "rows": [],
                            "budget_total": "0",
                            "actual_total": "0",
                            "remaining_total": "0",
                            "alert_percent": alert_percent(ctx).await?.map(|p| p.to_string()),
                            "alert_count": 0,
                            "actual_source": "已過帳分錄的即時計算結果",
                        }),
                        "讀取預算執行 · 尚未建立任何預算版本".to_string(),
                    ))
                }
            },
        };

        let budgets = ctx.store.budget_rows(&version.code).await?;
        let movements = ctx.store.dept_movements(Some(from), to).await?;
        let lines = budget_lines(&budgets, &movements);

        // The alert threshold is a company setting, not a number in this file.
        let alert = alert_percent(ctx).await?;
        let over_alert = |rate: Option<Money>| match (rate, alert) {
            (Some(rate), Some(alert)) => rate >= alert,
            _ => false,
        };

        let budget_total: Money = lines.iter().map(|l| l.budget).sum();
        let actual_total: Money = lines.iter().map(|l| l.actual).sum();
        let over = lines.iter().filter(|l| over_alert(l.rate())).count();

        let rows: Vec<Value> = lines
            .iter()
            .map(|l| {
                let rate = l.rate();
                json!({
                    "account_code": l.account_code,
                    "account_name": l.account_name,
                    "budget": l.budget.to_string(),
                    "actual": l.actual.to_string(),
                    "remaining": l.remaining().to_string(),
                    "rate": rate.map(|r| r.to_string()),
                    "rate_label": rate.map(|r| format!("{r}%")).unwrap_or_else(|| "—".into()),
                    "alert": over_alert(rate),
                    "over_budget": l.actual > l.budget,
                    "line_count": l.line_count,
                    "note": l.note,
                })
            })
            .collect();

        let summary = match alert {
            Some(alert) => format!(
                "讀取預算執行（{} · {from} 至 {to}）· 預算 {} 實際 {} · {over} 個科目超過 {alert}%",
                version.label,
                money(budget_total),
                money(actual_total),
            ),
            None => format!(
                "讀取預算執行（{} · {from} 至 {to}）· 預算 {} 實際 {} · 尚未設定提醒門檻",
                version.label,
                money(budget_total),
                money(actual_total),
            ),
        };
        Ok(OpOutcome::new(
            json!({
                "version": version.code,
                "version_label": version.label,
                "fiscal_year": version.fiscal_year,
                "versions": versions,
                "from": from,
                "to": to,
                "rows": rows,
                "budget_total": budget_total.to_string(),
                "actual_total": actual_total.to_string(),
                "remaining_total": (budget_total - actual_total).to_string(),
                "alert_percent": alert.map(|p| p.to_string()),
                "alert_count": over,
                // This has to appear on screen: the only question anyone asks
                // about the "actual" column is where it came from.
                "actual_source": "已過帳分錄的即時計算結果",
            }),
            summary,
        ))
    }
}

/// Budget alert threshold. A company setting, neither regulation nor a
/// constant in this file.
///
/// `None` when nobody has set one, which is the state of every new
/// installation. No threshold means no alert, not a broken screen: the
/// budget still has to be readable before anyone can decide what percentage
/// deserves a warning, and on a fresh install the person reading the budget
/// is not the one who can edit company settings.
async fn alert_percent(ctx: &OpCtx) -> Result<Option<Money>, OpError> {
    let settings = ctx.store.company_settings().await?;
    let Some(raw) = settings.iter().find(|s| s.code == "budget_alert_percent") else {
        return Ok(None);
    };
    let value = raw.value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<Money>()
        .map(Some)
        .map_err(|_| OpError::BadInput(format!("預算提醒門檻「{value}」不是一個百分比數字")))
}

// Departmental profit and loss

struct GetDepartmentPl;

#[async_trait]
impl Operation for GetDepartmentPl {
    fn name(&self) -> &'static str {
        "get_department_pl"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀部門損益：依分錄所屬部門區分的收入與成本費用。未指定部門者列為「未指定」且不分攤，因此各列加總必等於損益表的本期損益。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<BudgetPeriodInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: BudgetPeriodInput = parse(input)?;
        let (from, to) = resolve_period(&i, ctx.today)?;

        let movements = ctx.store.dept_movements(Some(from), to).await?;
        let rows = department_pl(&movements);
        let total_profit: Money = rows.iter().map(DeptPl::profit).sum();

        // A second path: the query the statements use. Both read the ledger but
        // through different SQL, and agreement is what proves "departmental P&L
        // sums to the income statement" is more than a claim.
        let ledger = ctx.store.ledger_movements(Some(from), to).await?;
        let statement_profit: Money = ledger
            .iter()
            .filter(|m| m.kind == "revenue" || m.kind == "expense")
            .map(AccountMovement::net_credit)
            .sum();
        let gap = total_profit - statement_profit;

        let unallocated = rows.iter().find(|r| r.code.is_none());
        let json_rows: Vec<Value> = rows
            .iter()
            .map(|r| {
                let margin = r.margin();
                json!({
                    "code": r.code,
                    "label": r.label,
                    "revenue": r.revenue.to_string(),
                    "cost": r.cost.to_string(),
                    "profit": r.profit().to_string(),
                    "margin": margin.map(|m| m.to_string()),
                    "margin_label": margin.map(|m| format!("{m}%")).unwrap_or_else(|| "—".into()),
                    "unallocated": r.code.is_none(),
                })
            })
            .collect();

        let summary = format!(
            "讀取部門損益（{from} 至 {to}）· {} 個部門合計 {} · 與損益表{}",
            rows.iter().filter(|r| r.code.is_some()).count(),
            money(total_profit),
            if gap.is_zero() { "相符" } else { "不符" }
        );
        Ok(OpOutcome::new(
            json!({
                "from": from,
                "to": to,
                "rows": json_rows,
                "total_revenue": rows.iter().map(|r| r.revenue).sum::<Money>().to_string(),
                "total_cost": rows.iter().map(|r| r.cost).sum::<Money>().to_string(),
                "total_profit": total_profit.to_string(),
                "statement_profit": statement_profit.to_string(),
                "difference": gap.to_string(),
                "reconciled": gap.is_zero(),
                "unallocated_cost": unallocated.map(|r| r.cost.to_string()),
                "unallocated_note":
                    "折舊等共用支出未指定部門。要歸屬到部門，請在傳票的分錄上填寫部門。",
            }),
            summary,
        ))
    }
}

// Setting a budget

struct SetBudget;

#[derive(Deserialize, JsonSchema)]
pub struct SetBudgetInput {
    /// Budget version code. Defaults to the current version.
    #[serde(default)]
    pub version: Option<String>,
    /// Account code.
    pub account_code: String,
    /// Department code. Omit for the company-wide row.
    #[serde(default)]
    pub dept_code: Option<String>,
    /// Annual budget amount.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub amount: Money,
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for SetBudget {
    fn name(&self) -> &'static str {
        "set_budget"
    }
    fn risk(&self) -> Risk {
        // Budgets are the owner's call. Changing one changes the denominator
        // of every utilisation figure.
        Risk::BossOnly
    }
    fn summary(&self) -> &'static str {
        "設定某個科目（或某部門）的年度預算金額。僅變更預算數；實際數一律來自總帳，無法於此變更。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SetBudgetInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SetBudgetInput = parse(input)?;
        if i.amount < Money::ZERO {
            return Err(OpError::BadInput("預算金額不能是負數".into()));
        }

        let versions = ctx.store.budget_versions().await?;
        let version = match &i.version {
            Some(code) => code.clone(),
            None => versions
                .iter()
                .find(|v| v.is_current)
                .map(|v| v.code.clone())
                .ok_or_else(|| OpError::BadInput("尚無現行的預算版本".into()))?,
        };

        let accounts = ctx.store.accounts().await?;
        let account = accounts
            .iter()
            .find(|a| a.code == i.account_code && a.is_postable)
            .ok_or_else(|| {
                OpError::BadInput(format!("「{}」不是可入帳的科目", i.account_code))
            })?;

        let row = ctx
            .store
            .set_budget(
                &version,
                &i.account_code,
                i.dept_code.as_deref(),
                i.amount,
                i.note.as_deref().unwrap_or(""),
                &ctx.actor,
            )
            .await?;

        let summary = format!(
            "設定預算 {version} · {} {} {}{}",
            account.code,
            account.name,
            money(i.amount),
            row.dept_label.as_ref().map(|d| format!("（{d}）")).unwrap_or_default()
        );
        Ok(OpOutcome::new(json!({ "budget": row, "version": version }), summary))
    }
}

// Creating a version

struct CreateBudgetVersion;

#[derive(Deserialize, JsonSchema)]
pub struct CreateBudgetVersionInput {
    /// Short code, e.g. `V1`. Unique across every year.
    pub code: String,
    /// Name shown on screen, e.g. 「2026 年度預算」.
    #[serde(default)]
    pub label: Option<String>,
    /// Fiscal year. Defaults to the year of today.
    #[serde(default)]
    pub fiscal_year: Option<i32>,
    /// Make this the version utilisation is measured against. Only one
    /// version per year can be current; the previous one is stood down.
    #[serde(default = "default_true")]
    pub is_current: bool,
    #[serde(default)]
    pub note: Option<String>,
}

fn default_true() -> bool {
    true
}

#[async_trait]
impl Operation for CreateBudgetVersion {
    fn name(&self) -> &'static str {
        "create_budget_version"
    }
    fn risk(&self) -> Risk {
        // Same floor as set_budget: a version is the denominator every
        // utilisation figure is measured against.
        Risk::BossOnly
    }
    fn summary(&self) -> &'static str {
        "建立一版年度預算。每個年度只能有一版是現行版，指定新的現行版時，原現行版改為非現行。金額另以 set_budget 逐科目設定。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateBudgetVersionInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CreateBudgetVersionInput = parse(input)?;
        let code = i.code.trim();
        if code.is_empty() {
            return Err(OpError::BadInput("預算版本代號要填寫".into()));
        }
        let fiscal_year = i.fiscal_year.unwrap_or_else(|| ctx.today.year());
        let label = i
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{fiscal_year} 年度預算"));

        if ctx.store.budget_versions().await?.iter().any(|v| v.code == code) {
            return Err(OpError::BadInput(format!("預算版本代號「{code}」已經有人用了")));
        }

        let version = ctx
            .store
            .create_budget_version(
                code,
                &label,
                fiscal_year,
                i.is_current,
                i.note.as_deref().unwrap_or(""),
                &ctx.actor,
            )
            .await?;

        let summary = format!(
            "建立預算版本 {} {}（{fiscal_year} 年度{}）",
            version.code,
            version.label,
            if version.is_current { "，現行版" } else { "" }
        );
        Ok(OpOutcome::new(json!({ "version": version }), summary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn n(s: &str) -> Money {
        Decimal::from_str(s).unwrap()
    }

    fn budget(code: &str, name: &str, amount: &str) -> BudgetRow {
        BudgetRow {
            account_code: code.into(),
            account_name: name.into(),
            dept_id: None,
            dept_code: None,
            dept_label: None,
            amount: n(amount),
            note: String::new(),
            updated_at: Utc::now(),
            updated_by: "測試使用者".into(),
        }
    }

    fn mv(dept: Option<(&str, &str)>, code: &str, name: &str, kind: &str, dr: &str, cr: &str) -> DeptMovement {
        DeptMovement {
            dept_id: dept.map(|_| 1),
            dept_code: dept.map(|d| d.0.to_string()),
            dept_label: dept.map(|d| d.1.to_string()),
            code: code.into(),
            name: name.into(),
            kind: kind.into(),
            report_group: if kind == "revenue" { "revenue".into() } else { "operating_expense".into() },
            debit: n(dr),
            credit: n(cr),
            line_count: 1,
        }
    }

    /// Reference P&L entries with both assigned and unassigned departments.
    fn reference_ledger() -> Vec<DeptMovement> {
        vec![
            mv(Some(("admin", "管理部")), "6111", "薪資支出", "expense", "342653", "0"),
            mv(Some(("admin", "管理部")), "6115", "租金支出", "expense", "52000", "0"),
            mv(Some(("sales", "業務部")), "6122", "旅費", "expense", "3480", "0"),
            // Depreciation carries no department: the four assets belong to
            // different units and any split would be a guess.
            mv(None, "6152", "折舊費用", "expense", "77700", "0"),
        ]
    }

    // Budgets

    /// The central property: actuals are the ledger and have no second source.
    ///
    /// Change one journal line and utilisation moves while `budget` is
    /// untouched. That is what "actuals are not stored" means.
    #[test]
    fn the_actual_column_is_the_ledger_and_nothing_else() {
        let budgets = vec![budget("6111", "薪資支出", "5600000")];
        let mut ledger = reference_ledger();

        let before = &budget_lines(&budgets, &ledger)[0];
        assert_eq!(before.actual, n("342653"), "實際數是分錄加起來的");
        assert_eq!(before.rate(), Some(n("6")), "342,653 ÷ 5,600,000 ≈ 6%");
        assert_eq!(before.remaining(), n("5257347"));

        // Add July overtime; nobody edits the budget table.
        ledger.push(mv(Some(("admin", "管理部")), "6111", "薪資支出", "expense", "1200000", "0"));
        let after = &budget_lines(&budgets, &ledger)[0];
        assert_eq!(after.actual, n("1542653"), "分錄多了，實際數就多了");
        assert_eq!(after.rate(), Some(n("28")));
        assert_eq!(after.remaining(), n("4057347"));

        // Void one (reversing entries) and the actual moves back.
        ledger.push(mv(Some(("admin", "管理部")), "6111", "薪資支出", "expense", "0", "1200000"));
        let reverted = &budget_lines(&budgets, &ledger)[0];
        assert_eq!(reverted.actual, before.actual, "反向分錄之後回到原來的數字");
        assert_eq!(budgets[0].amount, n("5600000"), "整段過程 budget 表都沒有被動過");
    }

    /// An account nobody has spent against reads 0, not "not found".
    #[test]
    fn a_budget_with_no_spending_yet_shows_zero_not_nothing() {
        let budgets = vec![budget("6142", "廣告費", "500000")];
        let line = &budget_lines(&budgets, &reference_ledger())[0];
        assert_eq!(line.actual, Money::ZERO);
        assert_eq!(line.line_count, 0, "零行分錄，畫面上要看得出是「尚未動支」");
        assert_eq!(line.rate(), Some(Money::ZERO));
        assert_eq!(line.remaining(), n("500000"));
    }

    /// A zero budget has no utilisation. "0%" and "no budget set" differ.
    #[test]
    fn a_zero_budget_has_no_rate_at_all() {
        let budgets = vec![budget("6188", "修繕費", "0")];
        let ledger = vec![mv(None, "6188", "修繕費", "expense", "5000", "0")];
        let line = &budget_lines(&budgets, &ledger)[0];
        assert_eq!(line.actual, n("5000"));
        assert_eq!(line.rate(), None, "除以零沒有答案，不能回 0");
        assert_eq!(line.remaining(), n("-5000"), "超支就是負的剩餘");
    }

    /// Revenue actuals read the credit side, the same orientation as the
    /// income statement.
    #[test]
    fn revenue_budgets_count_the_credit_side() {
        let budgets = vec![budget("4111", "銷貨收入", "12000000")];
        let ledger = vec![mv(Some(("sales", "業務部")), "4111", "銷貨收入", "revenue", "0", "45000")];
        assert_eq!(budget_lines(&budgets, &ledger)[0].actual, n("45000"));
    }

    // Departmental profit and loss

    /// The second property: departmental P&L sums to the income statement.
    ///
    /// It holds only because the "unassigned" row exists; hide it and the
    /// identity breaks immediately.
    #[test]
    fn the_departments_add_up_to_the_income_statement() {
        let ledger = reference_ledger();
        let rows = department_pl(&ledger);

        let total: Money = rows.iter().map(DeptPl::profit).sum();
        // The income statement calculation: net credit revenue minus net debit
        // expense. A fully independent path.
        let statement: Money = ledger
            .iter()
            .filter(|m| m.kind == "revenue" || m.kind == "expense")
            .map(DeptMovement::net_credit)
            .sum();
        assert_eq!(total, statement, "部門損益的合計必須等於本期損益");
        assert_eq!(total, n("-475833"));

        // Dropping "unassigned" loses 77,700, which proves it is not
        // decoration.
        let named: Money = rows.iter().filter(|r| r.code.is_some()).map(DeptPl::profit).sum();
        assert_eq!(statement - named, n("-77700"), "丟掉未指定就對不上損益表");
    }

    /// Unassigned always sorts last and carries a name.
    #[test]
    fn unallocated_costs_are_shown_never_spread_around() {
        let rows = department_pl(&reference_ledger());
        let last = rows.last().unwrap();
        assert!(last.code.is_none());
        assert_eq!(last.label, "未指定部門");
        assert_eq!(last.cost, n("77700"), "折舊在這裡，沒有被分攤到四個部門去");
        assert_eq!(rows.iter().filter(|r| r.code.is_some()).count(), 2);
    }

    /// No revenue means no margin, and the screen shows a dash.
    #[test]
    fn a_department_with_no_revenue_has_no_margin() {
        let rows = department_pl(&[
            mv(Some(("mkt", "行銷部")), "6142", "廣告費", "expense", "682000", "0"),
            mv(Some(("sales", "業務部")), "4111", "銷貨收入", "revenue", "0", "8642000"),
            mv(Some(("sales", "業務部")), "5111", "銷貨成本", "expense", "5218000", "0"),
        ]);
        let mkt = rows.iter().find(|r| r.code.as_deref() == Some("mkt")).unwrap();
        assert_eq!(mkt.profit(), n("-682000"));
        assert_eq!(mkt.margin(), None, "分母是零，不能報一個 0%");

        let sales = rows.iter().find(|r| r.code.as_deref() == Some("sales")).unwrap();
        assert_eq!(sales.profit(), n("3424000"));
        assert_eq!(sales.margin(), Some(n("39.6")), "與原型的 39.6% 一致");
    }

    #[test]
    fn periods_default_to_the_fiscal_year_to_date() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
        assert_eq!(
            resolve_period(&BudgetPeriodInput::default(), today).unwrap(),
            (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), today)
        );
        let backwards = BudgetPeriodInput {
            from: Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()),
            to: Some(today),
        };
        assert!(resolve_period(&backwards, today).is_err());
    }
}
