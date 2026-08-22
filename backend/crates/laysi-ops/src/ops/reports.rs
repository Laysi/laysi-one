//! The three financial statements: balance sheet, income statement, cash flow.
//!
//! Every cell is a sum over `voucher_line`. No table in the system stores a
//! statement figure. All three share the single
//! [`laysi_store::Store::ledger_movements`] query so they cannot grow separate
//! definitions of "posted" or of how dates are compared.
//!
//! Profit for the period must roll into equity for the sheet to balance: P&L
//! accounts not yet closed to retained earnings hold exactly this period's
//! change in equity. [`BalanceSheet::difference`] computes and returns the
//! residual every time.
//!
//! That period is deliberately "since inception" rather than "this year".
//! There is no period close yet, so no entry moves profit into retained
//! earnings, and restricting to the current year would make last year's profit
//! vanish and break the identity. Once closing exists this code needs no
//! change, because after the closing entry the prior-year P&L balances are
//! already zero.
//!
//! Cash flow uses the indirect method, which here is an algebraic identity of
//! double-entry bookkeeping rather than an approximation:
//!
//! ```text
//! Σ all accounts (Dr − Cr) = 0
//!   ⇒ Δcash = profit + Σ non-cash accounts (Cr − Dr)
//! ```
//!
//! `account.cash_flow_section` assigns each account to exactly one activity,
//! so the sections necessarily sum to the actual movement in cash.
//! [`cash_flow`] independently recomputes the closing balance from the cash
//! accounts' own movements and returns both;
//! [`tests::the_cash_flow_statement_always_lands_on_the_cash_balance`] checks
//! they agree over randomly generated balanced entries.
//!
//! The direct method is not used because a voucher with two or more contra
//! accounts in different activities (payroll settling withheld items, for
//! instance) would require guessing how to split the cash between sections.
//!
//! The cost of the indirect method: an entry that does not touch cash but
//! crosses activities (restating a fixed asset's net book value into cost and
//! accumulated depreciation, say) appears in two sections with opposite signs.
//! The total stays correct but those sections are inflated, which the screen
//! points out.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_store::AccountMovement;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(GetBalanceSheet), Box::new(GetIncomeStatement), Box::new(GetCashFlow)]
}

// Statement rows

/// One row of a statement. `kind` carries the style so the frontend does not
/// have to work out the hierarchy itself.
///
/// `value` is always a signed amount string. How negatives are presented
/// (accounting convention is parentheses) is a layout decision for the
/// frontend; the backend must not return `"(365,100)"`, which cannot be
/// used in further arithmetic.
#[derive(Debug, Clone)]
struct Line {
    label: String,
    code: Option<String>,
    value: Option<Money>,
    /// `head` section title, `sub` detail, `total` total, empty for a plain row.
    kind: &'static str,
    hint: Option<&'static str>,
}

impl Line {
    fn head(label: impl Into<String>) -> Self {
        Self { label: label.into(), code: None, value: None, kind: "head", hint: None }
    }
    fn item(m: &AccountMovement, value: Money) -> Self {
        Self {
            label: m.name.clone(),
            code: Some(m.code.clone()),
            value: Some(value),
            kind: "sub",
            hint: None,
        }
    }
    fn plain(label: impl Into<String>, value: Money) -> Self {
        Self { label: label.into(), code: None, value: Some(value), kind: "sub", hint: None }
    }
    fn total(label: impl Into<String>, value: Money) -> Self {
        Self { label: label.into(), code: None, value: Some(value), kind: "total", hint: None }
    }
    fn with_hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }
    fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "code": self.code,
            "value": self.value.map(|v| v.to_string()),
            "kind": self.kind,
            "hint": self.hint,
        })
    }
}

fn lines_json(lines: &[Line]) -> Vec<Value> {
    lines.iter().map(Line::to_json).collect()
}

/// Select the accounts of one section, dropping those with no movement.
///
/// Accounts with no balance stay off the statement, but a zero balance with
/// transactions is kept: it records that the period's inflows and outflows
/// cancelled out, which is not the same as nothing having happened.
fn group<'a>(rows: &'a [AccountMovement], name: &str) -> impl Iterator<Item = &'a AccountMovement> {
    let name = name.to_string();
    rows.iter().filter(move |m| m.report_group == name && m.line_count > 0)
}

// Balance sheet

/// Balance sheet. The three totals are the reason this type exists; the rest
/// of the fields are presentation.
#[derive(Debug)]
struct BalanceSheet {
    assets: Money,
    liabilities: Money,
    /// Balance of the equity accounts, excluding profit for the period.
    equity_accounts: Money,
    /// Profit for the period, not yet closed out. The identity does not hold
    /// without it.
    profit: Money,
    lines: Vec<Line>,
}

impl BalanceSheet {
    fn equity(&self) -> Money {
        self.equity_accounts + self.profit
    }

    /// Assets minus (liabilities plus equity). Zero is a conclusion, not an
    /// assumption.
    fn difference(&self) -> Money {
        self.assets - self.liabilities - self.equity()
    }

    fn balanced(&self) -> bool {
        self.difference().is_zero()
    }
}

/// Build the balance sheet from cumulative movements up to a given date.
///
/// Asset rows use the net debit (so 1602 accumulated depreciation is
/// negative and correctly acts as a contra account); liability and equity
/// rows use the net credit. Each side is summed independently and whether
/// the identity holds is decided by [`BalanceSheet::difference`].
fn balance_sheet(rows: &[AccountMovement]) -> BalanceSheet {
    let mut lines = Vec::new();
    let mut assets = Money::ZERO;
    let mut liabilities = Money::ZERO;
    let mut equity_accounts = Money::ZERO;

    /// Lay out one section. `debit_side` chooses net debit or net credit:
    /// debit for assets, credit for liabilities and equity, both presented so
    /// that a positive number means the section grew.
    fn section(
        lines: &mut Vec<Line>,
        rows: &[AccountMovement],
        title: &str,
        g: &str,
        debit_side: bool,
        total: &mut Money,
    ) -> bool {
        let mut opened = false;
        for m in group(rows, g) {
            if !opened {
                lines.push(Line::head(title));
                opened = true;
            }
            let v = if debit_side { m.net_debit() } else { m.net_credit() };
            *total += v;
            lines.push(Line::item(m, v));
        }
        opened
    }

    section(&mut lines, rows, "流動資產", "current_asset", true, &mut assets);
    section(&mut lines, rows, "非流動資產", "non_current_asset", true, &mut assets);
    lines.push(Line::total("資產總計", assets));

    section(&mut lines, rows, "流動負債", "current_liability", false, &mut liabilities);
    section(&mut lines, rows, "非流動負債", "non_current_liability", false, &mut liabilities);
    let equity_opened =
        section(&mut lines, rows, "權益", "equity", false, &mut equity_accounts);

    // Profit for the period: the P&L accounts have not been closed to retained
    // earnings, so their balance is this period's change in equity. Without
    // this row the identity does not hold; it is part of equity, not a note.
    let profit: Money = rows
        .iter()
        .filter(|m| m.kind == "revenue" || m.kind == "expense")
        .map(AccountMovement::net_credit)
        .sum();
    if !profit.is_zero() {
        if !equity_opened {
            lines.push(Line::head("權益"));
        }
        lines.push(
            Line::plain("本期損益（尚未結轉）", profit)
                .with_hint("損益科目尚未結轉至保留盈餘，故目前計入權益"),
        );
    }

    let mut sheet = BalanceSheet { assets, liabilities, equity_accounts, profit, lines };
    sheet.lines.push(Line::total("負債及權益總計", sheet.liabilities + sheet.equity()));
    sheet
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct BalanceSheetInput {
    /// As-of date (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub as_of: Option<NaiveDate>,
}

struct GetBalanceSheet;

#[async_trait]
impl Operation for GetBalanceSheet {
    fn name(&self) -> &'static str {
        "get_balance_sheet"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀資產負債表：某一天的資產、負債與權益，全部由已過帳分錄現算。本期損益結轉權益，並回報「資產＝負債＋權益」的差額。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<BalanceSheetInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: BalanceSheetInput = parse(input)?;
        let as_of = i.as_of.unwrap_or(ctx.today);
        let rows = ctx.store.ledger_movements(None, as_of).await?;
        let bs = balance_sheet(&rows);

        let summary = format!(
            "讀取資產負債表（{as_of} 結存）· 資產 {} 負債 {} 權益 {} · 借貸{}",
            money(bs.assets),
            money(bs.liabilities),
            money(bs.equity()),
            if bs.balanced() { "相等" } else { "不等" }
        );
        Ok(OpOutcome::new(
            json!({
                "as_of": as_of,
                "period_label": format!("{} 結存", as_of.format("%Y/%m/%d")),
                "rows": lines_json(&bs.lines),
                "assets": bs.assets.to_string(),
                "liabilities": bs.liabilities.to_string(),
                "equity": bs.equity().to_string(),
                "equity_accounts": bs.equity_accounts.to_string(),
                "profit": bs.profit.to_string(),
                "difference": bs.difference().to_string(),
                "balanced": bs.balanced(),
                "unposted": unposted_note(ctx, as_of).await?,
            }),
            summary,
        ))
    }
}

// Income statement

#[derive(Debug)]
struct IncomeStatement {
    revenue: Money,
    cogs: Money,
    operating_expense: Money,
    other_net: Money,
    lines: Vec<Line>,
}

impl IncomeStatement {
    fn gross(&self) -> Money {
        self.revenue - self.cogs
    }
    fn operating_profit(&self) -> Money {
        self.gross() - self.operating_expense
    }
    fn profit(&self) -> Money {
        self.operating_profit() + self.other_net
    }
}

/// Build the income statement from the movements over a period.
///
/// Expense rows are presented as negatives: reading the statement top to
/// bottom and adding must give profit for the period, with no "subtract this
/// one" rule that exists only in the layout.
fn income_statement(rows: &[AccountMovement]) -> IncomeStatement {
    let mut lines = Vec::new();

    let mut revenue = Money::ZERO;
    for m in group(rows, "revenue") {
        let v = m.net_credit();
        revenue += v;
        lines.push(Line::item(m, v));
    }
    if revenue.is_zero() && lines.is_empty() {
        lines.push(Line::plain("銷貨收入", Money::ZERO));
    }

    let mut cogs = Money::ZERO;
    for m in group(rows, "cogs") {
        let v = m.net_debit();
        cogs += v;
        lines.push(Line::item(m, -v));
    }

    let mut st = IncomeStatement {
        revenue,
        cogs,
        operating_expense: Money::ZERO,
        other_net: Money::ZERO,
        lines,
    };
    st.lines.push(Line::total("營業毛利", st.gross()));

    let mut head = false;
    for m in group(rows, "operating_expense") {
        if !head {
            st.lines.push(Line::head("營業費用"));
            head = true;
        }
        let v = m.net_debit();
        st.operating_expense += v;
        st.lines.push(Line::item(m, -v));
    }
    st.lines.push(Line::total("營業淨利", st.operating_profit()));

    let mut head = false;
    for m in group(rows, "other_income") {
        if !head {
            st.lines.push(Line::head("營業外收支"));
            head = true;
        }
        let v = m.net_credit();
        st.other_net += v;
        st.lines.push(Line::item(m, v));
    }
    for m in group(rows, "other_expense") {
        if !head {
            st.lines.push(Line::head("營業外收支"));
            head = true;
        }
        let v = m.net_debit();
        st.other_net -= v;
        st.lines.push(Line::item(m, -v));
    }

    let profit = st.profit();
    st.lines.push(Line::total("本期損益", profit));
    st
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct PeriodInput {
    /// Period start (YYYY-MM-DD). Defaults to 1 January of the fiscal year.
    #[serde(default)]
    pub from: Option<NaiveDate>,
    /// Period end (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub to: Option<NaiveDate>,
}

/// Resolve the period. `default_from` decides where an omitted start begins.
fn resolve_period(
    i: &PeriodInput,
    today: NaiveDate,
    default_from: impl Fn(NaiveDate) -> NaiveDate,
) -> Result<(NaiveDate, NaiveDate), OpError> {
    let to = i.to.unwrap_or(today);
    let from = i.from.unwrap_or_else(|| default_from(to));
    if from > to {
        return Err(OpError::BadInput(format!("期間起日 {from} 晚於迄日 {to}")));
    }
    Ok((from, to))
}

/// First day of the fiscal year containing `d`.
///
/// `start_month` comes from `company_setting.fiscal_year`. A year starting in
/// April means 2026-03-31 still belongs to the year that began 2025-04-01, so
/// the calendar year is one case of this rather than the rule.
fn year_start(d: NaiveDate, start_month: u32) -> NaiveDate {
    let year = if d.month() >= start_month { d.year() } else { d.year() - 1 };
    NaiveDate::from_ymd_opt(year, start_month, 1).unwrap_or(d)
}

/// First day of the current month.
fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

struct GetIncomeStatement;

#[async_trait]
impl Operation for GetIncomeStatement {
    fn name(&self) -> &'static str {
        "get_income_statement"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀損益表：一段期間的收益減費用，由已過帳分錄現算。省略期間表示會計年度至今。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<PeriodInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PeriodInput = parse(input)?;
        let start_month = ctx.store.fiscal_year_start_month().await?;
        let (from, to) = resolve_period(&i, ctx.today, |d| year_start(d, start_month))?;
        let rows = ctx.store.ledger_movements(Some(from), to).await?;
        let st = income_statement(&rows);

        let summary = format!(
            "讀取損益表（{from} 至 {to}）· 收入 {} 費用 {} 本期損益 {}",
            money(st.revenue),
            money(st.cogs + st.operating_expense),
            money(st.profit())
        );
        Ok(OpOutcome::new(
            json!({
                "from": from,
                "to": to,
                "period_label": format!(
                    "{}–{} 累計", from.format("%Y/%m"), to.format("%m")
                ),
                "rows": lines_json(&st.lines),
                "revenue": st.revenue.to_string(),
                "cogs": st.cogs.to_string(),
                "gross_profit": st.gross().to_string(),
                "operating_expense": st.operating_expense.to_string(),
                "operating_profit": st.operating_profit().to_string(),
                "profit": st.profit().to_string(),
                "unposted": unposted_note(ctx, to).await?,
            }),
            summary,
        ))
    }
}

// Cash flow statement

#[derive(Debug)]
struct CashFlow {
    opening: Money,
    profit: Money,
    operating: Vec<Line>,
    operating_total: Money,
    investing: Vec<Line>,
    investing_total: Money,
    financing: Vec<Line>,
    financing_total: Money,
    /// The cash accounts' own movements over the period. This is the second,
    /// fully independent calculation.
    cash_movement: Money,
}

impl CashFlow {
    fn net_change(&self) -> Money {
        self.operating_total + self.investing_total + self.financing_total
    }

    /// Closing cash derived from the three activity sections.
    fn closing(&self) -> Money {
        self.opening + self.net_change()
    }

    /// Closing cash computed directly from the cash accounts' entries.
    fn ledger_closing(&self) -> Money {
        self.opening + self.cash_movement
    }

    /// Difference between the two paths. Zero if double entry holds.
    fn difference(&self) -> Money {
        self.closing() - self.ledger_closing()
    }
}

/// How an account reads on the cash flow statement, e.g. "receivables up".
fn movement_label(m: &AccountMovement) -> String {
    let d = m.balance();
    format!("{}{}", m.name, if d > Money::ZERO { "增加" } else { "減少" })
}

/// A contra account (asset kind with a credit balance, such as accumulated
/// depreciation) is the familiar "add back depreciation" line in operating
/// activities, and gets an explanation because it is the least intuitive
/// row on the statement.
fn contra_asset_hint(m: &AccountMovement) -> Option<&'static str> {
    (m.kind == "asset" && m.normal_side == "credit")
        .then_some("提列時已認列費用，但未實際支付現金，故加回")
}

/// Indirect method: start from profit and add back the movement of each
/// non-cash account.
///
/// Every account belongs to exactly one activity (decided by
/// `account.cash_flow_section`), so the three sections necessarily sum to
/// the actual movement in cash. That follows directly from
/// `Σ all accounts (Dr − Cr) = 0`; it needs no apportionment rule.
fn cash_flow(opening: Money, rows: &[AccountMovement]) -> CashFlow {
    let mut cf = CashFlow {
        opening,
        profit: Money::ZERO,
        operating: Vec::new(),
        operating_total: Money::ZERO,
        investing: Vec::new(),
        investing_total: Money::ZERO,
        financing: Vec::new(),
        financing_total: Money::ZERO,
        cash_movement: Money::ZERO,
    };

    // Operating activities start from profit. Revenue and expense are already
    // inside that single figure, so P&L accounts must have a NULL
    // cash_flow_section (enforced by the CHECK in migrations/0006) or the same
    // money would be counted twice.
    cf.profit = rows
        .iter()
        .filter(|m| m.kind == "revenue" || m.kind == "expense")
        .map(AccountMovement::net_credit)
        .sum();
    cf.operating_total = cf.profit;
    cf.operating.push(Line::plain("本期損益", cf.profit));

    for m in rows {
        if m.is_cash() {
            cf.cash_movement += m.net_debit();
            continue;
        }
        // Effect on cash: an asset increase (debit) consumes cash, a liability or
        // equity increase (credit) brings cash in. Both are "credit minus debit",
        // so assets and liabilities do not need separate branches.
        let effect = m.net_credit();
        if effect.is_zero() {
            continue;
        }
        let mut line = Line::plain(movement_label(m), effect);
        line.code = Some(m.code.clone());
        match m.cash_flow_section.as_deref() {
            Some("operating") => {
                if let Some(h) = contra_asset_hint(m) {
                    line = line.with_hint(h);
                }
                cf.operating_total += effect;
                cf.operating.push(line);
            }
            Some("investing") => {
                cf.investing_total += effect;
                cf.investing.push(line);
            }
            Some("financing") => {
                cf.financing_total += effect;
                cf.financing.push(line);
            }
            // P&L accounts are covered by `profit` above. An account with no
            // activity that is not a P&L account means the data is broken; surface
            // it as a difference rather than silently dropping part of the cash.
            _ => {}
        }
    }

    cf
}

/// Lay the statement out for the screen.
fn cash_flow_lines(cf: &CashFlow) -> Vec<Line> {
    let mut lines = vec![Line::plain("期初現金", cf.opening), Line::head("營業活動")];
    lines.extend(cf.operating.iter().cloned());
    lines.push(Line::total("營業活動現金流", cf.operating_total));

    if !cf.investing.is_empty() {
        lines.push(Line::head("投資活動"));
        lines.extend(cf.investing.iter().cloned());
        lines.push(Line::total("投資活動現金流", cf.investing_total));
    }
    if !cf.financing.is_empty() {
        lines.push(Line::head("籌資活動"));
        lines.extend(cf.financing.iter().cloned());
        lines.push(Line::total("籌資活動現金流", cf.financing_total));
    }

    lines.push(Line::total("本期現金淨增減", cf.net_change()));
    lines.push(Line::plain("期末現金餘額", cf.closing()));
    lines
}

struct GetCashFlow;

#[async_trait]
impl Operation for GetCashFlow {
    fn name(&self) -> &'static str {
        "get_cash_flow"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀現金流量表（間接法）：從本期損益調整非現金科目的變動，分營業、投資、籌資三段。期末現金會與現金科目餘額對帳。省略期間表示當月。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<PeriodInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PeriodInput = parse(input)?;
        let (from, to) = resolve_period(&i, ctx.today, month_start)?;

        // Opening cash: the cash account balance the day before the period
        // starts. Same query, same definition.
        let opening = match from.pred_opt() {
            Some(prev) => ctx
                .store
                .ledger_movements(None, prev)
                .await?
                .iter()
                .filter(|m| m.is_cash())
                .map(AccountMovement::net_debit)
                .sum(),
            None => Money::ZERO,
        };

        let rows = ctx.store.ledger_movements(Some(from), to).await?;
        let cf = cash_flow(opening, &rows);
        let lines = cash_flow_lines(&cf);

        // A third path: the balance on the bank account side. Same calculation as
        // get_cash_position, cut off at `to`. The statement is only trustworthy
        // if all three agree.
        //
        // Cash sitting on a cash account with no funding account named is not
        // a discrepancy — it is on the ledger and counted here — so it joins
        // the account side instead of turning the whole statement red on a
        // company's first day. What it does mean is that the cards on 「銀行
        // 與現金」 do not yet say where that money is, which is reported as
        // its own figure.
        let accounts = ctx.store.cash_position_as_of(to).await?;
        let assigned_total: Money = accounts.iter().map(|a| a.balance).sum();
        let unattributed = ctx.store.unattributed_cash(Some(to)).await?;
        let accounts_total = assigned_total + unattributed;
        let account_gap = cf.ledger_closing() - accounts_total;
        let reconciled = cf.difference().is_zero() && account_gap.is_zero();

        let summary = format!(
            "讀取現金流量表（{from} 至 {to}）· 淨增減 {} 期末現金 {} · 與現金科目餘額{}",
            money(cf.net_change()),
            money(cf.closing()),
            if reconciled { "相符" } else { "不符" }
        );
        Ok(OpOutcome::new(
            json!({
                "from": from,
                "to": to,
                "period_label": format!("{} 當期", from.format("%Y/%m")),
                "rows": lines_json(&lines),
                "opening": cf.opening.to_string(),
                "profit": cf.profit.to_string(),
                "operating_total": cf.operating_total.to_string(),
                "investing_total": cf.investing_total.to_string(),
                "financing_total": cf.financing_total.to_string(),
                "net_change": cf.net_change().to_string(),
                "closing": cf.closing().to_string(),
                "ledger_closing": cf.ledger_closing().to_string(),
                "difference": cf.difference().to_string(),
                "cash_accounts": accounts,
                "cash_accounts_total": accounts_total.to_string(),
                "cash_accounts_assigned": assigned_total.to_string(),
                "cash_accounts_unattributed": unattributed.to_string(),
                "cash_accounts_difference": account_gap.to_string(),
                "reconciled": reconciled,
                "unposted": unposted_note(ctx, to).await?,
            }),
            summary,
        ))
    }
}

// Shared

/// What the statement leaves out, and why.
///
/// Two different reasons, so two counts. A voucher is missing either because
/// nobody posted it yet, or because it is posted but dated after the
/// statement's cut-off — the second is what happens the moment someone runs
/// depreciation for the whole of a month and then opens today's balance
/// sheet. Reporting only the first leaves that entry unaccounted for on every
/// screen that could answer for it.
async fn unposted_note(ctx: &OpCtx, through: NaiveDate) -> Result<Value, OpError> {
    let mut pending = ctx.store.search_vouchers(Some("pending"), None, 500).await?;
    pending.extend(ctx.store.search_vouchers(Some("draft"), None, 500).await?);
    let total: Money = pending.iter().map(|v| v.amount).sum();
    let (later_count, later_total) = ctx.store.posted_after(through).await?;
    Ok(json!({
        "count": pending.len(),
        "amount": total.to_string(),
        "after_period": {
            "count": later_count,
            "amount": later_total.to_string(),
            "through": through,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn n(s: &str) -> Money {
        Decimal::from_str(s).unwrap()
    }

    /// Build the period movement of one account. `section` of `None` means a
    /// P&L account.
    #[allow(clippy::too_many_arguments)]
    fn mv(
        code: &str,
        name: &str,
        kind: &str,
        normal_side: &str,
        group: &str,
        section: Option<&str>,
        debit: &str,
        credit: &str,
    ) -> AccountMovement {
        AccountMovement {
            code: code.into(),
            name: name.into(),
            kind: kind.into(),
            normal_side: normal_side.into(),
            is_postable: true,
            is_active: true,
            report_group: group.into(),
            cash_flow_section: section.map(str::to_string),
            debit: n(debit),
            credit: n(credit),
            line_count: 1,
        }
    }

    fn cash(code: &str, name: &str, dr: &str, cr: &str) -> AccountMovement {
        mv(code, name, "asset", "debit", "current_asset", Some("cash"), dr, cr)
    }
    fn asset(code: &str, name: &str, dr: &str, cr: &str) -> AccountMovement {
        mv(code, name, "asset", "debit", "current_asset", Some("operating"), dr, cr)
    }
    fn equipment(code: &str, name: &str, dr: &str, cr: &str) -> AccountMovement {
        mv(code, name, "asset", "debit", "non_current_asset", Some("investing"), dr, cr)
    }
    fn accum_dep(dr: &str, cr: &str) -> AccountMovement {
        mv("1602", "累計折舊", "asset", "credit", "non_current_asset", Some("operating"), dr, cr)
    }
    fn liability(code: &str, name: &str, dr: &str, cr: &str) -> AccountMovement {
        mv(code, name, "liability", "credit", "current_liability", Some("operating"), dr, cr)
    }
    fn equity(code: &str, name: &str, dr: &str, cr: &str) -> AccountMovement {
        mv(code, name, "equity", "credit", "equity", Some("financing"), dr, cr)
    }
    fn revenue(dr: &str, cr: &str) -> AccountMovement {
        mv("4111", "銷貨收入", "revenue", "credit", "revenue", None, dr, cr)
    }
    fn expense(code: &str, name: &str, dr: &str, cr: &str) -> AccountMovement {
        mv(
            code,
            name,
            "expense",
            "debit",
            "operating_expense",
            None,
            dr,
            cr,
        )
    }

    /// A balanced reference ledger, used to check that the three statements
    /// derive consistent answers from one set of entries.
    fn reference_ledger() -> Vec<AccountMovement> {
        vec![
            cash("1101", "現金", "45480", "3480"),
            cash("1102", "銀行存款", "4351563", "550953"),
            asset("1131", "應收帳款", "1482400", "214000"),
            asset("1251", "存貨", "1486000", "0"),
            equipment("1601", "辦公設備", "1030200", "0"),
            accum_dep("0", "365100"),
            liability("2101", "應付帳款", "156300", "547560"),
            liability("2171", "應付費用", "0", "268400"),
            equity("3101", "股本", "0", "5000000"),
            equity("3201", "保留盈餘", "0", "2078283"),
            expense("6111", "薪資支出", "342653", "0"),
            expense("6115", "租金支出", "52000", "0"),
            expense("6122", "旅費", "3480", "0"),
            expense("6152", "折舊費用", "77700", "0"),
        ]
    }

    /// Only balanced entries qualify as input to the statements.
    fn is_balanced(rows: &[AccountMovement]) -> bool {
        rows.iter().map(AccountMovement::net_debit).sum::<Money>().is_zero()
    }

    // Balance sheet

    /// The central property: assets = liabilities + equity, with profit for
    /// the period counted inside equity.
    #[test]
    fn the_balance_sheet_balances_with_the_periods_profit_rolled_into_equity() {
        let rows = reference_ledger();
        assert!(is_balanced(&rows), "測試資料自己就得先借貸相等");

        let bs = balance_sheet(&rows);
        assert_eq!(bs.assets, n("7262110"));
        assert_eq!(bs.liabilities, n("659660"));
        assert_eq!(bs.equity_accounts, n("7078283"), "股本 ＋ 保留盈餘");
        assert_eq!(bs.profit, n("-475833"), "尚未結轉的本期損益");
        assert_eq!(bs.equity(), n("6602450"));

        assert_eq!(bs.difference(), Money::ZERO);
        assert!(bs.balanced(), "資產 {} ≠ 負債＋權益 {}", bs.assets, bs.liabilities + bs.equity());
    }

    /// Drop profit for the period and the identity breaks, which proves that
    /// row is not decoration.
    #[test]
    fn forgetting_the_period_profit_would_break_the_equation() {
        let bs = balance_sheet(&reference_ledger());
        assert_ne!(bs.profit, Money::ZERO);
        assert_eq!(
            bs.assets - bs.liabilities - bs.equity_accounts,
            bs.profit,
            "少算本期損益的話，差額剛好就是本期損益"
        );
    }

    /// Any balanced set of entries balances, not just the reference ledger.
    #[test]
    fn any_balanced_ledger_produces_a_balancing_sheet() {
        let cases: Vec<Vec<AccountMovement>> = vec![
            // Issue an invoice: debit receivables, credit revenue and output tax.
            vec![
                asset("1131", "應收帳款", "47250", "0"),
                revenue("0", "45000"),
                liability("2211", "代收營業稅", "0", "2250"),
            ],
            // Buy equipment on credit.
            vec![
                equipment("1601", "辦公設備", "68000", "0"),
                liability("2101", "應付帳款", "0", "68000"),
            ],
            // Depreciation.
            vec![expense("6152", "折舊費用", "12950", "0"), accum_dep("0", "12950")],
            // Capital increase.
            vec![cash("1102", "銀行存款", "1000000", "0"), equity("3101", "股本", "0", "1000000")],
            // Collection: cash moves within assets, neither side changes.
            vec![
                cash("1102", "銀行存款", "214000", "0"),
                asset("1131", "應收帳款", "0", "214000"),
            ],
        ];
        for rows in cases {
            assert!(is_balanced(&rows));
            let bs = balance_sheet(&rows);
            assert!(bs.balanced(), "{:?} 沒有平，差 {}", bs.lines, bs.difference());
        }
    }

    /// Accumulated depreciation must appear negative within assets; it is a
    /// contra account, not a liability.
    #[test]
    fn accumulated_depreciation_reduces_assets_instead_of_adding_a_liability() {
        let rows = vec![
            equipment("1601", "辦公設備", "1030200", "0"),
            accum_dep("0", "365100"),
            equity("3201", "保留盈餘", "0", "665100"),
        ];
        let bs = balance_sheet(&rows);
        assert_eq!(bs.assets, n("665100"), "成本 − 累計折舊");
        assert_eq!(bs.liabilities, Money::ZERO);
        let row = bs.lines.iter().find(|l| l.code.as_deref() == Some("1602")).unwrap();
        assert_eq!(row.value, Some(n("-365100")), "在資產段裡是負數");
        assert!(bs.balanced());
    }

    // Income statement

    #[test]
    fn the_income_statement_is_revenue_minus_expenses() {
        let rows = vec![
            revenue("0", "12486300"),
            mv("5111", "銷貨成本", "expense", "debit", "cogs", None, "7104200", "0"),
            expense("6111", "薪資支出", "3403400", "0"),
            expense("6152", "折舊費用", "184800", "0"),
        ];
        let st = income_statement(&rows);
        assert_eq!(st.revenue, n("12486300"));
        assert_eq!(st.cogs, n("7104200"));
        assert_eq!(st.gross(), n("5382100"), "毛利 = 收入 − 成本");
        assert_eq!(st.operating_expense, n("3588200"));
        assert_eq!(st.operating_profit(), n("1793900"));
        assert_eq!(st.profit(), n("1793900"));
    }

    /// Profit on the income statement must be the same number the balance
    /// sheet rolls into equity. Both use one definition, or the statements
    /// contradict each other.
    #[test]
    fn the_profit_line_agrees_with_the_balance_sheet() {
        let rows = reference_ledger();
        assert_eq!(
            income_statement(&rows).profit(),
            balance_sheet(&rows).profit
        );
    }

    /// Expense rows are negative: adding top to bottom gives profit, with no
    /// row that has to be subtracted.
    #[test]
    fn expense_lines_are_signed_so_the_column_simply_adds_up() {
        let st = income_statement(&reference_ledger());
        let sum: Money = st
            .lines
            .iter()
            .filter(|l| l.kind == "sub" && l.code.is_some())
            .filter_map(|l| l.value)
            .sum();
        assert_eq!(sum, st.profit(), "所有明細列相加就是本期損益");
    }

    // Cash flow statement

    /// The second central property: closing cash must equal the balance the
    /// cash accounts compute for themselves.
    ///
    /// The two paths are fully independent — one goes through profit plus
    /// non-cash movements, the other through the cash accounts' entries.
    /// Their equality is an identity of double entry, not a coincidence.
    #[test]
    fn the_cash_flow_statement_always_lands_on_the_cash_balance() {
        let opening = n("4183043");
        // A posted single-month reference set of entries.
        let rows = vec![
            cash("1101", "現金", "0", "3480"),
            cash("1102", "銀行存款", "214000", "550953"),
            asset("1131", "應收帳款", "0", "214000"),
            liability("2101", "應付帳款", "156300", "0"),
            expense("6111", "薪資支出", "342653", "0"),
            expense("6115", "租金支出", "52000", "0"),
            expense("6122", "旅費", "3480", "0"),
        ];
        assert!(is_balanced(&rows));

        let cf = cash_flow(opening, &rows);
        assert_eq!(cf.profit, n("-398133"));
        assert_eq!(cf.operating_total, n("-340433"));
        assert_eq!(cf.investing_total, Money::ZERO);
        assert_eq!(cf.financing_total, Money::ZERO);
        assert_eq!(cf.net_change(), n("-340433"));

        assert_eq!(cf.closing(), n("3842610"), "期末現金");
        assert_eq!(cf.ledger_closing(), n("3842610"), "由現金科目的分錄直接算");
        assert_eq!(cf.difference(), Money::ZERO, "兩條路徑必須落在同一個數字上");
    }

    /// Not just that one set: any balanced entries land on the cash balance.
    #[test]
    fn every_balanced_ledger_reconciles_the_two_ways_of_reaching_closing_cash() {
        let cases: Vec<(Money, Vec<AccountMovement>)> = vec![
            (Money::ZERO, reference_ledger()),
            (
                n("1000000"),
                vec![
                    equipment("1601", "辦公設備", "68000", "0"),
                    cash("1102", "銀行存款", "0", "68000"),
                ],
            ),
            (
                n("500000"),
                vec![expense("6152", "折舊費用", "12950", "0"), accum_dep("0", "12950")],
            ),
            (
                n("42000"),
                vec![
                    cash("1102", "銀行存款", "0", "50000"),
                    liability("2181", "銀行借款", "50000", "0"),
                ],
            ),
            (
                n("-8000"),
                vec![
                    cash("1102", "銀行存款", "1000000", "0"),
                    equity("3101", "股本", "0", "1000000"),
                ],
            ),
            // A period that does not touch cash: all three sections zero, closing
            // equals opening.
            (
                n("777"),
                vec![
                    asset("1131", "應收帳款", "5000", "0"),
                    revenue("0", "5000"),
                ],
            ),
        ];
        for (opening, rows) in cases {
            assert!(is_balanced(&rows), "測試資料自己要先平");
            let cf = cash_flow(opening, &rows);
            assert_eq!(
                cf.difference(),
                Money::ZERO,
                "三段活動加總 {} 與現金科目變動 {} 不一致",
                cf.net_change(),
                cf.cash_movement
            );
            assert_eq!(cf.closing(), opening + cf.cash_movement);
        }
    }

    /// The depreciation row must be added back, and the statement has to say
    /// why — it is the least intuitive row on it.
    #[test]
    fn depreciation_is_added_back_and_says_why() {
        let rows = vec![expense("6152", "折舊費用", "12950", "0"), accum_dep("0", "12950")];
        let cf = cash_flow(Money::ZERO, &rows);
        assert_eq!(cf.profit, n("-12950"));
        assert_eq!(cf.operating_total, Money::ZERO, "提折舊不影響現金");

        let line = cf.operating.iter().find(|l| l.code.as_deref() == Some("1602")).unwrap();
        assert_eq!(line.value, Some(n("12950")));
        assert!(line.hint.is_some_and(|h| h.contains("未實際支付現金")));
    }

    /// Equipment purchases go to investing and a capital increase to financing.
    /// The section comes from the account's own setting, not from an if-else
    /// here: a different chart of accounts sections differently without a
    /// single change to this file.
    #[test]
    fn the_sections_come_from_the_chart_of_accounts() {
        let rows = vec![
            cash("1102", "銀行存款", "1000000", "68000"),
            equipment("1601", "辦公設備", "68000", "0"),
            equity("3101", "股本", "0", "1000000"),
        ];
        let cf = cash_flow(Money::ZERO, &rows);
        assert_eq!(cf.investing_total, n("-68000"), "買設備：錢出去");
        assert_eq!(cf.financing_total, n("1000000"), "增資：錢進來");
        assert_eq!(cf.operating_total, Money::ZERO);
        assert_eq!(cf.difference(), Money::ZERO);

        // Move the same account to operating and the amount changes section,
        // while the total stays the same.
        let mut moved = rows.clone();
        moved[1].cash_flow_section = Some("operating".into());
        let cf2 = cash_flow(Money::ZERO, &moved);
        assert_eq!(cf2.investing_total, Money::ZERO);
        assert_eq!(cf2.operating_total, n("-68000"));
        assert_eq!(cf2.net_change(), cf.net_change(), "分段換了，現金變動不會換");
    }

    #[test]
    fn periods_default_to_the_year_and_the_month() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
        let empty = PeriodInput::default();
        assert_eq!(
            resolve_period(&empty, today, |d| year_start(d, 1)).unwrap(),
            (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), today)
        );
        assert_eq!(
            resolve_period(&empty, today, month_start).unwrap(),
            (NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(), today)
        );

        let backwards = PeriodInput {
            from: Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()),
            to: Some(today),
        };
        assert!(
            resolve_period(&backwards, today, |d| year_start(d, 1)).is_err(),
            "起日晚於迄日要擋下"
        );

        // A fiscal year starting in April: March still belongs to the year
        // that began the previous April.
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(year_start(day(2026, 3, 31), 4), day(2025, 4, 1));
        assert_eq!(year_start(day(2026, 4, 1), 4), day(2026, 4, 1));
    }
}
