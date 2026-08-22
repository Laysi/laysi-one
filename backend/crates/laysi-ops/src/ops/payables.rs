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
use laysi_store::CreateApBill as StoreCreateApBill;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(ListPayables), Box::new(CreateApBill), Box::new(CreatePaymentBatch)]
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
        let mut rows =
            ctx.store.ap_bills(ctx.today, i.include_settled, i.q.as_deref(), limit).await?;
        if let Some(days) = i.due_within_days {
            // days_overdue is today minus the due date, so >= -days covers bills
            // due within `days` days.
            rows.retain(|r| r.days_overdue >= -days);
        }

        let detail = ctx.store.ap_outstanding_total().await?;
        let gl = ctx.store.balance_of(&["2101"], false).await?;
        let diff = detail - gl;

        let listed: Money = rows.iter().filter(|r| r.recognised).map(|r| r.outstanding).sum();
        let waiting: Money = rows.iter().filter(|r| !r.recognised).map(|r| r.outstanding).sum();
        let overdue = rows.iter().filter(|r| r.days_overdue > 0 && r.recognised).count();

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
                "vat": crate::ops::tax::vat_json(ctx.today)?,
            }),
            summary,
        ))
    }
}

fn default_payable_account() -> String {
    "2101".into()
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
    /// Debit account: any active, postable expense, inventory or prepayment
    /// account.
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
    "1284".into()
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
        let split =
            crate::ops::tax::split_vat(billed_on, input.net_amount, input.tax_amount, input.amount)?;
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
        let source = if ctx.channel == Channel::Ui { "manual" } else { "ai_draft" };

        let batch =
            ctx.store.create_payment_batch(&i.bill_nos, pay_on, bank, source, &ctx.actor).await?;

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
        assert!(validate_ap_bill(&input, billed_on).unwrap_err().to_string().contains("到期日"));
        assert_eq!(CreateApBill.risk(), Risk::NeedsConfirm);
        assert!(CreateApBill.summary().contains("一併產生待過帳傳票"));
    }
}
