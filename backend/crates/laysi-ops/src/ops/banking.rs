//! Banking and cash: bank transactions, match candidates, reconciliation.
//!
//! `bank_txn` holds what the bank reported, not our entries. Reconciling links
//! a transaction to a voucher and produces no journal entry at all, so the
//! account balance still has exactly one source (`get_cash_position`, derived
//! from entries). A transaction cannot be reconciled twice and a voucher
//! cannot be claimed by two transactions; both are enforced by SQL UNIQUE
//! constraints, and this layer only turns the violation into a readable error.
//!
//! Candidates are ranked by amount difference first and date difference
//! second — value dates drift, amounts do not. Every candidate carries its
//! `amount_gap` and `day_gap` rather than an unverifiable similarity score.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use laysi_domain::Money;
use laysi_store::{BankTxn, MatchCandidate};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(ListBankTransactions), Box::new(SuggestBankMatches), Box::new(MatchBankTransaction)]
}

/// One sentence describing how close a candidate is. Written for a person,
/// so it gives the reason rather than a score.
fn reason(c: &MatchCandidate, txn: &BankTxn) -> String {
    let same_amount = c.amount_gap.is_zero();
    let days = c.day_gap.abs();
    let base = match (same_amount, days) {
        (true, 0) => "金額與日期均相符".to_string(),
        (true, d) if d <= 3 => format!("金額相符，日期相差 {d} 天"),
        (true, d) => format!("金額相符，但日期相差 {d} 天"),
        (false, 0) => format!("同日，金額相差 {}", money(c.amount_gap)),
        (false, _) => "可能相關".to_string(),
    };
    // Opposite directions (one in, one out) can never be the same movement
    // however close the amounts, and that has to be said.
    if (c.net_effect > Money::ZERO) != (txn.amount > Money::ZERO) {
        return format!("{base}，但方向相反");
    }
    if c.status != "posted" {
        return format!("{base}（傳票尚未過帳）");
    }
    base
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListBankTransactionsInput {
    /// Restrict to one account, matching `bank_account.id`. Omit for all.
    #[serde(default)]
    pub bank_account_id: Option<i32>,
    /// Only unreconciled transactions.
    #[serde(default)]
    pub only_unmatched: bool,
    /// Maximum rows, default 200.
    #[serde(default)]
    pub limit: Option<i64>,
}

struct ListBankTransactions;

#[async_trait]
impl Operation for ListBankTransactions {
    fn name(&self) -> &'static str {
        "list_bank_transactions"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀銀行流水：日期、摘要、金額（正數入帳負數出帳）與核銷狀態"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListBankTransactionsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListBankTransactionsInput = parse(input)?;
        let limit = i.limit.unwrap_or(200).clamp(1, 500);
        let rows =
            ctx.store.bank_transactions(i.bank_account_id, i.only_unmatched, limit).await?;

        let unmatched = rows.iter().filter(|t| !t.is_matched()).count();
        let pending: Money = rows.iter().filter(|t| !t.is_matched()).map(|t| t.amount).sum();
        let summary = format!("讀取銀行流水 · {} 筆 · 未核銷 {unmatched} 筆", rows.len());
        Ok(OpOutcome::new(
            json!({
                "transactions": rows,
                "unmatched_count": unmatched,
                "unmatched_net": pending.to_string(),
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct SuggestBankMatchesInput {
    /// Id of the bank transaction.
    pub txn_id: i32,
    /// Maximum candidates to return, default 5.
    #[serde(default)]
    pub limit: Option<i64>,
}

struct SuggestBankMatches;

#[async_trait]
impl Operation for SuggestBankMatches {
    fn name(&self) -> &'static str {
        "suggest_bank_matches"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "為一筆銀行流水尋找帳上可能對應的傳票，依金額與日期接近程度排序。僅提供建議，不執行核銷。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SuggestBankMatchesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SuggestBankMatchesInput = parse(input)?;
        let txn = ctx.store.bank_transaction(i.txn_id).await?;
        let limit = i.limit.unwrap_or(5).clamp(1, 20);
        let candidates = ctx.store.bank_match_candidates(&txn, limit).await?;

        let listed: Vec<Value> = candidates
            .iter()
            .map(|c| {
                json!({
                    "voucher_no": c.voucher_no,
                    "date": c.date,
                    "memo": c.memo,
                    "status": c.status,
                    "net_effect": c.net_effect.to_string(),
                    "amount_gap": c.amount_gap.to_string(),
                    "day_gap": c.day_gap,
                    "exact": c.amount_gap.is_zero() && c.day_gap.abs() <= 3,
                    "reason": reason(c, &txn),
                })
            })
            .collect();

        let summary = format!(
            "為 #{} {} {} 找到 {} 個候選",
            txn.id,
            txn.memo,
            money(txn.amount),
            listed.len()
        );
        Ok(OpOutcome::new(json!({ "transaction": txn, "candidates": listed }), summary))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct MatchBankTransactionInput {
    /// Id of the bank transaction.
    pub txn_id: i32,
    /// Voucher to match it to, e.g. V2607-015.
    pub voucher_no: String,
}

struct MatchBankTransaction;

#[async_trait]
impl Operation for MatchBankTransaction {
    fn name(&self) -> &'static str {
        "match_bank_transaction"
    }
    fn risk(&self) -> Risk {
        // Creates no journal entry, but asserts that two records are the same
        // movement. Getting that wrong distorts the reconciliation, so it is
        // not read-only.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "將一筆銀行流水核銷至一張傳票。不產生分錄，僅標示兩者為同一筆；已核銷者不得重複核銷。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<MatchBankTransactionInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: MatchBankTransactionInput = parse(input)?;
        let txn = ctx.store.match_bank_transaction(i.txn_id, &i.voucher_no, &ctx.actor).await?;
        let summary =
            format!("已核銷 · {} {} ↔ {}", txn.memo, money(txn.amount), i.voucher_no);
        Ok(OpOutcome::new(json!({ "transaction": txn }), summary).with_voucher(i.voucher_no))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn d(s: &str) -> Money {
        Decimal::from_str(s).unwrap()
    }

    fn txn(amount: &str) -> BankTxn {
        BankTxn {
            id: 1,
            bank_account_id: 1,
            bank_name: "示範銀行 活存".into(),
            account_no: "0016-3812-xxxx".into(),
            txn_date: NaiveDate::from_ymd_opt(2026, 7, 21).unwrap(),
            memo: "ATM 轉出 供應商貨款".into(),
            amount: d(amount),
            kind: "轉帳".into(),
            matched_voucher_no: None,
            matched_at: None,
            matched_by: None,
        }
    }

    fn candidate(net: &str, gap: &str, day_gap: i32, status: &str) -> MatchCandidate {
        MatchCandidate {
            voucher_no: "V2607-015".into(),
            date: NaiveDate::from_ymd_opt(2026, 7, 21).unwrap(),
            memo: "支付供應商貨款".into(),
            status: status.into(),
            net_effect: d(net),
            amount_gap: d(gap),
            day_gap,
        }
    }

    #[test]
    fn an_exact_match_says_so() {
        let t = txn("-156300");
        assert_eq!(reason(&candidate("-156300", "0", 0, "posted"), &t), "金額與日期均相符");
    }

    #[test]
    fn a_close_date_is_still_reported_as_a_gap() {
        let t = txn("-156300");
        assert_eq!(
            reason(&candidate("-156300", "0", 2, "posted"), &t),
            "金額相符，日期相差 2 天"
        );
    }

    /// Equal amounts in opposite directions are never the same movement; the
    /// ranking puts them first, so the check has to reject them.
    #[test]
    fn opposite_direction_is_called_out() {
        let t = txn("-156300");
        assert_eq!(
            reason(&candidate("156300", "312600", 0, "posted"), &t),
            "同日，金額相差 312,600，但方向相反"
        );
    }

    /// An unposted voucher can be reconciled, but the user must be told it is
    /// not in the statements yet.
    #[test]
    fn an_unposted_voucher_is_flagged() {
        let t = txn("-8420");
        assert_eq!(
            reason(&candidate("-8420", "0", 1, "pending"), &t),
            "金額相符，日期相差 1 天（傳票尚未過帳）"
        );
    }
}
