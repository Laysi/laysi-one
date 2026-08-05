//! E-invoices: issuing, track allocation, upload status and retry.
//!
//! The VAT rate is a regulation value and does not appear in this module.
//! Issuing always goes `resolve("vat", issue_date)` then `vat(rs, net)`, and
//! stores the version used in `einvoice.vat_version`. That column is what
//! answers "which invoices were issued under the old rate" after an
//! amendment; tax already printed on an issued invoice must not move.
//!
//! [`IssueEInvoice`] does three things in one transaction: consume a track
//! number, create an unposted voucher (debit 1131, credit 4111 and 2211), and
//! write the receivable. Splitting the tax onto 2211 is what makes the VAT
//! return reconcilable — output tax on the return is the period movement of
//! 2211.
//!
//! The tax bureau MIG interface is not wired up, so
//! [`RetryEInvoiceUpload`] only requeues the invoice (`failed` → `queued`)
//! and never marks it as uploaded.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Duration, NaiveDate};
use laysi_domain::Money;
use laysi_store::IssueEInvoice as IssueReq;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(ListEInvoices), Box::new(IssueEInvoice), Box::new(RetryEInvoiceUpload)]
}

/// The track currently in use. Multiple tracks come later; naming it once
/// keeps that decision in one place.
const TRACK: &str = "KX";

/// Default payment terms. A commercial term agreed with customers, not a
/// regulation value, which is why it may live here while the tax rate may
/// not.
const DEFAULT_DUE_DAYS: i64 = 30;

/// Human-readable track period: `2607` with a two-month cycle reads as the
/// July-August period of ROC 26.
///
/// The months per period come from the VAT rules rather than being fixed
/// here: tracks are allocated per filing period, so this label follows a
/// change in the period length.
fn track_period_label(period: &str, period_months: u32) -> String {
    let (year, month) = period.split_at(period.len().saturating_sub(2));
    let Ok(first) = month.parse::<u32>() else {
        return period.to_string();
    };
    let last = (first + period_months.max(1) - 1).min(12);
    if first == last {
        format!("{year} 年 {first} 月期")
    } else {
        format!("{year} 年 {first}-{last} 月期")
    }
}

/// Human-readable upload status.
fn status_label(status: &str) -> &'static str {
    match status {
        "uploaded" => "已上傳",
        "failed" => "上傳失敗",
        "void" => "已作廢",
        _ => "待上傳",
    }
}

struct ListEInvoices;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListEInvoicesInput {
    /// Keyword matched against buyer name, invoice number and line items.
    #[serde(default)]
    pub q: Option<String>,
    /// Include voided invoices; excluded by default.
    #[serde(default)]
    pub include_void: bool,
    /// Maximum rows, default 50.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[async_trait]
impl Operation for ListEInvoices {
    fn name(&self) -> &'static str {
        "list_einvoices"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀電子發票：字軌號碼、買方、含稅金額與上傳狀態，並回報字軌剩餘可用號碼"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListEInvoicesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListEInvoicesInput = parse(input)?;
        let limit = i.limit.unwrap_or(50).clamp(1, 200);
        let rows = ctx.store.einvoices(i.q.as_deref(), i.include_void, limit).await?;
        let track = ctx.store.einvoice_track(TRACK).await?;

        // The rate that would apply to an invoice issued now. Both the on-screen
        // estimate and the tax rate label use this value; the frontend must not
        // carry a rate of its own, which would become a second source.
        let rs = laysi_regulation::resolve("vat", ctx.today)?;
        let rate = rs.decimal("rate")?;
        let period_months: u32 = rs.decimal("period_months")?.trunc().try_into().unwrap_or(1);
        let track_label = track_period_label(&track.period, period_months);

        let failed = rows.iter().filter(|e| e.upload_status == "failed").count();
        let queued = rows.iter().filter(|e| e.upload_status == "queued").count();
        let listed: Vec<Value> = rows
            .iter()
            .map(|e| {
                let mut v = serde_json::to_value(e).unwrap_or_else(|_| json!({}));
                v["status_label"] = json!(status_label(&e.upload_status));
                v
            })
            .collect();

        // Gross total of issued invoices. This is how much was invoiced, not
        // what the books show as revenue: revenue lives in 4111 and tax in
        // 2211, both reconciled on the VAT return.
        let issued_total: Money =
            rows.iter().filter(|e| e.upload_status != "void").map(|e| e.total_amount).sum();

        let summary = format!(
            "讀取電子發票 · {} 張合計 {} · 上傳失敗 {failed} 張 · 字軌 {} 剩 {} 號",
            rows.len(),
            money(issued_total),
            track.track,
            track.remaining
        );
        Ok(OpOutcome::new(
            json!({
                "invoices": listed,
                "track": track,
                "track_label": track_label,
                "issued_total": issued_total.to_string(),
                "failed_count": failed,
                "queued_count": queued,
                "vat": {
                    "version": rs.version,
                    "rate": rate.to_string(),
                    "rate_percent": crate::ops::tax::percent(rate),
                    "effective_from": rs.effective_from,
                    "source_note": rs.source_note,
                },
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct IssueEInvoiceInput {
    /// Buyer name.
    pub buyer: String,
    /// Buyer tax ID. Leave blank for individual buyers.
    #[serde(default)]
    pub buyer_tax_id: Option<String>,
    /// Line item description.
    pub item: String,
    /// Net amount. VAT is computed from the rate in force on the issue date;
    /// do not send a gross amount.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub net_amount: Money,
    /// Issue date (YYYY-MM-DD). Defaults to today; the rate resolves on it.
    #[serde(default)]
    pub issued_on: Option<NaiveDate>,
    /// Payment terms in days. Defaults to 30.
    #[serde(default)]
    pub due_in_days: Option<i64>,
}

struct IssueEInvoice;

#[async_trait]
impl Operation for IssueEInvoice {
    fn name(&self) -> &'static str {
        "issue_einvoice"
    }
    fn risk(&self) -> Risk {
        // Produces an unposted voucher that a person still has to post, the
        // same tier as record_receipt. But it also consumes a track number,
        // which cannot be taken back, so it is certainly not read-only.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "開立電子發票：營業稅依開立日適用的稅率計算，同時產生待過帳的應收傳票（借應收帳款／貸銷貨收入＋代收營業稅）"
    }
    fn input_schema(&self) -> Value {
        schema_of::<IssueEInvoiceInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: IssueEInvoiceInput = parse(input)?;
        if i.net_amount <= Money::ZERO {
            return Err(OpError::BadInput("未稅金額要大於零".into()));
        }
        let issued_on = i.issued_on.unwrap_or(ctx.today);
        let due_on = issued_on + Duration::days(i.due_in_days.unwrap_or(DEFAULT_DUE_DAYS).max(0));

        // The rate has one source: the VAT ruleset resolved on the issue date.
        // Without one, issuing is refused rather than guessed.
        let rs = laysi_regulation::resolve("vat", issued_on)?;
        let tax = laysi_regulation::vat(rs, i.net_amount)?;

        let issued = ctx
            .store
            .issue_einvoice(IssueReq {
                track: TRACK,
                buyer_tax_id: i.buyer_tax_id.as_deref().unwrap_or(""),
                buyer: &i.buyer,
                item: &i.item,
                issued_on,
                due_on,
                net_amount: i.net_amount,
                tax_amount: tax,
                vat_version: &rs.version,
                created_by: &ctx.actor,
            })
            .await?;

        // The audit summary names the version: which rules an invoice's tax was
        // computed under must be visible from the audit trail alone.
        let summary = format!(
            "{} · {} 含稅 {}（未稅 {} ＋ 營業稅 {}，依 vat {}）· {} 待過帳",
            issued.invoice_no,
            issued.buyer,
            money(issued.total_amount),
            money(issued.net_amount),
            money(issued.tax_amount),
            issued.vat_version,
            issued.voucher_no
        );
        let no = issued.voucher_no.clone();
        let voucher = ctx.store.voucher_by_no(&no).await?;
        Ok(OpOutcome::new(
            json!({
                "invoice": issued,
                "voucher": voucher,
                "vat": {
                    "version": rs.version,
                    "effective_from": rs.effective_from,
                    "source_note": rs.source_note,
                },
            }),
            summary,
        )
        .with_voucher(no))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct RetryEInvoiceUploadInput {
    /// Invoice number, e.g. KX-38472918.
    pub invoice_no: String,
}

struct RetryEInvoiceUpload;

#[async_trait]
impl Operation for RetryEInvoiceUpload {
    fn name(&self) -> &'static str {
        "retry_einvoice_upload"
    }
    fn risk(&self) -> Risk {
        // Touches no books, but resends a statutory document to the tax bureau.
        // A duplicate submission is harder to unwind than a missing one.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "將上傳失敗的電子發票重新排入上傳佇列。僅變更上傳狀態，不影響帳務；平台實際接收前不會標示為已上傳。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RetryEInvoiceUploadInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RetryEInvoiceUploadInput = parse(input)?;
        let e = ctx.store.retry_einvoice_upload(&i.invoice_no).await?;
        let summary = format!(
            "{} · {} {} 已重新排入上傳佇列（第 {} 次）",
            e.invoice_no,
            e.buyer,
            money(e.total_amount),
            e.upload_attempts
        );
        Ok(OpOutcome::new(json!({ "invoice": e }), summary))
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

    /// The central property: the tax is computed by the rules, not the code.
    ///
    /// The same net 45,000 invoice issued in 2026 and in 2027 yields different
    /// tax without a single change to the code; only the ruleset `resolve`
    /// returns differs.
    #[test]
    fn the_tax_on_an_invoice_comes_from_the_ruleset_not_from_this_file() {
        let rs2026 = laysi_regulation::resolve("vat", d(2026, 7, 25)).unwrap();
        assert_eq!(laysi_regulation::vat(rs2026, n("45000")).unwrap(), n("2250"));

        // Simulate the VAT rate rising to 8% in 2027 by adding a ruleset with a
        // later effective_from.
        let mut rs2027 = rs2026.clone();
        rs2027.version = "2027.1".into();
        rs2027.effective_from = d(2027, 1, 1);
        rs2027.payload["rate"] = json!("0.08");

        assert_eq!(laysi_regulation::vat(&rs2027, n("45000")).unwrap(), n("3600"));

        // Results under the old rules are unaffected: invoices already issued
        // and filed do not change because the law changed later.
        assert_eq!(laysi_regulation::vat(rs2026, n("45000")).unwrap(), n("2250"));
        assert_ne!(rs2026.version, rs2027.version, "版本要能分辨，否則追不出哪張用舊版");
    }

    /// The reference case: net 45,000 gives a gross of exactly 47,250.
    #[test]
    fn a_reference_invoice_reproduces_its_total() {
        let rs = laysi_regulation::resolve("vat", d(2026, 7, 23)).unwrap();
        let tax = laysi_regulation::vat(rs, n("45000")).unwrap();
        assert_eq!(tax, n("2250"));
        assert_eq!(n("45000") + tax, n("47250"));
    }

    /// The period length comes from the VAT rules, not this file; switching to
    /// monthly filing changes the track label too.
    #[test]
    fn the_track_label_follows_the_filing_period_length() {
        assert_eq!(track_period_label("2607", 2), "26 年 7-8 月期");
        assert_eq!(track_period_label("2601", 2), "26 年 1-2 月期");
        assert_eq!(track_period_label("2611", 2), "26 年 11-12 月期");
        assert_eq!(track_period_label("2607", 1), "26 年 7 月期");
    }

    #[test]
    fn upload_states_read_as_chinese() {
        assert_eq!(status_label("uploaded"), "已上傳");
        assert_eq!(status_label("failed"), "上傳失敗");
        assert_eq!(status_label("void"), "已作廢");
        assert_eq!(status_label("queued"), "待上傳");
    }
}
