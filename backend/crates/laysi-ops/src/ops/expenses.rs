//! Expense claims: receipt, account confirmed by accounting, approvals, and
//! finally an expense voucher.
//!
//! Approval thresholds are company policy rather than regulation, so they live
//! in `approval_rule.min_amount` (created in 0006); this module contains no
//! amounts at all. [`required_steps`] does one thing: compare the claim amount
//! against that table to derive the steps it must pass.
//!
//! The account suggested on submission and the account decided by a human are separate
//! columns. Keeping both is what makes it possible to measure how accurate the
//! suggestions are, and to tell which claims a person actually reviewed.
//!
//! The voucher is created at the final approval, in the same transaction as
//! the approval record. It is created as `pending`: approving a claim and
//! posting it to the ledger are two decisions, matching the pattern used for
//! receipts, payments and depreciation. The expense line carries the
//! claimant's department so departmental P&L sees the cost.
//!
//! Steps can be deleted while a claim is in flight, which leaves a claim with
//! every rule it must pass already recorded and no step to approve. The
//! voucher is then the only thing outstanding, so approving such a claim goes
//! straight to it rather than reporting that there is nothing left to do:
//! there is, and no other operation creates it.
//!
//! A claim repaid at month end credits 2101 and therefore also becomes an
//! `ap_bill` in that same transaction. Payables reconciles its detail against
//! the 2101 balance, so a credit with no bill puts the two permanently apart,
//! and the payment batch — the only path by which the money leaves — reads
//! that same table.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_store::{ApprovalRule, ExpenseClaim, SubmitExpenseClaim};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListExpenseClaims),
        Box::new(SubmitExpenseClaimOp),
        Box::new(ApproveExpenseClaim),
        Box::new(RejectExpenseClaim),
    ]
}

/// The two ways a claim gets paid back.
///
/// Self-service offers the same two, so the wording of the rejection lives
/// here rather than in both callers.
pub(crate) fn checked_pay_method(value: Option<&str>) -> Result<&str, OpError> {
    let method = value.unwrap_or("reimburse");
    if method != "reimburse" && method != "petty_cash" {
        return Err(OpError::BadInput(format!(
            "付款方式僅接受 reimburse（月底彙總匯款）或 petty_cash（零用金），收到「{method}」"
        )));
    }
    Ok(method)
}

/// Which approval steps this claim must pass.
///
/// Every rule comes from `approval_rule`: a NULL `min_amount` means always
/// required, a value means "required above this amount". There is no amount
/// in this function.
pub(crate) fn required_steps(rules: &[ApprovalRule], amount: Money) -> Vec<&ApprovalRule> {
    let mut steps: Vec<&ApprovalRule> = rules
        .iter()
        .filter(|r| r.min_amount.is_none_or(|min| amount >= min))
        .collect();
    steps.sort_by_key(|r| r.step_no);
    steps
}

/// The next unapproved step, or `None` when all are done.
fn next_step<'a>(claim: &ExpenseClaim, steps: &[&'a ApprovalRule]) -> Option<&'a ApprovalRule> {
    steps.iter().copied().find(|s| !claim.step_done(s.step_no))
}

/// Whether this identity may approve this step.
///
/// The owner may stand in for any step — small companies work that way, and
/// a claim should not stall while a manager travels. But a claimant may
/// never approve their own claim, which is the one separation-of-duties rule
/// enforceable today: comparing names is enough, no permission matrix needed.
///
/// A company whose only accounts are the administrator and the owner has no
/// second signatory, so the refusal says where one comes from. Creating an
/// account is `AdminOnly`, which is why it names the administrator rather
/// than telling the reader to go and do it.
///
/// `role_label` is the role as the screens name it; the role code never
/// reaches the message.
///
/// `step` is `None` once every rule the claim must pass is recorded and only
/// the voucher is left. There is then no role to compare against: the step
/// that named one may have been deleted while the claim was in flight, and
/// demanding some other role here would enforce a rule that appears in no row
/// of `approval_rule` — which is how the claim got stuck in the first place.
/// Who may call this operation at all is still decided by the role matrix in
/// `dispatch`, and the separation-of-duties rule below does not come from the
/// table, so it holds either way.
fn may_decide(
    step: Option<&ApprovalRule>,
    claim: &ExpenseClaim,
    actor: &str,
    role: &str,
    role_label: &str,
) -> Result<(), OpError> {
    if actor == claim.claimant {
        return Err(OpError::BadInput(format!(
            "{} 由 {} 本人送出，要由另一個帳號簽核。目前沒有其他可簽核的帳號，請系統管理員在「帳號管理」建立。",
            claim.claim_no, claim.claimant
        )));
    }
    let Some(step) = step else {
        return Ok(());
    };
    let wanted = step.role_code.as_deref().unwrap_or("");
    if role == wanted || role == crate::ROLE_BOSS {
        return Ok(());
    }
    Err(OpError::BadInput(format!(
        "第 {} 關「{}」應由{}簽核，目前身分為{}。",
        step.step_no,
        step.label,
        step.role_name.as_deref().unwrap_or(wanted),
        role_label
    )))
}

/// Which step the approval record is written on when no step is pending.
///
/// One past the highest this claim has signed. 0053 dropped the foreign key
/// into `approval_rule`, so the number is an ordinal and need not name a rule
/// that still exists — which is the point, because the case this covers is a
/// flow whose remaining steps were deleted while the claim was in it.
fn finalise_step_no(claim: &ExpenseClaim) -> i32 {
    claim.steps.iter().map(|s| s.step_no).max().unwrap_or(0) + 1
}

/// Plain-language description of where a claim is waiting.
fn step_label(claim: &ExpenseClaim, next: Option<&ApprovalRule>) -> String {
    match claim.status.as_str() {
        "approved" => "已入帳".into(),
        "rejected" => "已退回".into(),
        _ => match next {
            Some(s) => match s.role_code.as_deref() {
                Some("acct") => "待確認會計科目".into(),
                Some("manager") => "待主管核准".into(),
                Some("boss") => "待負責人核准".into(),
                _ => format!("待{}", s.label),
            },
            None => "待產生傳票".into(),
        },
    }
}

/// JSON shape of a claim. The flow status is derived, not a stored string.
pub(crate) fn claim_json(claim: &ExpenseClaim, rules: &[ApprovalRule]) -> Value {
    let steps = required_steps(rules, claim.amount);
    let next = next_step(claim, &steps);
    json!({
        "claim_no": claim.claim_no,
        "title": claim.title,
        "claimant": claim.claimant,
        "dept_code": claim.dept_code,
        "dept_label": claim.dept_label,
        "spent_on": claim.spent_on,
        "amount": claim.amount.to_string(),
        "suggested_account_code": claim.suggested_account_code,
        "suggested_account_name": claim.suggested_account_name,
        "account_code": claim.account_code,
        "account_name": claim.account_name,
        "account_label": claim
            .account_code
            .as_ref()
            .zip(claim.account_name.as_ref())
            .or_else(|| claim.suggested_account_code.as_ref().zip(claim.suggested_account_name.as_ref()))
            .map(|(c, n)| format!("{c} {n}")),
        // Was the account decided by a person, or only suggested on submission? The screen
        // must distinguish them.
        "account_confirmed": claim.account_code.is_some(),
        "pay_method": claim.pay_method,
        "pay_method_label": if claim.pay_method == "petty_cash" { "零用金即時支付" } else { "月底彙總匯款" },
        "status": claim.status,
        "step_label": step_label(claim, next),
        "next_step": next.map(|s| json!({
            "step_no": s.step_no,
            "label": s.label,
            "role_code": s.role_code,
            "role_name": s.role_name,
            "min_amount": s.min_amount.map(|m| m.to_string()),
        })),
        "required_steps": steps.iter().map(|s| json!({
            "step_no": s.step_no,
            "label": s.label,
            "role_code": s.role_code,
            "role_name": s.role_name,
            "condition_note": s.condition_note,
            "min_amount": s.min_amount.map(|m| m.to_string()),
            "done": claim.step_done(s.step_no),
            // Why this step applies: over the threshold, or always required.
            "reason": match s.min_amount {
                None => "全部單據皆需經過".to_string(),
                Some(m) => format!("金額超過 {}", money(m)),
            },
        })).collect::<Vec<_>>(),
        "history": claim.steps,
        "voucher_no": claim.voucher_no,
        "voucher_status": claim.voucher_status,
        "document_no": claim.document_no,
        "document_name": claim.document_name,
        "memo": claim.memo,
        "created_at": claim.created_at,
        "created_by": claim.created_by,
    })
}

// Listing

struct ListExpenseClaims;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListExpenseClaimsInput {
    /// Include settled and rejected claims; by default only pending ones.
    #[serde(default)]
    pub include_done: bool,
    /// Maximum rows, default 50.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[async_trait]
impl Operation for ListExpenseClaims {
    fn name(&self) -> &'static str {
        "list_expense_claims"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀費用報支單：各單目前關卡、應經關卡（門檻查自公司簽核規則）、送單時建議的科目與人員確認的科目"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListExpenseClaimsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListExpenseClaimsInput = parse(input)?;
        let rules = ctx.store.approval_rules().await?;
        let claims = ctx
            .store
            .expense_claims(i.include_done, i.limit.unwrap_or(50))
            .await?;

        let waiting: Money = claims
            .iter()
            .filter(|c| c.status == "submitted" || c.status == "approving")
            .map(|c| c.amount)
            .sum();
        // Whether this identity can take the next step is decided here rather
        // than on the screen: the rule stays in one place, and the button can
        // be drawn dead with the reason beside it instead of being live and
        // answering with a refusal.
        let role_label = crate::role_name(ctx).await;
        let rows: Vec<Value> = claims
            .iter()
            .map(|c| {
                let mut row = claim_json(c, &rules);
                let steps = required_steps(&rules, c.amount);
                let next = next_step(c, &steps);
                // A claim still in flight always has something to do, even
                // with no step left: the voucher. Settled claims have none.
                let in_flight = c.status == "submitted" || c.status == "approving";
                let blocked = in_flight
                    .then(|| {
                        may_decide(next, c, &ctx.actor, &ctx.actor_role, &role_label)
                            .err()
                            .map(|e| e.to_string())
                    })
                    .flatten();
                row["can_approve"] = json!(in_flight && blocked.is_none());
                row["approve_blocked_reason"] = json!(blocked);
                row
            })
            .collect();

        let summary = format!(
            "讀取費用報支 · {} 張待處理合計 {} · 門檻依公司簽核規則",
            claims
                .iter()
                .filter(|c| c.status == "submitted" || c.status == "approving")
                .count(),
            money(waiting)
        );
        Ok(OpOutcome::new(
            json!({
                "claims": rows,
                "waiting_total": waiting.to_string(),
                "waiting_count": claims
                    .iter()
                    .filter(|c| c.status == "submitted" || c.status == "approving")
                    .count(),
                "flow": rules.iter().map(|r| json!({
                    "step_no": r.step_no,
                    "label": r.label,
                    "role_code": r.role_code,
                    "role_name": r.role_name,
                    "condition_note": r.condition_note,
                    "min_amount": r.min_amount.map(|m| m.to_string()),
                    "always": r.min_amount.is_none(),
                })).collect::<Vec<_>>(),
                // Thresholds are data, not code. The screen states where they
                // come from.
                "thresholds_source": "公司自訂的簽核規則",
                "actor": ctx.actor,
                "actor_role": ctx.actor_role,
            }),
            summary,
        ))
    }
}

// Submitting a claim

struct SubmitExpenseClaimOp;

#[derive(Deserialize, JsonSchema)]
pub struct SubmitExpenseClaimInput {
    /// What the money was spent on.
    pub title: String,
    /// Amount, tax inclusive.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub amount: Money,
    /// Spend date (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub spent_on: Option<NaiveDate>,
    /// Claimant. Defaults to the current actor.
    #[serde(default)]
    pub claimant: Option<String>,
    /// Department code.
    #[serde(default)]
    pub dept_code: Option<String>,
    /// Expense account suggested by whoever submitted the claim. A person decides the
    /// final account at the accounting step.
    #[serde(default)]
    pub suggested_account_code: Option<String>,
    /// `reimburse` paid with the month-end run (default), or `petty_cash`
    /// paid immediately.
    #[serde(default)]
    pub pay_method: Option<String>,
    /// Document number of the receipt (upload it with upload_document first).
    #[serde(default)]
    pub document_no: Option<String>,
    #[serde(default)]
    pub memo: Option<String>,
}

#[async_trait]
impl Operation for SubmitExpenseClaimOp {
    fn name(&self) -> &'static str {
        "submit_expense_claim"
    }
    fn risk(&self) -> Risk {
        // Submitting alone moves no money, but it creates work for a person,
        // so it is not read-only.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "送出費用報支單（申請人送件關卡）。可帶入建議的費用科目與收據文件編號。不產生任何分錄。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SubmitExpenseClaimInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SubmitExpenseClaimInput = parse(input)?;
        if i.amount <= Money::ZERO {
            return Err(OpError::BadInput("報支金額要大於零".into()));
        }
        let pay_method = checked_pay_method(i.pay_method.as_deref())?;

        // The suggested account must exist and be an expense account. A wrong
        // account kind here would only surface when the voucher is created,
        // which is far too late.
        if let Some(code) = &i.suggested_account_code {
            let accounts = ctx.store.accounts().await?;
            let ok = accounts
                .iter()
                .any(|a| &a.code == code && a.is_postable && a.kind == "expense");
            if !ok {
                return Err(OpError::BadInput(format!(
                    "「{code}」非可入帳的費用科目。費用報支的借方必須是費用科目。"
                )));
            }
        }

        // The receipt's document number must exist: attaching a document that
        // is not there is worse than attaching none.
        let document_id = match &i.document_no {
            None => None,
            Some(no) => Some(ctx.store.document_id(no).await?),
        };

        let claimant = i.claimant.clone().unwrap_or_else(|| ctx.actor.clone());
        let claim = ctx
            .store
            .submit_expense_claim(SubmitExpenseClaim {
                title: &i.title,
                claimant: &claimant,
                claimant_role: &ctx.actor_role,
                // This operation takes a claimant name, so the person filing
                // and the person named need not be the same. There is no key
                // to record, only the name that was given.
                employee_id: None,
                dept_code: i.dept_code.as_deref(),
                spent_on: i.spent_on.unwrap_or(ctx.today),
                amount: i.amount,
                suggested_account_code: i.suggested_account_code.as_deref(),
                pay_method,
                document_id,
                memo: i.memo.as_deref().unwrap_or(""),
            })
            .await?;

        let rules = ctx.store.approval_rules().await?;
        let steps = required_steps(&rules, claim.amount);
        let summary = format!(
            "{} · {} {} · 要經過 {} 關（依公司簽核規則的金額門檻）",
            claim.claim_no,
            claim.title,
            money(claim.amount),
            steps.len()
        );
        Ok(OpOutcome::new(
            json!({ "claim": claim_json(&claim, &rules) }),
            summary,
        ))
    }
}

// Approving

struct ApproveExpenseClaim;

#[derive(Deserialize, JsonSchema)]
pub struct ApproveExpenseClaimInput {
    /// Claim number, e.g. `EX2607-001`.
    pub claim_no: String,
    /// Required at the accounting step; later steps may omit it to keep the
    /// existing account.
    #[serde(default)]
    pub account_code: Option<String>,
    /// Approval comment, kept on the record.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for ApproveExpenseClaim {
    fn name(&self) -> &'static str {
        "approve_expense_claim"
    }
    fn risk(&self) -> Risk {
        // The final step creates a voucher: this is not a draft, it is the
        // company accepting the cost.
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "簽核費用報支單目前所在的關卡。會計關卡要帶入費用科目；最後一關會產生待過帳的費用傳票（借費用科目／貸應付帳款或零用金）。付款方式為月底彙總匯款時，同時列入應付帳款，由付款單匯出。申請人不得核准本人送出的單據。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ApproveExpenseClaimInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ApproveExpenseClaimInput = parse(input)?;
        let claim = ctx.store.expense_claim(&i.claim_no).await?;
        let rules = ctx.store.approval_rules().await?;
        let steps = required_steps(&rules, claim.amount);

        match claim.status.as_str() {
            "approved" => {
                return Err(OpError::BadInput(format!(
                    "{} 已入帳，如需更正請開立反向分錄",
                    claim.claim_no
                )))
            }
            "rejected" => {
                return Err(OpError::BadInput(format!(
                    "{} 已退回，要由申請人重新送出",
                    claim.claim_no
                )))
            }
            _ => {}
        }

        // None here is the deleted-step case described at the top of this
        // module: the claim is in flight with every applicable rule recorded.
        let step = next_step(&claim, &steps);
        may_decide(
            step,
            &claim,
            &ctx.actor,
            &ctx.actor_role,
            &crate::role_name(ctx).await,
        )?;

        // Confirming the account is what the accounting step is for, so that
        // step cannot be approved without one.
        let account_code = i
            .account_code
            .clone()
            .or_else(|| claim.account_code.clone());
        if let Some(step) = step.filter(|s| s.role_code.as_deref() == Some("acct")) {
            if account_code.is_none() {
                return Err(OpError::BadInput(format!(
                    "第 {} 關為「{}」，要先指定費用科目才能簽核（送單時建議 {}）",
                    step.step_no,
                    step.label,
                    claim
                        .suggested_account_code
                        .clone()
                        .unwrap_or_else(|| "（未填）".into())
                )));
            }
        }
        if let Some(code) = &i.account_code {
            let accounts = ctx.store.accounts().await?;
            if !accounts
                .iter()
                .any(|a| &a.code == code && a.is_postable && a.kind == "expense")
            {
                return Err(OpError::BadInput(format!(
                    "「{code}」非可入帳的費用科目。費用報支的借方必須是費用科目。"
                )));
            }
        }

        // Was that the last step? If so, create the voucher in the same
        // transaction. With no step left the voucher is all that remains, so
        // this call creates it.
        let last = match step {
            Some(s) => steps.last().map(|l| l.step_no) == Some(s.step_no),
            None => true,
        };
        if last && account_code.is_none() {
            return Err(OpError::BadInput(format!(
                "{} 尚未確定費用科目，無法開立傳票",
                claim.claim_no
            )));
        }

        // The approval record has to name a step. A pending step names
        // itself; otherwise the number is the next ordinal and the note says
        // what the record stands for, since no step of that number was ever
        // waiting on this claim.
        let (step_no, default_note) = match step {
            Some(s) => (s.step_no, ""),
            None => (finalise_step_no(&claim), "簽核關卡已變更，補開費用傳票"),
        };

        let updated = ctx
            .store
            .approve_expense_step(laysi_store::ApproveExpenseStep {
                claim_no: &i.claim_no,
                step_no,
                actor: &ctx.actor,
                actor_role: &ctx.actor_role,
                note: i.note.as_deref().unwrap_or(default_note),
                account_code: i.account_code.as_deref(),
                finalise: last,
                posted_on: None,
            })
            .await?;

        let step_note = match step {
            Some(s) => format!("通過第 {} 關「{}」", s.step_no, s.label),
            None => "應經的關卡都已簽核".to_string(),
        };
        let summary = match &updated.voucher_no {
            Some(no) => format!(
                "{} {} {} {} · 產生 {} 待過帳{}",
                updated.claim_no,
                updated.title,
                money(updated.amount),
                step_note,
                no,
                if updated.pay_method == "petty_cash" {
                    ""
                } else {
                    "，並列入應付帳款"
                }
            ),
            None => format!(
                "{} {} {} {} · 下一關 {}",
                updated.claim_no,
                updated.title,
                money(updated.amount),
                step_note,
                next_step(&updated, &steps)
                    .map(|s| s.label.clone())
                    .unwrap_or_default()
            ),
        };

        let out = OpOutcome::new(
            json!({
                "claim": claim_json(&updated, &rules),
                "voucher": match &updated.voucher_no {
                    Some(no) => Some(ctx.store.voucher_by_no(no).await?),
                    None => None,
                },
                "step_taken": step_no,
                "finished": updated.status == "approved",
            }),
            summary,
        );
        Ok(match &updated.voucher_no {
            Some(no) => out.with_voucher(no.clone()),
            None => out,
        })
    }
}

// Rejecting

struct RejectExpenseClaim;

#[derive(Deserialize, JsonSchema)]
pub struct RejectExpenseClaimInput {
    pub claim_no: String,
    /// Why it was rejected. The claimant needs to know what to fix.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for RejectExpenseClaim {
    fn name(&self) -> &'static str {
        "reject_expense_claim"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "退回一張報支單，由申請人補件；整張作廢重送。單據內容變更後，先前關卡的簽核就不再有效。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RejectExpenseClaimInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RejectExpenseClaimInput = parse(input)?;
        let claim = ctx.store.expense_claim(&i.claim_no).await?;
        let rules = ctx.store.approval_rules().await?;
        let steps = required_steps(&rules, claim.amount);
        let step = next_step(&claim, &steps).ok_or_else(|| {
            OpError::BadInput(format!(
                "{} 已完成全部簽核，如需更正請開立反向分錄",
                claim.claim_no
            ))
        })?;
        may_decide(
            Some(step),
            &claim,
            &ctx.actor,
            &ctx.actor_role,
            &crate::role_name(ctx).await,
        )?;

        let note = i.note.as_deref().unwrap_or("請補資料");
        let updated = ctx
            .store
            .reject_expense_claim(&i.claim_no, step.step_no, &ctx.actor, &ctx.actor_role, note)
            .await?;

        let summary = format!(
            "{} {} {} 已退回給 {}：{}",
            updated.claim_no,
            updated.title,
            money(updated.amount),
            updated.claimant,
            note
        );
        Ok(OpOutcome::new(
            json!({ "claim": claim_json(&updated, &rules) }),
            summary,
        ))
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

    /// The four steps created by 0006. A test fixture, not a default in the
    /// code: the real path always reads `approval_rule`.
    fn reference_rules() -> Vec<ApprovalRule> {
        vec![
            rule(1, None, "申請人上傳", None),
            rule(2, Some("acct"), "會計確認科目", None),
            rule(3, Some("manager"), "主管核准", Some("10000")),
            rule(4, Some("boss"), "老闆簽核", Some("50000")),
        ]
    }

    fn rule(step_no: i32, role: Option<&str>, label: &str, min: Option<&str>) -> ApprovalRule {
        ApprovalRule {
            step_no,
            role_code: role.map(str::to_string),
            role_name: role.map(|r| r.to_string()),
            label: label.into(),
            condition_note: String::new(),
            min_amount: min.map(n),
        }
    }

    fn claim(amount: &str, done: &[i32]) -> ExpenseClaim {
        ExpenseClaim {
            id: 1,
            claim_no: "EX2607-001".into(),
            title: "客戶拜訪高鐵票".into(),
            claimant: "申請人甲".into(),
            claimant_role: "manager".into(),
            dept_code: Some("sales".into()),
            dept_label: Some("業務部".into()),
            spent_on: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
            amount: n(amount),
            suggested_account_code: Some("6122".into()),
            suggested_account_name: Some("旅費".into()),
            account_code: None,
            account_name: None,
            pay_method: "reimburse".into(),
            status: "submitted".into(),
            voucher_no: None,
            voucher_status: None,
            document_no: None,
            document_name: None,
            memo: String::new(),
            created_at: Utc::now(),
            created_by: "申請人甲".into(),
            steps: done
                .iter()
                .map(|s| laysi_store::ExpenseStep {
                    step_no: *s,
                    decision: "approved".into(),
                    actor: "某人".into(),
                    actor_role: "acct".into(),
                    note: String::new(),
                    decided_at: Utc::now(),
                })
                .collect(),
        }
    }

    /// The central property: thresholds are data, not code.
    ///
    /// The same code with a different approval_rule gives a different flow,
    /// without a single change to this file.
    #[test]
    fn the_thresholds_come_from_the_table_not_from_this_file() {
        let rules = reference_rules();
        let steps = |amount: &str| -> Vec<i32> {
            required_steps(&rules, n(amount))
                .iter()
                .map(|s| s.step_no)
                .collect()
        };
        assert_eq!(steps("2860"), vec![1, 2], "小額只要上傳與會計確認");
        assert_eq!(
            steps("10000"),
            vec![1, 2, 3],
            "剛好一萬就要主管，門檻是「以上」"
        );
        assert_eq!(steps("18600"), vec![1, 2, 3]);
        assert_eq!(steps("50000"), vec![1, 2, 3, 4], "五萬就要老闆");
        assert_eq!(steps("45000"), vec![1, 2, 3]);

        // A different company: manager above 20,000, owner above 100,000. The
        // code did not change.
        let other = vec![
            rule(1, None, "申請人上傳", None),
            rule(2, Some("acct"), "會計確認科目", None),
            rule(3, Some("manager"), "主管核准", Some("20000")),
            rule(4, Some("boss"), "老闆簽核", Some("100000")),
        ];
        let n_steps = |amount: &str| required_steps(&other, n(amount)).len();
        assert_eq!(n_steps("18600"), 2, "同一張單，換一家公司就不用主管簽");
        assert_eq!(n_steps("45000"), 3);
    }

    /// Steps run in order, and only the final one creates the voucher.
    #[test]
    fn the_flow_advances_one_step_at_a_time() {
        let rules = reference_rules();

        let c = claim("45000", &[1]);
        let steps = required_steps(&rules, c.amount);
        assert_eq!(next_step(&c, &steps).unwrap().step_no, 2, "上傳完換會計");

        let c = claim("45000", &[1, 2]);
        assert_eq!(
            next_step(&c, &steps).unwrap().step_no,
            3,
            "會計確認完換主管"
        );

        let c = claim("45000", &[1, 2, 3]);
        assert!(next_step(&c, &steps).is_none(), "四萬五不需要老闆那一關");

        // The same approval record still needs one more step at 50,000.
        let big = claim("50000", &[1, 2, 3]);
        let big_steps = required_steps(&rules, big.amount);
        assert_eq!(next_step(&big, &big_steps).unwrap().step_no, 4);
    }

    /// A claimant cannot approve their own claim.
    #[test]
    fn nobody_approves_their_own_claim() {
        let rules = reference_rules();
        let c = claim("2860", &[1]);
        let steps = required_steps(&rules, c.amount);
        let step = next_step(&c, &steps).unwrap();

        let err = may_decide(Some(step), &c, "申請人甲", "boss", "負責人").unwrap_err();
        assert!(
            err.to_string().contains("要由另一個帳號簽核"),
            "連負責人都不能簽自己的單"
        );
        assert!(
            err.to_string().contains("帳號管理"),
            "只有一個帳號的公司要看得到出路在哪：{err}"
        );

        assert!(
            may_decide(Some(step), &c, "會計人員", "acct", "會計").is_ok(),
            "會計簽會計那一關"
        );
        assert!(
            may_decide(Some(step), &c, "負責人", "boss", "負責人").is_ok(),
            "負責人可以代任何一關"
        );

        let err = may_decide(Some(step), &c, "出納人員", "cashier", "出納").unwrap_err();
        assert!(
            err.to_string().contains("會計確認科目"),
            "擋下來時要說清楚該誰簽：{err}"
        );
        assert!(
            !err.to_string().contains("cashier"),
            "錯誤訊息不出現角色代號：{err}"
        );
    }

    /// Deleting the step a claim is waiting on must not strand it.
    ///
    /// A company with no manager deletes step 3 while a 12,000 claim sits
    /// there. Every rule the claim must pass is then recorded, and the
    /// voucher is the only thing left.
    #[test]
    fn deleting_the_pending_step_leaves_the_voucher_reachable() {
        let rules = vec![
            rule(1, None, "申請人上傳", None),
            rule(2, Some("acct"), "會計確認科目", None),
            rule(4, Some("boss"), "負責人簽核", Some("50000")),
        ];
        let c = claim("12000", &[1, 2]);
        let steps = required_steps(&rules, c.amount);
        let next = next_step(&c, &steps);
        assert!(next.is_none(), "沒有關卡在等，剩下的是開傳票");

        assert!(
            may_decide(next, &c, "會計人員", "acct", "會計").is_ok(),
            "沒有關卡可比對角色時不擋人，權限由角色對照表決定"
        );
        let err = may_decide(next, &c, "申請人甲", "boss", "負責人").unwrap_err();
        assert!(
            err.to_string().contains("要由另一個帳號簽核"),
            "申請人不得核准本人送出的單據，沒有關卡也一樣：{err}"
        );

        assert_eq!(finalise_step_no(&c), 3, "補開傳票的紀錄接在已簽的關卡之後");
    }

    /// The whole rule set can also stop applying to a claim.
    #[test]
    fn a_claim_below_every_threshold_still_reaches_the_voucher() {
        let rules = vec![rule(4, Some("boss"), "負責人簽核", Some("50000"))];
        let c = claim("12000", &[1]);
        let steps = required_steps(&rules, c.amount);
        assert!(steps.is_empty(), "一萬二不到門檻，一關都不用經過");
        assert!(next_step(&c, &steps).is_none());
        assert_eq!(finalise_step_no(&c), 2, "只簽過第 1 關");
    }

    /// Every rule the claim has signed can be gone by the time it finalises.
    /// 0053 dropped the foreign key so the ordinal no longer has to match one.
    #[test]
    fn a_claim_that_used_every_number_still_records_its_finalisation() {
        let c = claim("12000", &[1, 2]);
        assert_eq!(finalise_step_no(&c), 3);
        assert_eq!(
            finalise_step_no(&claim("12000", &[])),
            1,
            "一關都沒簽過就從 1 開始"
        );
    }

    /// The wording shown on screen is derived, not stored.
    #[test]
    fn the_waiting_label_matches_the_prototype_wording() {
        let rules = reference_rules();
        let label = |amount: &str, done: &[i32]| {
            let c = claim(amount, done);
            let steps = required_steps(&rules, c.amount);
            step_label(&c, next_step(&c, &steps))
        };
        assert_eq!(label("2860", &[1]), "待確認會計科目");
        assert_eq!(label("18600", &[1, 2]), "待主管核准");
        assert_eq!(label("45000", &[1, 2, 3]), "待產生傳票");
        assert_eq!(label("50000", &[1, 2, 3]), "待負責人核准");

        let mut done = claim("1200", &[1, 2]);
        done.status = "approved".into();
        let steps = required_steps(&rules, done.amount);
        assert_eq!(step_label(&done, next_step(&done, &steps)), "已入帳");
    }
}
