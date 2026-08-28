//! Accounts payable: outstanding bills and payment batches.
//!
//! Symmetric with receivables: `ap_bill` is the subsidiary ledger and the
//! balance of record is the GL balance of 2101. [`ListPayables`] returns and
//! compares both sides; the reasoning is in `receivables.rs`.
//!
//! Recording a receipt describes money that already arrived; a payment batch
//! instructs money to leave, and a mistake cannot be recalled. So
//! [`CreatePaymentBatch`] is `HighRisk` and is off for AI by default in the
//! migration. Accounting staff on screen go through `Channel::Ui` and are
//! unaffected by that grant.
//!
//! One payment batch produces one voucher: a debit line per bill so it stays
//! visible who was paid, and a single combined credit to the bank account,
//! matching the single outgoing transfer the bank will report.
//!
//! [`CreateApBill`] stays here rather than in `masters.rs` because a bill is a
//! transaction and must create its unposted voucher in the same transaction,
//! sharing one set of rules with outstanding balances, payment and 2101
//! reconciliation.

use crate::{money, parse, schema_of, Channel, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_store::{Account, CreateApBill as StoreCreateApBill};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListPayables),
        Box::new(CreateApBill),
        Box::new(CreatePaymentBatch),
    ]
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListPayablesInput {
    /// Keyword matched against supplier, document number and description.
    #[serde(default)]
    pub q: Option<String>,
    /// Only bills due within this many days, including overdue ones.
    /// Omit for all.
    #[serde(default)]
    pub due_within_days: Option<i32>,
    /// Include fully paid bills; excluded by default.
    #[serde(default)]
    pub include_settled: bool,
    /// Maximum rows, default 200.
    #[serde(default)]
    pub limit: Option<i64>,
}

struct ListPayables;

#[async_trait]
impl Operation for ListPayables {
    fn name(&self) -> &'static str {
        "list_payables"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀應付明細：付款對象、內容與到期日，並與 2101 應付帳款的總帳餘額對帳"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListPayablesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListPayablesInput = parse(input)?;
        let limit = i.limit.unwrap_or(200).clamp(1, 500);
        let mut rows = ctx
            .store
            .ap_bills(ctx.today, i.include_settled, i.q.as_deref(), limit)
            .await?;
        if let Some(days) = i.due_within_days {
            // days_overdue is today minus the due date, so >= -days covers bills
            // due within `days` days.
            rows.retain(|r| r.days_overdue >= -days);
        }

        let detail = ctx.store.ap_outstanding_total().await?;
        let gl = ctx.store.balance_of(&["2101"], false).await?;
        let diff = detail - gl;

        // The bill drawer offers exactly the accounts create_ap_bill accepts,
        // so the list and the rule cannot drift apart.
        let chart = ctx.store.accounts().await?;
        let debit_accounts: Vec<Value> = chart
            .iter()
            .filter(|a| is_purchase_account(a, &[PAYABLE_CONTROL_ACCOUNT, INPUT_TAX_ACCOUNT]))
            .map(|a| json!({ "code": a.code, "name": a.name }))
            .collect();

        let listed: Money = rows
            .iter()
            .filter(|r| r.recognised)
            .map(|r| r.outstanding)
            .sum();
        let waiting: Money = rows
            .iter()
            .filter(|r| !r.recognised)
            .map(|r| r.outstanding)
            .sum();
        let overdue = rows
            .iter()
            .filter(|r| r.days_overdue > 0 && r.recognised)
            .count();

        let summary = format!(
            "查詢應付 · {} 筆未付 {} · 與 2101 總帳{}",
            rows.len(),
            money(listed),
            if diff.is_zero() { "相符" } else { "不符" }
        );
        Ok(OpOutcome::new(
            json!({
                "bills": rows,
                "listed_total": listed.to_string(),
                "total": detail.to_string(),
                "gl_balance": gl.to_string(),
                "difference": diff.to_string(),
                "reconciled": diff.is_zero(),
                "not_yet_posted": waiting.to_string(),
                "overdue_count": overdue,
                "debit_accounts": debit_accounts,
                "vat": crate::ops::tax::vat_json(ctx.today)?,
            }),
            summary,
        ))
    }
}

const PAYABLE_CONTROL_ACCOUNT: &str = "2101";
const INPUT_TAX_ACCOUNT: &str = "1284";

/// Receivables control account, which a bill never debits. Its report group
/// and cash flow activity are the same as inventory's and prepayments', so
/// nothing else on the row separates them; `receivables.rs` holds the same
/// default on the other side of the ledger.
const RECEIVABLE_CONTROL_ACCOUNT: &str = "1131";

/// Report groups a bill may debit: what the company acquired. Expenses,
/// inventory and prepayments, plus non-current assets, so an asset bought on
/// credit is billed here and its voucher number written back to the register.
const PURCHASE_REPORT_GROUPS: [&str; 5] = [
    "cogs",
    "operating_expense",
    "other_expense",
    "current_asset",
    "non_current_asset",
];

const LIABILITY_REPORT_GROUPS: [&str; 2] = ["current_liability", "non_current_liability"];

const ASSET_REPORT_GROUPS: [&str; 2] = ["current_asset", "non_current_asset"];

/// Whether a bill may debit this account. `control` holds the other two
/// accounts of the same voucher, which the operation posts on its own.
///
/// Cash is excluded by its cash flow activity rather than by code, because a
/// bill that debits cash reports money arriving out of a supplier's invoice.
/// Accounts carrying a credit balance are excluded for the same reason the
/// fixed asset register separates cost from accumulated depreciation.
fn is_purchase_account(a: &Account, control: &[&str]) -> bool {
    a.is_active
        && a.is_postable
        && a.normal_side == "debit"
        && PURCHASE_REPORT_GROUPS.contains(&a.report_group.as_str())
        && a.cash_flow_section.as_deref() != Some("cash")
        && a.code != RECEIVABLE_CONTROL_ACCOUNT
        && !control.contains(&a.code.as_str())
}

/// The three accounts of the voucher a bill creates. Checked against the chart
/// rather than against the account code, so a rewritten chart stays supported.
fn check_ap_bill_accounts(
    accounts: &[Account],
    debit: &str,
    payable: &str,
    input_tax: &str,
) -> Result<(), OpError> {
    let label = |code: &str| match accounts.iter().find(|a| a.code == code) {
        Some(a) => format!("{} {}", a.code, a.name),
        None => code.to_string(),
    };
    let find = |code: &str| accounts.iter().find(|a| a.code == code);
    let usable = |a: &Account| a.is_active && a.is_postable;

    if !find(debit).is_some_and(|a| is_purchase_account(a, &[payable, input_tax])) {
        return Err(OpError::BadInput(format!(
            "「{}」不能當借方科目，請改選啟用中、可入帳的費用、存貨、預付款或固定資產科目",
            label(debit)
        )));
    }
    if !find(payable).is_some_and(|a| {
        usable(a)
            && a.normal_side == "credit"
            && LIABILITY_REPORT_GROUPS.contains(&a.report_group.as_str())
    }) {
        return Err(OpError::BadInput(format!(
            "「{}」不能當應付控制科目，請改選啟用中、可入帳的負債科目",
            label(payable)
        )));
    }
    if !find(input_tax).is_some_and(|a| {
        usable(a)
            && a.normal_side == "debit"
            && ASSET_REPORT_GROUPS.contains(&a.report_group.as_str())
    }) {
        return Err(OpError::BadInput(format!(
            "「{}」不能當進項稅額科目，請改選啟用中、可入帳的資產科目",
            label(input_tax)
        )));
    }
    Ok(())
}

fn default_payable_account() -> String {
    PAYABLE_CONTROL_ACCOUNT.into()
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateApBillInput {
    /// Supplier's bill number.
    pub bill_no: String,
    /// Supplier name.
    pub vendor: String,
    /// Bill description.
    #[serde(default)]
    pub memo: String,
    /// Bill date. Defaults to today.
    #[serde(default)]
    pub billed_on: Option<NaiveDate>,
    /// Due date.
    pub due_on: NaiveDate,
    /// Tax-inclusive total, which is what the supplier is owed. Amounts are
    /// strings in JSON. Give any one or two of `amount`, `net_amount` and
    /// `tax_amount`; the rest follow from the VAT rules in force on the bill
    /// date.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub amount: Option<Money>,
    /// Amount before tax, debited to the expense or asset account.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub net_amount: Option<Money>,
    /// Deductible input tax, debited to 1284. Send `0` for a receipt that
    /// carries no deductible tax.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub tax_amount: Option<Money>,
    /// Debit account: what the bill bought. Any active, postable expense,
    /// inventory, prepayment or non-current asset account. Cash, receivables
    /// and the other two accounts of this voucher are rejected.
    pub debit_account_code: String,
    /// Payables control account, default 2101.
    #[serde(default = "default_payable_account")]
    pub payable_account_code: String,
    /// Input tax account, default 1284. The VAT return reads the period
    /// movement of this account.
    #[serde(default = "default_input_tax_account")]
    pub input_tax_account_code: String,
}

fn default_input_tax_account() -> String {
    INPUT_TAX_ACCOUNT.into()
}

fn validate_ap_bill(input: &CreateApBillInput, billed_on: NaiveDate) -> Result<(), OpError> {
    if input.bill_no.trim().is_empty() || input.vendor.trim().is_empty() {
        return Err(OpError::BadInput("帳單號碼與供應商都要填寫".into()));
    }
    if input.due_on < billed_on {
        return Err(OpError::BadInput("到期日不得早於帳單日".into()));
    }
    if input.debit_account_code.trim().is_empty()
        || input.payable_account_code.trim().is_empty()
        || input.input_tax_account_code.trim().is_empty()
    {
        return Err(OpError::BadInput(
            "借方科目、應付控制科目與進項稅額科目都要填寫，建立帳單時會同時產生傳票".into(),
        ));
    }
    Ok(())
}

struct CreateApBill;

#[async_trait]
impl Operation for CreateApBill {
    fn name(&self) -> &'static str {
        "create_ap_bill"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "建立應付帳單，並一併產生待過帳傳票（借指定科目＋進項稅額／貸應付帳款）。進項稅額依帳單日適用的稅率拆出，不可扣抵時填 0；傳票過帳後才會列入未付合計，實際匯款另行執行。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateApBillInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let input: CreateApBillInput = parse(input)?;
        let billed_on = input.billed_on.unwrap_or(ctx.today);
        validate_ap_bill(&input, billed_on)?;
        check_ap_bill_accounts(
            &ctx.store.accounts().await?,
            input.debit_account_code.trim(),
            input.payable_account_code.trim(),
            input.input_tax_account_code.trim(),
        )?;
        let split = crate::ops::tax::split_vat(
            billed_on,
            input.net_amount,
            input.tax_amount,
            input.amount,
        )?;
        let bill = ctx
            .store
            .create_ap_bill(StoreCreateApBill {
                bill_no: input.bill_no.trim(),
                vendor: input.vendor.trim(),
                memo: input.memo.trim(),
                billed_on,
                due_on: input.due_on,
                amount: split.total,
                net_amount: split.net,
                tax_amount: split.tax,
                debit_account_code: input.debit_account_code.trim(),
                payable_account_code: input.payable_account_code.trim(),
                input_tax_account_code: input.input_tax_account_code.trim(),
                created_by: &ctx.actor,
            })
            .await?;
        let no = bill.voucher_no.clone();
        let voucher = ctx.store.voucher_by_no(&no).await?;
        let summary = format!(
            "建立應付 {} · {} 含稅 {}（未稅 {} ＋ 進項稅額 {}，依 vat {}）· {} 待過帳",
            bill.bill_no,
            bill.vendor,
            money(split.total),
            money(split.net),
            money(split.tax),
            split.version,
            bill.voucher_no
        );
        Ok(OpOutcome::new(
            json!({
                "bill": bill,
                "voucher": voucher,
                "net_amount": split.net.to_string(),
                "tax_amount": split.tax.to_string(),
                "vat_version": split.version,
            }),
            summary,
        )
        .with_voucher(no))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct CreatePaymentBatchInput {
    /// Bill numbers to pay, e.g. ["AP-2607-022", "AP-2607-023"].
    pub bill_nos: Vec<String>,
    /// Payment date (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub pay_on: Option<NaiveDate>,
    /// Which account pays, matching `bank_account.id`. Defaults to the first
    /// bank account.
    #[serde(default)]
    pub bank_account_id: Option<i32>,
}

struct CreatePaymentBatch;

#[async_trait]
impl Operation for CreatePaymentBatch {
    fn name(&self) -> &'static str {
        "create_payment_batch"
    }
    fn risk(&self) -> Risk {
        // Recording a receipt describes the past; a payment instructs the
        // future and cannot be recalled. Off for AI by default; accounting
        // staff on screen are unaffected (see [`Channel`]).
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "送出付款單：將勾選的帳單合併為一張待過帳的付款傳票（借應付帳款／貸銀行存款）。將產生實際付款，預設關閉。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreatePaymentBatchInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CreatePaymentBatchInput = parse(input)?;
        let pay_on = i.pay_on.unwrap_or(ctx.today);
        let bank = i.bank_account_id.unwrap_or(1);
        let source = if ctx.channel == Channel::Ui {
            "manual"
        } else {
            "ai_draft"
        };

        let batch = ctx
            .store
            .create_payment_batch(&i.bill_nos, pay_on, bank, source, &ctx.actor)
            .await?;

        let summary = format!(
            "{} · {} 筆合計 {} · {} 待過帳",
            batch.batch_no,
            batch.bill_count,
            money(batch.total),
            batch.voucher_no
        );
        let no = batch.voucher_no.clone();
        let voucher = ctx.store.voucher_by_no(&no).await?;
        Ok(OpOutcome::new(json!({ "batch": batch, "voucher": voucher }), summary).with_voucher(no))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn create_ap_bill_rejects_a_due_date_before_the_bill_date() {
        let billed_on = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let input = CreateApBillInput {
            bill_no: "AP-001".into(),
            vendor: "測試供應商".into(),
            memo: String::new(),
            billed_on: Some(billed_on),
            due_on: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            amount: Some(Decimal::from_str("1").unwrap()),
            net_amount: None,
            tax_amount: None,
            debit_account_code: "6199".into(),
            payable_account_code: "2101".into(),
            input_tax_account_code: "1284".into(),
        };
        assert!(validate_ap_bill(&input, billed_on)
            .unwrap_err()
            .to_string()
            .contains("到期日"));
        assert_eq!(CreateApBill.risk(), Risk::NeedsConfirm);
        assert!(CreateApBill.summary().contains("一併產生待過帳傳票"));
    }

    fn account(code: &str, name: &str, group: &str, side: &str, cash: Option<&str>) -> Account {
        let kind = match group {
            "current_asset" | "non_current_asset" => "asset",
            "current_liability" | "non_current_liability" => "liability",
            "equity" => "equity",
            "revenue" | "other_income" => "revenue",
            _ => "expense",
        };
        Account {
            code: code.into(),
            name: name.into(),
            kind: kind.into(),
            normal_side: side.into(),
            is_postable: true,
            is_active: true,
            parent_code: None,
            sort_order: 0,
            report_group: group.into(),
            cash_flow_section: cash.map(str::to_string),
        }
    }

    /// The seeded chart, which is what the screen offers.
    fn chart() -> Vec<Account> {
        vec![
            account("1101", "現金", "current_asset", "debit", Some("cash")),
            account("1102", "銀行存款", "current_asset", "debit", Some("cash")),
            account(
                "1131",
                "應收帳款",
                "current_asset",
                "debit",
                Some("operating"),
            ),
            account("1251", "存貨", "current_asset", "debit", Some("operating")),
            account(
                "1284",
                "進項稅額",
                "current_asset",
                "debit",
                Some("operating"),
            ),
            account(
                "1451",
                "預付費用",
                "current_asset",
                "debit",
                Some("operating"),
            ),
            account(
                "1601",
                "辦公設備",
                "non_current_asset",
                "debit",
                Some("investing"),
            ),
            account(
                "1602",
                "累計折舊",
                "non_current_asset",
                "credit",
                Some("operating"),
            ),
            account(
                "2101",
                "應付帳款",
                "current_liability",
                "credit",
                Some("operating"),
            ),
            account("4111", "銷貨收入", "revenue", "credit", None),
            account("5111", "銷貨成本", "cogs", "debit", None),
            account("6118", "水電費", "operating_expense", "debit", None),
        ]
    }

    fn check(debit: &str) -> Result<(), OpError> {
        check_ap_bill_accounts(&chart(), debit, "2101", "1284")
    }

    #[test]
    fn a_bill_only_debits_what_the_company_bought() {
        for code in ["6118", "5111", "1251", "1451", "1601"] {
            assert!(check(code).is_ok(), "{code} should be accepted");
        }
        for code in [
            "1101", "1102", "1131", "1284", "1602", "2101", "4111", "9999",
        ] {
            let message = check(code).unwrap_err().to_string();
            assert!(message.contains("不能當借方科目"), "{code}: {message}");
        }
    }

    #[test]
    fn a_disabled_or_roll_up_account_is_not_offered() {
        let mut chart = chart();
        chart[11].is_active = false;
        assert!(check_ap_bill_accounts(&chart, "6118", "2101", "1284").is_err());
        chart[11].is_active = true;
        chart[11].is_postable = false;
        assert!(check_ap_bill_accounts(&chart, "6118", "2101", "1284").is_err());
    }

    #[test]
    fn the_other_two_accounts_keep_their_side_of_the_voucher() {
        let payable = check_ap_bill_accounts(&chart(), "6118", "1251", "1284");
        assert!(payable
            .unwrap_err()
            .to_string()
            .contains("不能當應付控制科目"));
        let input_tax = check_ap_bill_accounts(&chart(), "6118", "2101", "2101");
        assert!(input_tax
            .unwrap_err()
            .to_string()
            .contains("不能當進項稅額科目"));
        assert!(check_ap_bill_accounts(&chart(), "6118", "2101", "1602").is_err());
    }
}
