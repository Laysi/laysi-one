//! Accounts receivable: ageing, outstanding invoices, recording receipts.
//!
//! `ar_invoice` is the subsidiary ledger answering who owes this, against
//! which invoice, due when. The balance of record is the ledger alone: the
//! outstanding total must equal the posted balance of 1131. [`GetArAging`]
//! computes both sides on every call and surfaces any mismatch.
//!
//! [`RecordReceipt`] creates an unposted receipt voucher (debit bank, credit
//! receivables) instead of flipping the invoice to "paid" — a status column
//! can move while the ledger cannot, and then the two disagree. Outstanding
//! totals do not change until the voucher is posted; the screen flags the
//! receipt as awaiting posting.
//!
//! [`CreateArInvoice`] stays here rather than in `masters.rs`: receivables are
//! transactions, and not every one of them gets an e-invoice. It shares the
//! store's "subsidiary row plus unposted voucher" transaction with
//! `issue_einvoice`, so B2B monthly settlement and overseas customers do not
//! have to pretend an e-invoice exists.

use crate::{money, parse, schema_of, Channel, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_store::{ArInvoice, CreateArInvoice as StoreCreateArInvoice};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetArAging),
        Box::new(ListReceivables),
        Box::new(CreateArInvoice),
        Box::new(RecordReceipt),
    ]
}

/// Inclusive upper bound of an ageing bucket. `None` means the last bucket
/// is open-ended.
///
/// Not a regulation value (those all live in `backend/regulation/`) but a
/// presentation grouping for the report.
const BUCKETS: [(&str, Option<i32>); 4] = [
    ("未到期", Some(0)),
    ("逾期 1–30 天", Some(30)),
    ("逾期 31–60 天", Some(60)),
    ("逾期 61 天以上", None),
];

/// Assign an invoice to a bucket. A negative `days_overdue` means not yet
/// due.
fn bucket_of(days_overdue: i32) -> usize {
    BUCKETS
        .iter()
        .position(|(_, upper)| upper.is_none_or(|u| days_overdue <= u))
        .unwrap_or(BUCKETS.len() - 1)
}

/// Only invoices that reached the ledger count: if the source voucher is
/// unposted, the ledger does not carry it yet.
fn recognised(invoices: &[ArInvoice]) -> impl Iterator<Item = &ArInvoice> {
    invoices.iter().filter(|i| i.recognised)
}

struct GetArAging;

#[async_trait]
impl Operation for GetArAging {
    fn name(&self) -> &'static str {
        "get_ar_aging"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀應收帳齡：未到期與各逾期區間的未收金額，並與 1131 應收帳款的總帳餘額對帳"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::AsOfInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: crate::AsOfInput = parse(input)?;
        let as_of = i.as_of.unwrap_or(ctx.today);
        let invoices = ctx.store.ar_invoices(as_of, false, None, 500).await?;

        let mut totals = [Money::ZERO; BUCKETS.len()];
        let mut counts = [0i64; BUCKETS.len()];
        for inv in recognised(&invoices) {
            let b = bucket_of(inv.days_overdue);
            totals[b] += inv.outstanding;
            counts[b] += 1;
        }

        // Two independent paths: the subsidiary ledger sums itself, the general
        // ledger computes its own balance. Equality is a conclusion, not an
        // assumption, so any difference is reported rather than swallowed.
        let detail = ctx.store.ar_outstanding_total().await?;
        let gl = ctx.store.balance_of(&["1131"], true).await?;
        let diff = detail - gl;

        // Invoices whose source voucher is unposted: the screen has to explain
        // why they are not in the total.
        let waiting: Money = invoices.iter().filter(|i| !i.recognised).map(|i| i.outstanding).sum();

        let buckets: Vec<Value> = BUCKETS
            .iter()
            .enumerate()
            .map(|(n, (label, _))| {
                json!({ "label": label, "value": totals[n].to_string(), "count": counts[n] })
            })
            .collect();

        let summary = format!(
            "讀取應收帳齡 · {} 家未收 {} · 與 1131 總帳{}",
            recognised(&invoices).count(),
            money(detail),
            if diff.is_zero() { "相符" } else { "不符" }
        );
        Ok(OpOutcome::new(
            json!({
                "as_of": as_of,
                "buckets": buckets,
                "total": detail.to_string(),
                "gl_balance": gl.to_string(),
                "difference": diff.to_string(),
                "reconciled": diff.is_zero(),
                "not_yet_posted": waiting.to_string(),
                "overdue_count": recognised(&invoices).filter(|i| i.days_overdue > 0).count(),
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListReceivablesInput {
    /// Keyword matched against customer name and invoice number.
    #[serde(default)]
    pub q: Option<String>,
    /// Only invoices past their payment terms.
    #[serde(default)]
    pub only_overdue: bool,
    /// Include fully settled invoices; excluded by default.
    #[serde(default)]
    pub include_settled: bool,
    /// Maximum rows, default 200.
    #[serde(default)]
    pub limit: Option<i64>,
}

struct ListReceivables;

#[async_trait]
impl Operation for ListReceivables {
    fn name(&self) -> &'static str {
        "list_receivables"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀應收明細：客戶、發票號碼、到期日與未收金額，逾期者予以標示"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListReceivablesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListReceivablesInput = parse(input)?;
        let limit = i.limit.unwrap_or(200).clamp(1, 500);
        let mut rows =
            ctx.store.ar_invoices(ctx.today, i.include_settled, i.q.as_deref(), limit).await?;
        if i.only_overdue {
            rows.retain(|r| r.days_overdue > 0);
        }

        let total: Money = recognised(&rows).map(|r| r.outstanding).sum();
        let overdue = rows.iter().filter(|r| r.days_overdue > 0 && r.recognised).count();
        let summary =
            format!("查詢應收 · {} 筆未收 {} · 逾期 {overdue} 筆", rows.len(), money(total));
        Ok(OpOutcome::new(
            json!({ "invoices": rows, "total": total.to_string(), "overdue_count": overdue }),
            summary,
        ))
    }
}

fn default_receivable_account() -> String {
    "1131".into()
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateArInvoiceInput {
    /// Receivable document number; it need not be an e-invoice track number.
    pub invoice_no: String,
    /// Customer name.
    pub customer: String,
    /// Description.
    #[serde(default)]
    pub memo: String,
    /// Issue date. Defaults to today.
    #[serde(default)]
    pub issued_on: Option<NaiveDate>,
    /// Due date.
    pub due_on: NaiveDate,
    /// Total receivable. Amounts are strings in JSON.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub amount: Money,
    /// Credit revenue account.
    pub revenue_account_code: String,
    /// Receivables control account, default 1131.
    #[serde(default = "default_receivable_account")]
    pub receivable_account_code: String,
}

fn validate_ar_invoice(input: &CreateArInvoiceInput, issued_on: NaiveDate) -> Result<(), OpError> {
    if input.invoice_no.trim().is_empty() || input.customer.trim().is_empty() {
        return Err(OpError::BadInput("應收單號與客戶都要填寫".into()));
    }
    if input.amount <= Money::ZERO {
        return Err(OpError::BadInput("應收金額必須大於零".into()));
    }
    if input.due_on < issued_on {
        return Err(OpError::BadInput("到期日不得早於發生日".into()));
    }
    if input.revenue_account_code.trim().is_empty()
        || input.receivable_account_code.trim().is_empty()
    {
        return Err(OpError::BadInput(
            "收益科目與應收控制科目都要填寫，建立應收時會同時產生傳票".into(),
        ));
    }
    Ok(())
}

struct CreateArInvoice;

#[async_trait]
impl Operation for CreateArInvoice {
    fn name(&self) -> &'static str {
        "create_ar_invoice"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "建立不需電子發票的應收單據，並一併產生待過帳傳票（借應收帳款／貸指定收益科目）。適用 B2B 月結與國外客戶；傳票過帳後才列入未收合計。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateArInvoiceInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let input: CreateArInvoiceInput = parse(input)?;
        let issued_on = input.issued_on.unwrap_or(ctx.today);
        validate_ar_invoice(&input, issued_on)?;
        let invoice = ctx
            .store
            .create_ar_invoice(StoreCreateArInvoice {
                invoice_no: input.invoice_no.trim(),
                customer: input.customer.trim(),
                issued_on,
                due_on: input.due_on,
                amount: input.amount,
                memo: input.memo.trim(),
                receivable_account_code: input.receivable_account_code.trim(),
                revenue_account_code: input.revenue_account_code.trim(),
                created_by: &ctx.actor,
            })
            .await?;
        let no = invoice.voucher_no.clone();
        let voucher = ctx.store.voucher_by_no(&no).await?;
        let summary = format!(
            "建立應收 {} · {} {} · {} 待過帳",
            invoice.invoice_no,
            invoice.customer,
            money(invoice.amount),
            invoice.voucher_no
        );
        Ok(OpOutcome::new(
            json!({ "invoice": invoice, "voucher": voucher }),
            summary,
        )
        .with_voucher(no))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct RecordReceiptInput {
    /// Invoice number, e.g. KX-38472910.
    pub invoice_no: String,
    /// Amount received. Defaults to the full outstanding balance of the
    /// invoice. A string in JSON.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub amount: Option<Money>,
    /// Value date (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub received_on: Option<NaiveDate>,
    /// Which account received the money, matching `bank_account.id`.
    /// Defaults to the first bank account.
    #[serde(default)]
    pub bank_account_id: Option<i32>,
}

struct RecordReceipt;

#[async_trait]
impl Operation for RecordReceipt {
    fn name(&self) -> &'static str {
        "record_receipt"
    }
    fn risk(&self) -> Risk {
        // Produces an unposted voucher that a person still has to post, the
        // same tier as draft_voucher. The operation that actually moves money
        // is create_payment_batch, which is high risk.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "登記客戶付款：產生一張待過帳的收款傳票（借銀行存款／貸應收帳款）。過帳後未收金額才會減少。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RecordReceiptInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RecordReceiptInput = parse(input)?;
        let received_on = i.received_on.unwrap_or(ctx.today);
        let bank = i.bank_account_id.unwrap_or(1);
        // UI means a person recorded it on screen, MCP means AI drafted it.
        // The audit trail has to tell them apart.
        let source = if ctx.channel == Channel::Ui { "manual" } else { "ai_draft" };

        let r = ctx
            .store
            .record_receipt(&i.invoice_no, i.amount, received_on, bank, source, &ctx.actor)
            .await?;

        let summary = format!(
            "{} · 收{} {} · {} 待過帳",
            r.invoice_no,
            r.customer,
            money(r.amount),
            r.voucher_no
        );
        let no = r.voucher_no.clone();
        let voucher = ctx.store.voucher_by_no(&no).await?;
        Ok(OpOutcome::new(json!({ "receipt": r, "voucher": voucher }), summary).with_voucher(no))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    /// Bucket boundaries are a reporting grouping and must not be approximate:
    /// an invoice is not overdue on its due date.
    #[test]
    fn aging_buckets_split_on_the_stated_boundaries() {
        assert_eq!(BUCKETS[bucket_of(-40)].0, "未到期");
        assert_eq!(BUCKETS[bucket_of(0)].0, "未到期", "到期當天還不算逾期");
        assert_eq!(BUCKETS[bucket_of(1)].0, "逾期 1–30 天");
        assert_eq!(BUCKETS[bucket_of(30)].0, "逾期 1–30 天");
        assert_eq!(BUCKETS[bucket_of(31)].0, "逾期 31–60 天");
        assert_eq!(BUCKETS[bucket_of(60)].0, "逾期 31–60 天");
        assert_eq!(BUCKETS[bucket_of(61)].0, "逾期 61 天以上");
        assert_eq!(BUCKETS[bucket_of(9999)].0, "逾期 61 天以上");
    }

    #[test]
    fn create_ar_invoice_rejects_a_due_date_before_issue() {
        let issued_on = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let input = CreateArInvoiceInput {
            invoice_no: "AR-001".into(),
            customer: "測試客戶".into(),
            memo: String::new(),
            issued_on: Some(issued_on),
            due_on: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            amount: Decimal::from_str("1").unwrap(),
            revenue_account_code: "4111".into(),
            receivable_account_code: "1131".into(),
        };
        assert!(
            validate_ar_invoice(&input, issued_on)
                .unwrap_err()
                .to_string()
                .contains("到期日")
        );
        assert_eq!(CreateArInvoice.risk(), Risk::NeedsConfirm);
        assert!(CreateArInvoice.summary().contains("B2B"));
    }
}
