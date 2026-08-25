//! CSV imports.
//!
//! [`PreviewImport`] parses, validates and reports problems per row without
//! writing anything, storing the parsed drafts in `import_batch.payload`.
//! [`CommitImport`] writes exactly that payload and never re-parses the file,
//! so confirming an import commits the rows the user actually reviewed.
//!
//! Imports produce unposted vouchers. [`RollbackImport`] can only recover
//! those; posted vouchers can be corrected only by a reversing entry, which
//! the 0001 trigger enforces.
//!
//! ROC-era dates (`114/06/30`) are converted to the Gregorian calendar, and
//! every converted row carries a warning — silent conversion is not
//! acceptable.
//!
//! `import_source.supported` is what routes a file to a parser, so a source
//! row only exists once its parser does. 0061 removed the five rows that
//! described parsers nobody had written.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_store::{CreateImportBatch, ImportIssue, ImportLineDraft, ImportVoucherDraft};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::str::FromStr;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListImports),
        Box::new(PreviewImport),
        Box::new(CommitImport),
        Box::new(RollbackImport),
    ]
}

/// Offset between the ROC and Gregorian calendars. A calendar constant
/// rather than a regulation value; it has not changed since ROC year 1.
const ROC_EPOCH_OFFSET: i32 = 1911;

/// A year of three digits or fewer is treated as a ROC year. No accounting
/// system holds vouchers from AD 200, and ROC 200 (AD 2111) has not arrived,
/// so the range cannot be misread.
const ROC_YEAR_CEILING: i32 = 200;

/// Column order of the CSV. A first row that is not data is skipped as a
/// header.
const COLUMNS: [&str; 7] = ["傳票號", "日期", "摘要", "科目代號", "借方", "貸方", "部門代碼"];

// Parsing

/// Lookup tables for accounts and departments. Parsing is a pure function
/// that never touches the database, so everything it needs is passed in and
/// tests can feed it fixtures.
pub struct Lookups {
    /// Code to name, postable accounts only.
    pub accounts: BTreeMap<String, String>,
    /// Department code to dimension_value.id.
    pub departments: BTreeMap<String, i32>,
}

/// Rows accumulated under one voucher number.
///
/// A named type rather than a tuple: the date and memo take the first
/// non-empty value while lines accumulate, and the three roles differ enough
/// that names read better than positions.
#[derive(Default)]
struct Group {
    /// `(CSV row number, line)`. The row number is kept so errors can point
    /// at it.
    lines: Vec<(i32, ImportLineDraft)>,
    date: Option<NaiveDate>,
    memo: String,
}

/// Result of one dry run.
#[derive(Debug, Default)]
pub struct Parsed {
    pub drafts: Vec<ImportVoucherDraft>,
    pub issues: Vec<ImportIssue>,
    /// Data rows, excluding the header.
    pub row_count: i32,
}

impl Parsed {
    fn error(&mut self, row_no: i32, issue: impl Into<String>, fix: impl Into<String>) {
        self.issues.push(ImportIssue {
            row_no,
            severity: "error".into(),
            issue: issue.into(),
            fix: fix.into(),
        });
    }
    fn warn(&mut self, row_no: i32, issue: impl Into<String>, fix: impl Into<String>) {
        self.issues.push(ImportIssue {
            row_no,
            severity: "warning".into(),
            issue: issue.into(),
            fix: fix.into(),
        });
    }
    pub fn errors(&self) -> usize {
        self.issues.iter().filter(|i| i.severity == "error").count()
    }
    pub fn warnings(&self) -> usize {
        self.issues.iter().filter(|i| i.severity == "warning").count()
    }
}

/// Result of parsing a date. `converted` means it was a ROC year.
struct ParsedDate {
    date: NaiveDate,
    converted: bool,
}

/// Parse a date, accepting `2025-06-30`, `2025/06/30` and ROC `114/06/30`.
///
/// A ROC year always reports `converted = true`. The conversion is correct,
/// but the user has to know the system changed their data; a silent
/// conversion is as dangerous as a guess.
fn parse_date(raw: &str) -> Option<ParsedDate> {
    let t = raw.trim();
    let parts: Vec<&str> = t.split(['-', '/', '.']).collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i32 = parts[0].trim().parse().ok()?;
    let m: u32 = parts[1].trim().parse().ok()?;
    let d: u32 = parts[2].trim().parse().ok()?;

    let converted = y <= ROC_YEAR_CEILING;
    let year = if converted { y + ROC_EPOCH_OFFSET } else { y };
    NaiveDate::from_ymd_opt(year, m, d).map(|date| ParsedDate { date, converted })
}

/// Parse an amount. Blank and `-` are zero, thousands separators are removed.
fn parse_amount(raw: &str) -> Option<Money> {
    let t = raw.trim().replace(',', "");
    if t.is_empty() || t == "-" {
        return Some(Money::ZERO);
    }
    Decimal::from_str(&t).ok()
}

/// Split one CSV row, honouring double-quoted fields (memos routinely
/// contain commas).
fn split_row(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Does this row look like a header? It does when neither the debit nor the
/// credit column parses as a number.
fn looks_like_header(cells: &[String]) -> bool {
    cells.len() >= 6 && parse_amount(&cells[4]).is_none() && parse_amount(&cells[5]).is_none()
}

/// Parse the historical voucher CSV.
///
/// Column order is in [`COLUMNS`]. Rows sharing a voucher number merge into
/// one voucher, and each voucher is checked for balance and line count
/// independently: an unbalanced one is rejected while the rest are still
/// listed, because the user needs to know which vouchers are wrong, not
/// that the file is broken.
pub fn parse_vouchers(csv: &str, look: &Lookups) -> Parsed {
    let mut out = Parsed::default();
    // A BTreeMap keeps voucher numbers in a stable order, so importing the
    // same file twice produces drafts in the same order.
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();

    for (idx, line) in csv.lines().enumerate() {
        let row_no = idx as i32 + 1;
        if line.trim().is_empty() {
            continue;
        }
        let cells = split_row(line);
        if idx == 0 && looks_like_header(&cells) {
            continue;
        }
        out.row_count += 1;

        if cells.len() < 6 {
            out.error(
                row_no,
                format!("只有 {} 欄，至少要 6 欄", cells.len()),
                format!("欄位順序：{}", COLUMNS.join("、")),
            );
            continue;
        }

        let voucher_ref = cells[0].trim().to_string();
        if voucher_ref.is_empty() {
            out.error(row_no, "缺少傳票號", "同一張傳票的每一行都要填相同的傳票號");
            continue;
        }

        let Some(d) = parse_date(&cells[1]) else {
            out.error(
                row_no,
                format!("日期「{}」看不懂", cells[1].trim()),
                "可以是 2025-06-30、2025/06/30 或民國年 114/06/30",
            );
            continue;
        };
        if d.converted {
            out.warn(
                row_no,
                format!("日期「{}」是民國年，已轉成 {}", cells[1].trim(), d.date),
                "請確認轉換後的年份",
            );
        }

        let memo = cells[2].trim().to_string();
        let code = cells[3].trim().to_string();
        if !look.accounts.contains_key(&code) {
            out.error(
                row_no,
                format!("科目「{code}」不存在，或不是可入帳科目"),
                "到「基礎設定」的科目表查代號；父階層科目不能直接開分錄",
            );
            continue;
        }

        let (Some(debit), Some(credit)) = (parse_amount(&cells[4]), parse_amount(&cells[5])) else {
            out.error(
                row_no,
                format!("借方「{}」或貸方「{}」不是數字", cells[4].trim(), cells[5].trim()),
                "金額只能有數字、小數點與千分位逗號",
            );
            continue;
        };
        if debit < Money::ZERO || credit < Money::ZERO {
            out.error(row_no, "金額是負數", "沖銷請將金額填入相反的欄位，不要使用負號");
            continue;
        }
        if (debit > Money::ZERO) == (credit > Money::ZERO) {
            let what = if debit.is_zero() { "借貸都是零" } else { "同一行同時有借方與貸方" };
            out.error(row_no, what, "一行只能記一邊");
            continue;
        }

        // A mistyped department is not treated as blank. The user meant to
        // assign one, and dropping it silently would leave a hole in
        // departmental P&L that only surfaces at month-end.
        let dept_raw = cells.get(6).map(|s| s.trim()).unwrap_or("");
        let dept_id = if dept_raw.is_empty() {
            None
        } else {
            match look.departments.get(dept_raw) {
                Some(id) => Some(*id),
                None => {
                    out.error(
                        row_no,
                        format!("查無部門「{dept_raw}」"),
                        format!(
                            "可用的部門代碼：{}",
                            look.departments.keys().cloned().collect::<Vec<_>>().join("、")
                        ),
                    );
                    continue;
                }
            }
        };

        let entry = groups.entry(voucher_ref).or_default();
        entry.date.get_or_insert(d.date);
        if entry.memo.is_empty() && !memo.is_empty() {
            entry.memo.clone_from(&memo);
        }
        entry.lines.push((
            row_no,
            ImportLineDraft {
                account_code: code,
                debit,
                credit,
                memo: (!memo.is_empty()).then(|| memo.clone()),
                dept_id,
            },
        ));
    }

    for (voucher_ref, g) in groups {
        let Group { lines, date, memo } = g;
        let first_row = lines.first().map(|(r, _)| *r).unwrap_or(0);
        let Some(date) = date else { continue };

        if lines.len() < 2 {
            out.error(
                first_row,
                format!("傳票 {voucher_ref} 只有 1 行分錄"),
                "複式記帳至少需兩行分錄，請確認是否漏填對方科目",
            );
            continue;
        }
        let dr: Money = lines.iter().map(|(_, l)| l.debit).sum();
        let cr: Money = lines.iter().map(|(_, l)| l.credit).sum();
        if dr != cr {
            out.error(
                first_row,
                format!(
                    "傳票 {voucher_ref} 借貸不相等：借方 {}，貸方 {}，差 {}",
                    money(dr),
                    money(cr),
                    money((dr - cr).abs())
                ),
                "每一行都會檢查；借貸不平的傳票資料庫也會擋下",
            );
            continue;
        }

        out.drafts.push(ImportVoucherDraft {
            date,
            memo: if memo.is_empty() { format!("匯入 {voucher_ref}") } else { memo },
            lines: lines.into_iter().map(|(_, l)| l).collect(),
        });
    }

    // Drafts are sorted by date so the imported voucher numbers follow the
    // dates rather than the order in the file.
    out.drafts.sort_by_key(|d| d.date);
    out
}

// Listing

struct ListImports;

#[async_trait]
impl Operation for ListImports {
    fn name(&self) -> &'static str {
        "list_imports"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀匯入來源與批次紀錄。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let sources = ctx.store.import_sources().await?;
        let batches = ctx.store.import_batches(20).await?;

        let supported: Vec<&str> =
            sources.iter().filter(|s| s.supported).map(|s| s.code.as_str()).collect();
        let summary = format!(
            "讀取匯入 · {} 種來源（{} 種可匯入）· {} 批紀錄",
            sources.len(),
            supported.len(),
            batches.len()
        );
        Ok(OpOutcome::new(
            json!({
                "sources": sources,
                "batches": batches,
                "supported_sources": supported,
                // Both of these must be shown on screen.
                "csv_columns": COLUMNS,
                "format_note":
                    "歷史傳票 CSV 的欄位順序：傳票號、日期、摘要、科目代號、借方、貸方、部門代碼。\
                     同一個傳票號的列會併成一張傳票；民國年日期會自動轉換並留下提醒。",
                "commit_note":
                    "匯入產生的是待過帳傳票，不會自動過帳。復原只適用於尚未過帳的批次；\
                     已過帳的更正只能開反向分錄。",
            }),
            summary,
        ))
    }
}

// Dry run

#[derive(Deserialize, JsonSchema)]
pub struct PreviewImportInput {
    /// Source code. Only `voucher` (historical voucher CSV) can be parsed.
    pub source_code: String,
    /// File name, used for display and audit only.
    pub file_name: String,
    /// File contents (plain CSV text).
    pub content: String,
}

struct PreviewImport;

#[async_trait]
impl Operation for PreviewImport {
    fn name(&self) -> &'static str {
        "preview_import"
    }
    fn risk(&self) -> Risk {
        // Writes no business data, but does leave a preview record, so it is
        // not read-only.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "匯入預覽：解析 CSV、逐列驗證科目與借貸並列出問題，不寫入任何傳票。解析結果會保留，確認匯入時採用同一份。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<PreviewImportInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: PreviewImportInput = parse(input)?;

        let sources = ctx.store.import_sources().await?;
        let source = sources
            .iter()
            .find(|s| s.code == i.source_code)
            .ok_or_else(|| OpError::BadInput(format!("查無匯入來源「{}」", i.source_code)))?;
        if !source.supported {
            return Err(OpError::BadInput(format!(
                "「{}」尚未支援匯入（{}）。目前只支援歷史傳票 CSV。",
                source.label, source.note
            )));
        }
        if i.content.trim().is_empty() {
            return Err(OpError::BadInput("檔案是空的".into()));
        }

        let accounts = ctx.store.accounts().await?;
        let departments = ctx.store.departments().await?;
        let look = Lookups {
            accounts: accounts
                .iter()
                .filter(|a| a.is_postable)
                .map(|a| (a.code.clone(), a.name.clone()))
                .collect(),
            departments: departments.iter().map(|d| (d.code.clone(), d.id)).collect(),
        };

        let parsed = parse_vouchers(&i.content, &look);
        let total: Money = parsed
            .drafts
            .iter()
            .flat_map(|d| d.lines.iter())
            .map(|l| l.debit)
            .sum();

        let batch = ctx
            .store
            .create_import_batch(CreateImportBatch {
                source_code: &i.source_code,
                file_name: &i.file_name,
                row_count: parsed.row_count,
                ok_count: parsed.drafts.len() as i32,
                error_count: parsed.errors() as i32,
                warn_count: parsed.warnings() as i32,
                payload: serde_json::to_value(&parsed.drafts).unwrap_or_else(|_| json!([])),
                issues: &parsed.issues,
                today: ctx.today,
                actor: &ctx.actor,
            })
            .await?;

        let summary = format!(
            "匯入預覽 {} · {} 列 → {} 張傳票 {} · 錯誤 {} 警告 {} · 尚未寫入",
            batch.batch_no,
            parsed.row_count,
            parsed.drafts.len(),
            money(total),
            parsed.errors(),
            parsed.warnings()
        );
        Ok(OpOutcome::new(
            json!({
                "batch": batch,
                "drafts": parsed.drafts,
                "total": total.to_string(),
                "can_commit": parsed.errors() == 0 && !parsed.drafts.is_empty(),
                "blocked_note": (parsed.errors() > 0).then_some(
                    "含錯誤的批次無法確認匯入，請修正檔案後重新預覽。"
                ),
            }),
            summary,
        ))
    }
}

// Committing an import

#[derive(Deserialize, JsonSchema)]
pub struct BatchInput {
    /// Batch number, e.g. `IM2607-001`.
    pub batch_no: String,
}

struct CommitImport;

#[async_trait]
impl Operation for CommitImport {
    fn name(&self) -> &'static str {
        "commit_import"
    }
    fn risk(&self) -> Risk {
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "將預覽結果寫入為傳票（狀態待過帳）。寫入的是預覽當時的解析結果，不會重新解析檔案；含錯誤的批次無法寫入。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<BatchInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: BatchInput = parse(input)?;

        let existing = ctx.store.import_batch(&i.batch_no).await?;
        if existing.error_count > 0 {
            return Err(OpError::BadInput(format!(
                "{} 有 {} 個錯誤，無法匯入。請修正檔案後重新預覽。",
                i.batch_no, existing.error_count
            )));
        }

        // Writes the payload stored by the dry run rather than re-parsing:
        // confirming commits the rows the user actually reviewed.
        let (_, payload) = ctx.store.import_payload(&i.batch_no).await?;
        let drafts: Vec<ImportVoucherDraft> = serde_json::from_value(payload)
            .map_err(|e| OpError::BadInput(format!("無法讀回預覽結果：{e}")))?;
        if drafts.is_empty() {
            return Err(OpError::BadInput(format!("{} 沒有可匯入的傳票", i.batch_no)));
        }

        let batch = ctx.store.commit_import_batch(&i.batch_no, &drafts, &ctx.actor).await?;
        let total: Money =
            drafts.iter().flat_map(|d| d.lines.iter()).map(|l| l.debit).sum();

        let summary = format!(
            "匯入 {} · {} 張傳票 {} 已寫入（待過帳）· {}",
            batch.batch_no,
            batch.voucher_nos.len(),
            money(total),
            batch.voucher_nos.first().cloned().unwrap_or_default()
        );
        Ok(OpOutcome::new(
            json!({
                "batch": batch,
                "total": total.to_string(),
                "posted": false,
                "note": "要出具報表請先到「傳票 · 總帳」過帳。",
            }),
            summary,
        ))
    }
}

struct RollbackImport;

#[async_trait]
impl Operation for RollbackImport {
    fn name(&self) -> &'static str {
        "rollback_import"
    }
    fn risk(&self) -> Risk {
        // Same reasoning as revoke_api_token: an action in the safe direction
        // need not be owner-only.
        Risk::HighRisk
    }
    fn summary(&self) -> &'static str {
        "復原一批匯入，刪除其產生的傳票。整批都沒過帳才能復原；已過帳的要用反向分錄更正。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<BatchInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: BatchInput = parse(input)?;
        let before = ctx.store.import_batch(&i.batch_no).await?;
        let batch = ctx.store.rollback_import_batch(&i.batch_no, &ctx.actor).await?;

        let summary = format!(
            "復原 {} · 刪除 {} 張待過帳傳票",
            batch.batch_no,
            before.voucher_nos.len()
        );
        Ok(OpOutcome::new(
            json!({ "batch": batch, "removed": before.voucher_nos }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Money {
        Decimal::from_str(s).unwrap()
    }

    fn look() -> Lookups {
        Lookups {
            accounts: [("6115", "租金支出"), ("1102", "銀行存款"), ("6111", "薪資支出")]
                .into_iter()
                .map(|(c, n)| (c.to_string(), n.to_string()))
                .collect(),
            departments: [("admin", 1), ("sales", 2)]
                .into_iter()
                .map(|(c, i)| (c.to_string(), i))
                .collect(),
        }
    }

    fn issues_of(p: &Parsed, severity: &str) -> Vec<String> {
        p.issues
            .iter()
            .filter(|i| i.severity == severity)
            .map(|i| i.issue.clone())
            .collect()
    }

    /// ROC years must be converted, and the conversion must be reported.
    ///
    /// This is the easiest thing in the module to get wrong and the hardest
    /// to notice: parsing `114/06/30` as a Gregorian year gives AD 114, which
    /// looks exactly like a valid date on screen.
    #[test]
    fn republic_era_dates_are_converted_and_always_reported() {
        let a = parse_date("114/06/30").unwrap();
        assert_eq!(a.date, NaiveDate::from_ymd_opt(2025, 6, 30).unwrap());
        assert!(a.converted, "轉換過就要說一聲");

        let b = parse_date("2025-06-30").unwrap();
        assert_eq!(b.date, NaiveDate::from_ymd_opt(2025, 6, 30).unwrap());
        assert!(!b.converted, "西元年沒有轉換，不該留提醒");

        assert_eq!(parse_date("2025/06/30").unwrap().date, b.date, "斜線與破折號都要收");
        assert!(parse_date("六月三十").is_none());
        assert!(parse_date("2025-13-01").is_none(), "第 13 個月不是日期");
    }

    /// The conversion warning really reaches the dry run result, not just the
    /// function's return value.
    #[test]
    fn the_conversion_warning_reaches_the_preview() {
        let csv = "IMP-1,114/06/30,6 月房租,6115,52000,0,admin\n\
                   IMP-1,114/06/30,6 月房租,1102,0,52000,";
        let p = parse_vouchers(csv, &look());
        assert_eq!(p.errors(), 0);
        assert_eq!(p.warnings(), 2, "兩列都轉換過，兩列都要留提醒");
        assert!(issues_of(&p, "warning")[0].contains("民國年"));
        assert_eq!(p.drafts.len(), 1);
        assert_eq!(p.drafts[0].date, NaiveDate::from_ymd_opt(2025, 6, 30).unwrap());
    }

    /// The unbalanced voucher is rejected while the rest still parse.
    ///
    /// "The file is broken" is useless to the user; which voucher is wrong is
    /// what they need.
    #[test]
    fn an_unbalanced_voucher_is_blocked_without_taking_the_rest_down() {
        let csv = "IMP-1,2025-06-30,房租,6115,52000,0,\n\
                   IMP-1,2025-06-30,房租,1102,0,52000,\n\
                   IMP-2,2025-07-01,薪資,6111,300000,0,\n\
                   IMP-2,2025-07-01,薪資,1102,0,299000,";
        let p = parse_vouchers(csv, &look());
        assert_eq!(p.drafts.len(), 1, "只有平的那一張變成草稿");
        assert_eq!(p.drafts[0].memo, "房租");
        assert_eq!(p.errors(), 1);
        let e = &issues_of(&p, "error")[0];
        assert!(e.contains("IMP-2") && e.contains("借貸不相等") && e.contains("1,000"));
    }

    /// One side per line, no negative amounts, at least two lines per voucher
    /// — the same invariants the database enforces.
    #[test]
    fn the_row_level_rules_match_the_database_invariants() {
        let both = parse_vouchers("A,2025-01-01,x,6115,100,100,", &look());
        assert!(issues_of(&both, "error")[0].contains("同時有借方與貸方"));

        let neither = parse_vouchers("A,2025-01-01,x,6115,0,0,", &look());
        assert!(issues_of(&neither, "error")[0].contains("借貸都是零"));

        let negative = parse_vouchers("A,2025-01-01,x,6115,-100,0,", &look());
        assert!(issues_of(&negative, "error")[0].contains("負數"));

        let lonely = parse_vouchers("A,2025-01-01,x,6115,100,0,", &look());
        assert!(issues_of(&lonely, "error")[0].contains("只有 1 行"));
        assert!(lonely.drafts.is_empty());
    }

    /// An unknown account and an unknown department are both errors, not
    /// something to fix later.
    ///
    /// A mistyped department is worth rejecting in particular: dropping it
    /// silently leaves a hole in departmental P&L that only surfaces at
    /// month-end reconciliation.
    #[test]
    fn unknown_accounts_and_departments_are_refused_never_guessed() {
        let bad_account = parse_vouchers("A,2025-01-01,x,9999,100,0,", &look());
        assert!(issues_of(&bad_account, "error")[0].contains("9999"));

        let bad_dept = parse_vouchers("A,2025-01-01,x,6115,100,0,業務部", &look());
        let e = &issues_of(&bad_dept, "error")[0];
        assert!(e.contains("業務部"), "要指出是哪一個部門打錯");
        assert!(bad_dept.issues[0].fix.contains("admin"), "要告訴他可用的代碼");
    }

    /// A department that resolves is attached; departmental P&L reads from it.
    #[test]
    fn a_known_department_is_carried_onto_the_line() {
        let csv = "A,2025-01-01,拜訪客戶,6115,100,0,sales\n\
                   A,2025-01-01,拜訪客戶,1102,0,100,";
        let p = parse_vouchers(csv, &look());
        assert_eq!(p.errors(), 0);
        assert_eq!(p.drafts[0].lines[0].dept_id, Some(2));
        assert_eq!(p.drafts[0].lines[1].dept_id, None, "沒填就是沒填，不要複製上一行的");
    }

    /// Headers are skipped, commas inside memos are kept, and thousands
    /// separators do not turn an amount into an error.
    #[test]
    fn the_parser_survives_what_real_exports_look_like() {
        let csv = "傳票號,日期,摘要,科目代號,借方,貸方,部門代碼\n\
                   A,2025-01-01,\"房租, 含管理費\",6115,\"52,000\",0,\n\
                   A,2025-01-01,\"房租, 含管理費\",1102,0,\"52,000\",";
        let p = parse_vouchers(csv, &look());
        assert_eq!(p.row_count, 2, "表頭不算資料列");
        assert_eq!(p.errors(), 0);
        assert_eq!(p.drafts[0].memo, "房租, 含管理費");
        assert_eq!(p.drafts[0].lines[0].debit, n("52000"));
    }

    /// Drafts sort by date, so imported voucher numbers follow the dates
    /// rather than the order of the file.
    #[test]
    fn drafts_come_out_in_date_order() {
        let csv = "B,2025-08-01,晚的,6115,10,0,\nB,2025-08-01,晚的,1102,0,10,\n\
                   A,2025-02-01,早的,6115,20,0,\nA,2025-02-01,早的,1102,0,20,";
        let p = parse_vouchers(csv, &look());
        assert_eq!(p.drafts.len(), 2);
        assert_eq!(p.drafts[0].memo, "早的");
        assert_eq!(p.drafts[1].memo, "晚的");
    }

    /// An empty file is not an error, just zero rows; the caller phrases the
    /// message.
    #[test]
    fn an_empty_file_parses_into_nothing() {
        let p = parse_vouchers("\n\n  \n", &look());
        assert_eq!(p.row_count, 0);
        assert!(p.drafts.is_empty() && p.issues.is_empty());
    }
}
