//! Vouchers and the general ledger.
//!
//! Covers the dashboard, trial balance, cash position, voucher search and
//! creation, posting, voiding, reversal, the chart of accounts, the audit
//! trail and ruleset versions. Other areas live in their own modules and are
//! registered in `ops/mod.rs`.

use crate::{
    money, parse, schema_of, AsOfInput, Channel, Empty, OpCtx, OpError, OpOutcome, Operation, Risk,
};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::LineInput;
use laysi_domain::Money;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetDashboard),
        Box::new(GetTrialBalance),
        Box::new(GetCashPosition),
        Box::new(SearchVouchers),
        Box::new(ListAccounts),
        Box::new(ListAiAudit),
        Box::new(DraftVoucher),
        Box::new(CreateVoucher),
        Box::new(PostVoucher),
        Box::new(VoidVoucher),
        Box::new(DiscardVoucher),
        Box::new(ReverseVoucher),
    ]
}

struct GetDashboard;

#[async_trait]
impl Operation for GetDashboard {
    fn name(&self) -> &'static str {
        "get_dashboard"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀總覽：現金、應收、應付、本月損益，以及待處理事項與最近的操作紀錄"
    }
    fn input_schema(&self) -> Value {
        schema_of::<Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let s = &ctx.store;
        let cash = s.balance_of(&["1101", "1102"], true).await?;
        let ar = s.balance_of(&["1131"], true).await?;
        let ap = s.balance_of(&["2101"], false).await?;

        let month_start = ctx.today.with_day(1).unwrap_or(ctx.today);
        let profit = s.profit_between(month_start, ctx.today).await?;

        let pending = s.search_vouchers(Some("pending"), None, 500).await?.len();
        let drafts = s.search_vouchers(Some("draft"), None, 500).await?.len();
        let bars = s.cash_history(ctx.today, 6).await?;
        let audit = s.recent_audit(3, None, None).await?;
        // Every figure above is drawn from posted vouchers only, so with none of
        // them the four stats are zero for want of data rather than of money.
        let started = !s.search_vouchers(Some("posted"), None, 1).await?.is_empty();

        let mut todos = Vec::new();
        if pending > 0 {
            todos.push(json!({
                "count": pending.to_string(), "label": "張傳票已建立但尚未過帳",
                "due": "影響報表", "screen": "gl"
            }));
        }
        if drafts > 0 {
            todos.push(json!({
                "count": drafts.to_string(), "label": "張 AI 草稿待確認會計科目",
                "due": "確認後才入帳", "screen": "gl"
            }));
        }

        let data = json!({
            "stats": [
                { "kicker": "銀行＋現金",   "value": money(cash),   "note": "各帳戶合計，僅計已過帳" },
                { "kicker": "應收帳款", "value": money(ar),     "note": "應收帳款餘額" },
                { "kicker": "應付帳款", "value": money(ap),     "note": "應付帳款餘額" },
                { "kicker": "本月損益",     "value": money(profit), "note": "未過帳傳票將影響本數" }
            ],
            "todos": todos,
            "ledger_started": started,
            "cash_bars": bars.iter().map(|(m, v)| json!({
                "month": m, "value": v.to_string(), "label": money(*v)
            })).collect::<Vec<_>>(),
            // Named "recent", not "AI": every call is audited, and on a
            // company's first day every row is the reader's own click. A list
            // headed 「AI 操作紀錄」 showing nothing but the owner's own name
            // reads as something impersonating them.
            "recent": audit.iter().map(|a| json!({
                "what": a.result_summary,
                "time": laysi_domain::business_time(a.at, ctx.offset).format("%m/%d %H:%M").to_string(),
                "who": a.actor,
                "channel": a.channel,
                "channel_label": if a.channel == "mcp" { "AI" } else { "人員" },
                "outcome": a.outcome,
            })).collect::<Vec<_>>(),
        });
        Ok(OpOutcome::new(
            data,
            format!("讀取總覽 · 現金 {}", money(cash)),
        ))
    }
}

struct GetTrialBalance;

#[async_trait]
impl Operation for GetTrialBalance {
    fn name(&self) -> &'static str {
        "get_trial_balance"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀試算表：各科目的借貸方發生額與餘額，只計已過帳傳票"
    }
    fn input_schema(&self) -> Value {
        schema_of::<AsOfInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: AsOfInput = parse(input)?;
        let rows = ctx.store.trial_balance(i.as_of).await?;
        let td: Money = rows.iter().map(|r| r.debit).sum();
        let tc: Money = rows.iter().map(|r| r.credit).sum();
        let summary = format!(
            "讀取試算表 · {} 個科目 · 借貸{}",
            rows.len(),
            if td == tc { "相等" } else { "不等" }
        );
        Ok(OpOutcome::new(
            json!({
                "rows": rows,
                "total_debit": td.to_string(),
                "total_credit": tc.to_string(),
                "balanced": td == tc,
            }),
            summary,
        ))
    }
}

struct GetCashPosition;

#[async_trait]
impl Operation for GetCashPosition {
    fn name(&self) -> &'static str {
        "get_cash_position"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀各銀行帳戶與零用金的現金水位。合計等於總帳的現金餘額；尚未指定資金帳戶的金額另列一筆。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let accounts = ctx.store.cash_position().await?;
        let assigned: Money = accounts.iter().map(|a| a.balance).sum();
        // Cash that landed on a cash account without naming one. Petty cash
        // has no account row, and a GL code shared by two accounts cannot be
        // split by inference; either way the money is on the ledger, so the
        // cards have to add up to the ledger rather than to each other.
        let unattributed = ctx.store.unattributed_cash(None).await?;
        let total = assigned + unattributed;
        let names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
        let summary = format!(
            "讀取現金水位 · {} · 合計 {}",
            names.join("、"),
            money(total)
        );
        Ok(OpOutcome::new(
            json!({
                "accounts": accounts,
                "assigned_total": assigned.to_string(),
                "unattributed": unattributed.to_string(),
                "total": total.to_string(),
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct SearchVouchersInput {
    /// Voucher status: draft (AI drafted), pending (awaiting posting),
    /// posted, or void.
    #[serde(default)]
    pub status: Option<String>,
    /// Keyword matched against memo and voucher number.
    #[serde(default)]
    pub q: Option<String>,
    /// Maximum rows, default 50.
    #[serde(default)]
    pub limit: Option<i64>,
}

struct SearchVouchers;

#[async_trait]
impl Operation for SearchVouchers {
    fn name(&self) -> &'static str {
        "search_vouchers"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "查傳票與分錄明細，可依狀態與關鍵字篩選"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SearchVouchersInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SearchVouchersInput = parse(input)?;
        let limit = i.limit.unwrap_or(50).clamp(1, 500);
        let vouchers = ctx
            .store
            .search_vouchers(i.status.as_deref(), i.q.as_deref(), limit)
            .await?;
        let summary = format!("查詢傳票 · 回傳 {} 筆", vouchers.len());
        Ok(OpOutcome::new(json!({ "vouchers": vouchers }), summary))
    }
}

struct ListAccounts;

#[async_trait]
impl Operation for ListAccounts {
    fn name(&self) -> &'static str {
        "list_accounts"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀會計科目表：代號、名稱、類別與正常餘額方向。開立分錄前應先查詢。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let accounts = ctx.store.accounts().await?;
        let summary = format!("讀取科目表 · {} 個科目", accounts.len());
        Ok(OpOutcome::new(json!({ "accounts": accounts }), summary))
    }
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListAiAuditInput {
    /// Maximum rows to return, default 50, at most 10000.
    pub limit: Option<i64>,
    /// Earliest business date to include (YYYY-MM-DD). Omit for no lower bound.
    pub from: Option<NaiveDate>,
    /// Latest business date to include (YYYY-MM-DD), inclusive.
    pub to: Option<NaiveDate>,
}

/// Midnight of a business day as the instant it happened.
///
/// `ai_audit_log.at` is a timestamp and the dates a person picks are business
/// dates, so the two bounds have to be derived with the offset this call runs
/// under. Total by construction: `NaiveTime::MIN` needs no validation.
fn day_start(day: NaiveDate, offset: chrono::FixedOffset) -> chrono::DateTime<chrono::Utc> {
    (day.and_time(chrono::NaiveTime::MIN)
        - chrono::Duration::seconds(offset.local_minus_utc().into()))
    .and_utc()
}

struct ListAiAudit;

#[async_trait]
impl Operation for ListAiAudit {
    fn name(&self) -> &'static str {
        "list_ai_audit"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀稽核紀錄：每一次操作與登入的時間、身分、來源位址、原始提問與結果。可指定日期區間，供匯出留存。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListAiAuditInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListAiAuditInput = parse(input)?;
        if let (Some(from), Some(to)) = (i.from, i.to) {
            if from > to {
                return Err(OpError::BadInput("起始日期晚於結束日期".into()));
            }
        }
        // The upper bound is the start of the day after `to`, so the last day
        // asked for is included whole.
        let entries = ctx
            .store
            .recent_audit(
                i.limit.unwrap_or(50).clamp(1, 10_000),
                i.from.map(|d| day_start(d, ctx.offset)),
                i.to.and_then(|d| d.succ_opt())
                    .map(|d| day_start(d, ctx.offset)),
            )
            .await?;
        let summary = format!("讀取稽核紀錄 · {} 筆", entries.len());
        Ok(OpOutcome::new(json!({ "entries": entries }), summary))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct DraftVoucherInput {
    /// Voucher date (YYYY-MM-DD).
    pub date: NaiveDate,
    /// Memo, one line written for a human reader.
    pub memo: String,
    /// Journal lines. At least two, and debits must equal credits.
    pub lines: Vec<LineInput>,
}

struct DraftVoucher;

#[async_trait]
impl Operation for DraftVoucher {
    fn name(&self) -> &'static str {
        "draft_voucher"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "草擬分錄，存為待確認傳票。不會過帳，要由人員確認科目後才生效。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<DraftVoucherInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DraftVoucherInput = parse(input)?;
        let no = ctx
            .store
            .create_voucher(
                i.date, &i.memo, &i.lines, "draft", "ai_draft", &ctx.actor, None,
            )
            .await?;
        let v = ctx.store.voucher_by_no(&no).await?;
        let summary = format!("{} · {} {}", no, v.accounts_label, money(v.amount));
        Ok(OpOutcome::new(json!({ "voucher": v }), summary).with_voucher(no))
    }
}

struct CreateVoucher;

#[async_trait]
impl Operation for CreateVoucher {
    fn name(&self) -> &'static str {
        "create_voucher"
    }
    fn risk(&self) -> Risk {
        // Creates an unposted voucher directly, a step beyond a draft. Off for
        // AI by default; accounting staff on screen are unaffected (see
        // [`Channel`]).
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "直接開立待過帳傳票（人工記帳用）。AI 請改用草擬分錄。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<DraftVoucherInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DraftVoucherInput = parse(input)?;
        let source = if ctx.channel == Channel::Ui {
            "manual"
        } else {
            "ai_draft"
        };
        let no = ctx
            .store
            .create_voucher(
                i.date, &i.memo, &i.lines, "pending", source, &ctx.actor, None,
            )
            .await?;
        let v = ctx.store.voucher_by_no(&no).await?;
        let summary = format!("{} · {} {}", no, v.accounts_label, money(v.amount));
        Ok(OpOutcome::new(json!({ "voucher": v }), summary).with_voucher(no))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct PostVoucherInput {
    /// Voucher number, e.g. V2607-018.
    pub no: String,
}

struct PostVoucher;

#[async_trait]
impl Operation for PostVoucher {
    fn name(&self) -> &'static str {
        "post_voucher"
    }
    fn risk(&self) -> Risk {
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "將傳票過帳。不應存在的錯帳請作廢；交易確實發生但記載錯誤請開立反向傳票。預設關閉。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<PostVoucherInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PostVoucherInput = parse(input)?;
        let v = ctx.store.post_voucher(&i.no).await?;
        let summary = format!("{} 已過帳 · {}", v.no, money(v.amount));
        let no = v.no.clone();
        Ok(OpOutcome::new(json!({ "voucher": v }), summary).with_voucher(no))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct VoidVoucherInput {
    /// Number of the posted voucher to void.
    pub no: String,
    /// Reason for voiding. Required, and written into the audit summary.
    pub reason: String,
}

struct VoidVoucher;

pub(crate) fn require_void_reason(reason: &str) -> Result<&str, OpError> {
    let reason = reason.trim();
    if reason.is_empty() {
        Err(OpError::BadInput("作廢理由必填，且不得僅含空白".into()))
    } else {
        Ok(reason)
    }
}

#[async_trait]
impl Operation for VoidVoucher {
    fn name(&self) -> &'static str {
        "void_voucher"
    }
    fn risk(&self) -> Risk {
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "作廢不應存在的已過帳傳票（如重複過帳或交易對象完全錯誤）；交易確實發生但金額或科目記載錯誤時，請開立反向傳票。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<VoidVoucherInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: VoidVoucherInput = parse(input)?;
        let reason = require_void_reason(&i.reason)?;

        let voucher = ctx.store.void_voucher(&i.no).await?;
        let summary = format!("{} 已作廢 · 理由：{reason}", voucher.no);
        let no = voucher.no.clone();
        Ok(OpOutcome::new(json!({ "voucher": voucher }), summary).with_voucher(no))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct DiscardVoucherInput {
    /// Number of the unposted voucher to delete.
    pub no: String,
}

struct DiscardVoucher;

#[async_trait]
impl Operation for DiscardVoucher {
    fn name(&self) -> &'static str {
        "discard_voucher"
    }
    fn risk(&self) -> Risk {
        // An unposted voucher has not reached the ledger, so nothing in the
        // reports moves. It is still gone for good, hence the confirmation.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "刪除一張還沒過帳的手工傳票。已過帳的不能刪除：不應存在的請作廢，記錯的請開立反向傳票。由應收、應付、薪資、折舊、報支或匯入產生的傳票也不能單獨刪除，請到產生它的那一頁處理。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<DiscardVoucherInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DiscardVoucherInput = parse(input)?;
        let voucher = ctx.store.voucher_by_no(&i.no).await?;
        ctx.store.discard_voucher(&i.no).await?;
        let summary = format!("{} 已刪除 · {} {}", voucher.no, voucher.date, voucher.memo);
        Ok(OpOutcome::new(
            json!({ "no": voucher.no, "discarded": true }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct ReverseVoucherInput {
    /// Number of the posted voucher to reverse.
    pub no: String,
    /// Date of the reversing voucher, usually the day the error was found.
    pub date: NaiveDate,
}

struct ReverseVoucher;

#[async_trait]
impl Operation for ReverseVoucher {
    fn name(&self) -> &'static str {
        "reverse_voucher"
    }
    fn risk(&self) -> Risk {
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "以指定日期開立並過帳反向傳票，更正確實發生但金額或科目記載錯誤的交易；原傳票不應存在時，請改為作廢。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ReverseVoucherInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ReverseVoucherInput = parse(input)?;
        let reversal = ctx.store.reverse_voucher(&i.no, i.date, &ctx.actor).await?;
        let original = ctx.store.voucher_by_no(&i.no).await?;
        let summary = format!(
            "{} 已過帳並沖銷 {} · {} · {}",
            reversal.no,
            original.no,
            reversal.date,
            money(reversal.amount)
        );
        let no = reversal.no.clone();
        Ok(OpOutcome::new(
            json!({ "voucher": reversal, "original": original }),
            summary,
        )
        .with_voucher(no))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_summaries_explain_when_to_choose_the_other_operation() {
        let void_summary = VoidVoucher.summary();
        let reverse_summary = ReverseVoucher.summary();

        assert!(void_summary.contains("不應存在"));
        assert!(void_summary.contains("反向傳票"));
        assert!(reverse_summary.contains("確實發生"));
        assert!(reverse_summary.contains("作廢"));

        // Summaries are read on screen and by the AI. Operation names are
        // internal identifiers, so the pointer to the other operation is its
        // Chinese name.
        assert!(!void_summary.contains('_'));
        assert!(!reverse_summary.contains('_'));
    }

    #[test]
    fn void_reason_is_required_by_the_public_input_schema() {
        let schema = VoidVoucher.input_schema();
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .expect("作廢輸入應列出必填欄位");
        assert!(required.iter().any(|field| field == "reason"));
    }

    #[test]
    fn void_reason_rejects_blank_text_and_is_trimmed_for_the_audit_summary() {
        let error = require_void_reason(" \n ").expect_err("空白理由必須拒絕");
        assert!(error.to_string().contains("理由必填"));
        assert_eq!(require_void_reason("  重複過帳  ").unwrap(), "重複過帳");
    }

    #[test]
    fn an_audit_range_is_bounded_by_business_midnights_and_covers_the_last_day_whole() {
        let offset = chrono::FixedOffset::east_opt(8 * 3600).expect("+08:00 是有效的偏移量");
        let from = NaiveDate::from_ymd_opt(2026, 8, 1).expect("日期必須有效");
        let to = NaiveDate::from_ymd_opt(2026, 8, 31).expect("日期必須有效");

        // Midnight local, not midnight UTC: eight hours of a picked day would
        // otherwise sit outside the range the person asked for.
        assert_eq!(
            day_start(from, offset).to_rfc3339(),
            "2026-07-31T16:00:00+00:00"
        );

        let upper = day_start(to.succ_opt().expect("次日必然存在"), offset);
        let last_moment = day_start(to, offset) + chrono::Duration::seconds(86_399);
        assert!(last_moment < upper, "結束日當天的最後一秒必須落在區間內");
        assert_eq!(upper.to_rfc3339(), "2026-08-31T16:00:00+00:00");
    }
}
