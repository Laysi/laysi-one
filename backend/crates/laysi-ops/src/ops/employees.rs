//! Employee roster and HR dashboard.
//!
//! [`ListEmployees`] is `ReadOnly`. On screen it returns the terms of
//! employment -- base salary, the three insured grades, the bank accounts and
//! the full id number -- because the drawer that edits them has to show what
//! it is about to replace, and no other page says what someone is paid today.
//! Monthly payslips stay in `list_payslips` (boss only).
//!
//! Over MCP the same operation answers with less: the amounts and the bank
//! accounts are dropped and the id number is masked. The tool grant already
//! promises as much, and a model has no filing errand that needs either.
//!
//! Annual leave entitlement is `annual_leave_days(ruleset, service_months)`,
//! never stored; days taken are summed from `leave_request`.
//!
//! Each of the four HR-to-finance links on the dashboard carries an
//! `implemented` flag: the ones that are wired report their evidence (voucher
//! number, reconciliation result), the ones that are not state what is
//! missing.

use crate::ops::documents::{
    document_json, human_size, kind_label, short_checksum, EMPLOYEE_KINDS, EMPLOYEE_LINK_TYPE,
};
use crate::{money, parse, schema_of, Channel, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use base64::Engine;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_store::storage::{self, MAX_OBJECT_BYTES};
use laysi_store::ReserveDocument;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// An id number with everything but its first three and last two characters
/// replaced, the same shape the roster shows before anyone presses to reveal.
fn mask_id_no(id_no: &str) -> String {
    let length = id_no.chars().count();
    if length < 6 {
        return id_no.to_string();
    }
    let head: String = id_no.chars().take(3).collect();
    let tail: String = id_no.chars().skip(length - 2).collect();
    format!("{head}{}{tail}", "*".repeat(length - 5))
}

/// Cut a roster row down to what an AI client is granted.
///
/// `id_no` is masked rather than removed so the roster can still answer
/// whether a number is on file at all. The amounts and the bank accounts go
/// entirely: they leave the building in whatever the model is talking to, and
/// nothing an AI is asked about the roster needs them.
fn redact_for_ai(row: &mut Value) {
    let Some(fields) = row.as_object_mut() else {
        return;
    };
    if let Some(Value::String(id_no)) = fields.get("id_no") {
        let masked = mask_id_no(id_no);
        fields.insert("id_no".into(), json!(masked));
    }
    for key in [
        "base_salary",
        "labor_insured_salary",
        "health_insured_salary",
        "pension_wage",
        "bank_accounts",
        "birthday",
    ] {
        fields.remove(key);
    }
}

/// Voucher status as the screens name it. The stored value is an internal
/// code and must not reach a page.
fn voucher_status_label(status: Option<&str>) -> &'static str {
    match status {
        Some("draft") => "待確認",
        Some("pending") => "待過帳",
        Some("posted") => "已過帳",
        Some("void") => "已作廢",
        _ => "—",
    }
}

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetHrDashboard),
        Box::new(ListEmployees),
        Box::new(ListEmployeeDocuments),
        Box::new(UploadEmployeeDocument),
        Box::new(GetEmployeeDocument),
    ]
}

/// Completed months of service up to a date. Calendar arithmetic, not
/// regulation.
pub(crate) fn service_months(hired: NaiveDate, as_of: NaiveDate) -> u32 {
    if as_of < hired {
        return 0;
    }
    let mut months =
        (as_of.year() - hired.year()) * 12 + as_of.month() as i32 - hired.month() as i32;
    if as_of.day() < hired.day() {
        months -= 1;
    }
    months.max(0) as u32
}

/// Plain-language service length, e.g. "6 years 1 month".
pub(crate) fn service_label(months: u32) -> String {
    match (months / 12, months % 12) {
        (0, m) => format!("{m} 個月"),
        (y, 0) => format!("{y} 年"),
        (y, m) => format!("{y} 年 {m} 個月"),
    }
}

pub(crate) fn employment_label(kind: &str) -> &'static str {
    match kind {
        "part_time" => "兼職",
        "contract" => "約聘",
        _ => "正職",
    }
}

/// Pay basis code stored on an hourly worker. It is a separate axis from
/// `employment_type`: a part timer may be on a monthly salary and an hourly
/// worker may be full time, so payroll asks this one.
pub(crate) const BASIS_HOURLY: &str = "hourly";
pub(crate) const BASIS_MONTHLY: &str = "monthly";

/// Insured as the company's own owner rather than as one of its employees.
pub(crate) const INSURED_OWNER: &str = "owner";
pub(crate) const INSURED_EMPLOYEE: &str = "employee";

pub(crate) fn pay_basis_label(basis: &str) -> &'static str {
    match basis {
        BASIS_HOURLY => "時薪",
        _ => "月薪",
    }
}

pub(crate) fn insured_as_label(insured_as: &str) -> &'static str {
    match insured_as {
        INSURED_OWNER => "負責人",
        _ => "受僱者",
    }
}

pub(crate) fn status_label(status: &str) -> &'static str {
    match status {
        "leaving" => "離職中",
        "left" => "已離職",
        _ => "在職",
    }
}

// HR dashboard

struct GetHrDashboard;

#[async_trait]
impl Operation for GetHrDashboard {
    fn name(&self) -> &'static str {
        "get_hr_dashboard"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀人資總覽：在職人數、本月到離職、待簽核的休假單，以及人資與財務各條銜接的接上狀態"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let month = ctx.today.format("%Y-%m").to_string();
        let staff = ctx.store.employees(true, None).await?;
        let leaves = ctx.store.leave_requests(false, 200).await?;
        let runs = ctx.store.payroll_runs(24).await?;

        let active = staff.iter().filter(|e| e.status != "left").count();
        let leaving = staff.iter().filter(|e| e.status == "leaving").count();
        let joined = staff
            .iter()
            .filter(|e| e.hired_on.format("%Y-%m").to_string() == month)
            .count();
        let departed = staff
            .iter()
            .filter(|e| {
                e.left_on
                    .is_some_and(|d| d.format("%Y-%m").to_string() == month)
            })
            .count();
        let latest = runs.first();
        let (posted, in_transit) = ctx.store.payroll_totals().await?;
        let gl = ctx.store.balance_of(&["6111"], true).await?;
        let gap = posted - gl;

        let stats = json!([
            { "kicker": "在職人數", "value": active.to_string(),
              "note": format!("其中 {leaving} 位離職中") },
            { "kicker": "這個月進出", "value": format!("+{joined} / -{departed}"),
              "note": format!("在職 {active} 位") },
            { "kicker": "待簽核休假單", "value": leaves.len().to_string(),
              "note": "休假單簽核" },
            { "kicker": latest.map(|r| format!("{} 薪資成本", r.period)).unwrap_or_else(|| "薪資成本".into()),
              "value": latest.map(|r| money(r.gross_total)).unwrap_or_else(|| "—".into()),
              "note": latest.map(|r| format!("公司另負擔 {}", money(r.employer_total)))
                        .unwrap_or_else(|| "尚無任何期別的計算紀錄".into()) },
        ]);

        let mut todos = Vec::new();
        if !leaves.is_empty() {
            todos.push(json!({
                "count": leaves.len().to_string(),
                "label": "張休假單等簽核",
                "due": leaves.first().map(|l| l.from_on.to_string()).unwrap_or_default(),
                "screen": "hr_att",
            }));
        }
        if !in_transit.is_zero() {
            todos.push(json!({
                "count": "1",
                "label": "期薪資傳票尚未過帳",
                "due": money(in_transit),
                "screen": "gl",
            }));
        }
        if ctx.store.payroll_run(&month).await?.is_none() {
            todos.push(json!({
                "count": "1",
                "label": format!("{month} 尚未計算薪資"),
                "due": "薪資作業",
                "screen": "hr_pay",
            }));
        }

        // The four HR-to-finance links. Whether each is wired is a field, not
        // copy: writing all four in the same tone would be misleading.
        let links = json!([
            {
                "label": "薪資過帳 → 傳票與總帳 6111",
                "implemented": true,
                "detail": match latest {
                    Some(r) => format!(
                        "{} 的薪資產生 {}（{}），應發 {} 計入 6111 薪資支出。人資已過帳合計 {}，總帳 6111 為 {}；兩條獨立計算路徑{}。",
                        r.period,
                        r.voucher_no.clone().unwrap_or_else(|| "—".into()),
                        voucher_status_label(r.voucher_status.as_deref()),
                        money(r.gross_total),
                        money(posted),
                        money(gl),
                        if gap.is_zero() { "相符" } else { "不符" }
                    ),
                    None => "尚無任何期別的薪資計算紀錄。".into(),
                },
                "screen": "hr_pay",
            },
            {
                "label": "薪資費用 → 部門損益",
                "implemented": true,
                "detail": "薪資傳票的借方逐部門各一行。未指定部門的員工彙總為「未指定」一行，不分攤。",
                "screen": "budget",
            },
            {
                "label": "代扣所得稅 → 2117 代扣款",
                "implemented": true,
                "partial": true,
                "detail": "員工自付的勞保、健保與代扣所得稅合併為一行貸 2117 代扣款。繳給國稅局與勞保局之後，到「傳票 · 總帳」開一張傳票沖銷這個科目。",
                "screen": "gl",
            },
            {
                "label": "公司負擔的勞健保與勞退 → 費用與應付",
                "implemented": false,
                "detail": match latest {
                    Some(r) => format!(
                        "{} 公司負擔 {}，總帳尚無這筆金額。要入帳請先在「基礎設定」新增保險費科目，再開立借保險費／貸應付費用的傳票。",
                        r.period,
                        money(r.employer_total)
                    ),
                    None => "公司負擔的勞健保與勞退目前不會產生任何分錄。".into(),
                },
                "screen": "hr_ins",
            },
        ]);

        let summary = format!(
            "讀人資總覽 · 在職 {active} 人 · 本月 +{joined}/-{departed} · {} 張休假單待簽",
            leaves.len()
        );
        Ok(OpOutcome::new(
            json!({
                "month": month,
                "stats": stats,
                "todos": todos,
                "links": links,
                "headcount": active,
                "leaving": leaving,
                "payroll_reconciled": gap.is_zero(),
                "payroll_not_yet_posted": in_transit.to_string(),
                // Both sides of the reconciliation, so the screen can print the
                // numbers rather than only claiming they agree.
                "payroll_posted_total": posted.to_string(),
                "payroll_gl_total": gl.to_string(),
            }),
            summary,
        ))
    }
}

// Employee roster

struct ListEmployees;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListEmployeesInput {
    /// Search name, employee number, job title or department.
    #[serde(default)]
    pub q: Option<String>,
    /// Include departed employees; excluded by default.
    #[serde(default)]
    pub include_left: bool,
}

#[async_trait]
impl Operation for ListEmployees {
    fn name(&self) -> &'static str {
        "list_employees"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀員工名冊：部門、職稱、到職日、年資、特休額度（額度依年資由特休規則計算），以及約定的底薪與三個投保級距。逐月薪資單另有操作，且限負責人。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListEmployeesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListEmployeesInput = parse(input)?;
        let staff = ctx.store.employees(i.include_left, i.q.as_deref()).await?;
        let rs = laysi_regulation::resolve("annual_leave", ctx.today)?;
        let labor = laysi_regulation::resolve("labor_insurance", ctx.today)?;
        let wage = laysi_regulation::resolve("minimum_wage", ctx.today)?;
        let taken = ctx.store.annual_leave_taken(ctx.today.year()).await?;
        // The roster's own department picker. Reading it from list_dimensions
        // would put the roster behind a second permission, and that permission
        // also opens the settings screen.
        let departments = ctx
            .store
            .dimensions()
            .await?
            .into_iter()
            .find(|d| d.code == "department")
            .map(|d| d.values)
            .unwrap_or_default();

        let mut rows = Vec::with_capacity(staff.len());
        let mut by_dept: Vec<(String, usize)> = Vec::new();
        let mut months_total = 0u32;
        let mut active = 0usize;

        for e in &staff {
            let months = service_months(e.hired_on, ctx.today);
            let quota = laysi_regulation::annual_leave_days(rs, months)?;
            let used = taken
                .iter()
                .find(|(id, _)| *id == e.id)
                .map(|(_, d)| *d)
                .unwrap_or(Money::ZERO);

            if e.status != "left" {
                active += 1;
                months_total += months;
                let label = e.dept_label.clone().unwrap_or_else(|| "未指定".into());
                match by_dept.iter_mut().find(|(d, _)| *d == label) {
                    Some(x) => x.1 += 1,
                    None => by_dept.push((label, 1)),
                }
            }

            rows.push(json!({
                "emp_no": e.emp_no,
                "name": e.name,
                "dept_code": e.dept_code,
                "dept_label": e.dept_label,
                "title": e.title,
                "employment_type": e.employment_type,
                "employment_label": employment_label(&e.employment_type),
                "pay_basis": e.pay_basis,
                "pay_basis_label": pay_basis_label(&e.pay_basis),
                "insured_as": e.insured_as,
                "insured_as_label": insured_as_label(&e.insured_as),
                "birthday": e.birthday,
                "hired_on": e.hired_on,
                "left_on": e.left_on,
                // The roster draws the button that issues a login, and the
                // account list is administrator only.
                "has_login": e.has_login,
                "status": e.status,
                "status_label": status_label(&e.status),
                "service_months": months,
                "service_label": service_label(months),
                // Leave entitlement is computed from the rules, not stored.
                "annual_leave_days": quota,
                "annual_leave_taken": used.to_string(),
                "annual_leave_left": (Money::from(quota) - used).to_string(),
                // Masking is a display decision; the roster returns what is
                // stored, and the screen decides whether to show it.
                "id_no": e.id_no,
                // Terms of employment, not a payslip. Without them no screen
                // in the system says what someone is paid today, so a rise
                // has to be typed from memory, and the edit drawer cannot show
                // the value it is about to leave alone. `get_insurance_summary`
                // already lists the three insured grades per person under this
                // same permission.
                "base_salary": e.base_salary.to_string(),
                "labor_insured_salary": e.labor_insured_salary.to_string(),
                "health_insured_salary": e.health_insured_salary.to_string(),
                "pension_wage": e.pension_wage.to_string(),
                // Full numbers, under the same permission as the insured
                // brackets: the drawer that edits them has to show what it is
                // about to replace. Self-service sees only the last digits.
                "bank_accounts": e.bank_accounts,
                "note": e.note,
            }));
        }

        if ctx.channel == Channel::Mcp {
            for row in &mut rows {
                redact_for_ai(row);
            }
        }

        let avg_months = if active == 0 {
            0
        } else {
            months_total / active as u32
        };
        by_dept.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        let summary = format!(
            "讀員工名冊 · {} 筆（在職 {active}）· 平均年資 {} · 特休依特休規則 {}",
            rows.len(),
            service_label(avg_months),
            rs.version
        );
        Ok(OpOutcome::new(
            json!({
                "employees": rows,
                "total": rows.len(),
                "active": active,
                "leaving": staff.iter().filter(|e| e.status == "leaving").count(),
                "by_dept": by_dept.iter()
                    .map(|(d, n)| json!({ "label": d, "count": n }))
                    .collect::<Vec<_>>(),
                "departments": departments,
                "avg_service_months": avg_months,
                "avg_service_label": service_label(avg_months),
                "annual_leave_ruleset": crate::ops::insurance::ruleset_json(rs),
                // What the drawer fills the three filed brackets with once a
                // monthly salary is typed. The two bounds are the only ones the
                // rules carry, so the suggestion is a starting point HR edits,
                // not the grade table itself; an hourly rate gets none, because
                // the month it maps to is not on file.
                "insured_suggestion": {
                    "floor": wage.decimal("monthly")?.to_string(),
                    "labor_ceiling": labor.decimal("max_insured_salary")?.to_string(),
                    "note": "帶入的數字尚未對照投保薪資分級表，請依申報值調整。",
                },
                // A blank column reads as "no data" more easily than a sentence
                // explaining where the figure lives.
                "salary_note": "個人薪資明細見「薪資作業」，限負責人檢視；投保級距見「勞健保與勞退」，該數字為向勞保局申報值，供會計核對繳款單。",
            }),
            summary,
        ))
    }
}

// Personnel attachments
//
// Scans of an identity card, a signed contract, a licence. They use the same
// two tables, the same bucket and the same BLAKE3 check as every other file;
// what is different is the way in. `manage_documents` opens every receipt and
// bank statement in the company, which is not a set the HR role holds, so
// these three operations take an employee number and never a free document
// number, and refuse anything not linked to an employee.

fn employee_document_kind(kind: Option<&str>) -> Result<&str, OpError> {
    let kind = kind.unwrap_or(EMPLOYEE_KINDS[0]);
    if !EMPLOYEE_KINDS.contains(&kind) {
        return Err(OpError::BadInput(format!(
            "未支援「{kind}」附件類型，可用類型：{}",
            EMPLOYEE_KINDS.map(kind_label).join("、")
        )));
    }
    Ok(kind)
}

struct ListEmployeeDocuments;

#[derive(Deserialize, JsonSchema)]
pub struct EmployeeDocumentsInput {
    /// Employee number from the roster.
    pub emp_no: String,
}

#[async_trait]
impl Operation for ListEmployeeDocuments {
    fn name(&self) -> &'static str {
        "list_employee_documents"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀某位員工的附件清單：檔名、類型、版次、大小與內容雜湊。內容要另外取回。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<EmployeeDocumentsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: EmployeeDocumentsInput = parse(input)?;
        let employee = ctx.store.employee(i.emp_no.trim()).await?;
        let docs = ctx
            .store
            .documents_linked_to(EMPLOYEE_LINK_TYPE, &employee.emp_no)
            .await?;
        let bytes: i64 = docs.iter().map(|d| d.byte_size).sum();

        let summary = format!(
            "讀 {} {} 的附件 · {} 份／{}",
            employee.emp_no,
            employee.name,
            docs.len(),
            human_size(bytes)
        );
        Ok(OpOutcome::new(
            json!({
                "emp_no": employee.emp_no,
                "employee_name": employee.name,
                "documents": docs.iter().map(document_json).collect::<Vec<_>>(),
                "kinds": EMPLOYEE_KINDS.iter()
                    .map(|k| json!({ "kind": k, "label": kind_label(k) }))
                    .collect::<Vec<_>>(),
                "max_object_bytes": MAX_OBJECT_BYTES,
            }),
            summary,
        ))
    }
}

struct UploadEmployeeDocument;

#[derive(Deserialize, JsonSchema)]
pub struct UploadEmployeeDocumentInput {
    /// Employee number from the roster.
    pub emp_no: String,
    /// File name. Non-ASCII names are kept; the object key is transliterated.
    pub file_name: String,
    /// File contents, base64.
    pub content_base64: String,
    /// Display name. Defaults to the file name.
    #[serde(default)]
    pub name: Option<String>,
    /// One of `id_document`, `contract`, `personnel`. Defaults to
    /// `id_document`.
    #[serde(default)]
    pub kind: Option<String>,
    /// When set, adds a revision to an attachment this employee already has.
    #[serde(default)]
    pub doc_no: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for UploadEmployeeDocument {
    fn name(&self) -> &'static str {
        "upload_employee_document"
    }
    fn risk(&self) -> Risk {
        // Writes to object storage, and what lands there is never deleted.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "為名冊上的人上傳一份附件，例如證件掃描檔或已簽署的合約。內容以 base64 傳入，存進物件儲存並記下內容雜湊。同一個附件編號再上傳一次會存成新版本。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UploadEmployeeDocumentInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UploadEmployeeDocumentInput = parse(input)?;
        let objects = ctx.objects()?;
        let employee = ctx.store.employee(i.emp_no.trim()).await?;
        let kind = employee_document_kind(i.kind.as_deref())?;

        let file_name = i.file_name.trim();
        if file_name.is_empty() {
            return Err(OpError::BadInput("檔名要填寫".into()));
        }
        let body = base64::engine::general_purpose::STANDARD
            .decode(i.content_base64.trim())
            .map_err(|e| OpError::BadInput(format!("檔案內容不是合法的 base64：{e}")))?;
        if body.is_empty() {
            return Err(OpError::BadInput("檔案是空的".into()));
        }
        if body.len() > MAX_OBJECT_BYTES {
            return Err(OpError::BadInput(format!(
                "檔案 {} 位元組，超過單檔上限 {} 位元組",
                body.len(),
                MAX_OBJECT_BYTES
            )));
        }

        // A revision may only be added to this employee's own attachment;
        // otherwise the number is a way to write into any document at all.
        if let Some(doc_no) = i.doc_no.as_deref() {
            let existing = ctx.store.document_version(doc_no.trim(), None).await?;
            if existing.link_type != EMPLOYEE_LINK_TYPE || existing.link_ref != employee.emp_no {
                return Err(OpError::BadInput(format!(
                    "{doc_no} 不是 {} {} 的附件",
                    employee.emp_no, employee.name
                )));
            }
        }

        // Same two steps as every other upload: the number and revision build
        // the object key, and the row is written only once the file lands.
        let slot = ctx
            .store
            .reserve_document_slot(ReserveDocument {
                doc_no: i.doc_no.as_deref().map(str::trim),
                name: i.name.as_deref().unwrap_or(file_name),
                kind,
                link_type: EMPLOYEE_LINK_TYPE,
                link_ref: &employee.emp_no,
                note: i.note.as_deref().unwrap_or(""),
                today: ctx.today,
                actor: &ctx.actor,
            })
            .await?;

        let key = storage::object_key(
            &ctx.today.format("%y%m").to_string(),
            &slot.doc_no,
            slot.version_no,
            file_name,
        );
        let content_type = storage::guess_content_type(file_name);
        let stored = objects.put(&key, &body, content_type, file_name).await?;

        let doc = match ctx
            .store
            .record_document_version(&slot, &stored, file_name, &ctx.actor)
            .await
        {
            Ok(doc) => doc,
            Err(e) => {
                objects.discard(&stored.key).await;
                return Err(e.into());
            }
        };

        let summary = format!(
            "上傳 {} · {} v{} {} · 指紋 {} · 掛在 {} {}",
            doc.file_name,
            doc.doc_no,
            doc.version_no,
            human_size(doc.byte_size),
            short_checksum(&doc.checksum),
            employee.emp_no,
            employee.name
        );
        Ok(OpOutcome::new(
            json!({
                "emp_no": employee.emp_no,
                "document": document_json(&doc),
                "new_version": doc.version_no > 1,
            }),
            summary,
        ))
    }
}

struct GetEmployeeDocument;

#[derive(Deserialize, JsonSchema)]
pub struct GetEmployeeDocumentInput {
    /// Employee number the attachment belongs to.
    pub emp_no: String,
    /// Attachment number, e.g. `D2608-004`.
    pub doc_no: String,
    /// Revision. Defaults to the latest.
    #[serde(default)]
    pub version: Option<i32>,
    /// Whether to return the content as base64. Off by default, which only
    /// verifies the hash.
    #[serde(default)]
    pub include_content: bool,
}

#[async_trait]
impl Operation for GetEmployeeDocument {
    fn name(&self) -> &'static str {
        "get_employee_document"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "取回一份員工附件並重算雜湊與登記值比對。預設只回驗證結果與後設資料；要求回傳內容時才以 base64 附上內容。雜湊不符會回報錯誤，不回傳內容。附件不屬於這位員工時一律查無此附件。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<GetEmployeeDocumentInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: GetEmployeeDocumentInput = parse(input)?;
        let objects = ctx.objects()?;
        let employee = ctx.store.employee(i.emp_no.trim()).await?;
        let doc = ctx
            .store
            .document_version(i.doc_no.trim(), i.version)
            .await?;
        // Not a refusal: a refusal would confirm that the number exists and
        // says whose it is.
        if doc.link_type != EMPLOYEE_LINK_TYPE || doc.link_ref != employee.emp_no {
            return Err(OpError::BadInput(format!("查無附件 {}", i.doc_no.trim())));
        }

        let body = objects.get(&doc.object_key, &doc.checksum).await?;
        let recomputed = storage::checksum(&body);
        let content = i
            .include_content
            .then(|| base64::engine::general_purpose::STANDARD.encode(&body));

        let summary = format!(
            "取回 {} v{} {} · 雜湊比對相符（{}）",
            doc.doc_no,
            doc.version_no,
            doc.file_name,
            short_checksum(&recomputed)
        );
        Ok(OpOutcome::new(
            json!({
                "emp_no": employee.emp_no,
                "document": document_json(&doc),
                "verified": recomputed == doc.checksum,
                "checksum_recomputed": recomputed,
                "byte_size_actual": body.len(),
                "content_base64": content,
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// Service length is calendar arithmetic: a month counts once complete.
    #[test]
    fn service_months_count_only_completed_months() {
        assert_eq!(
            service_months(d(2026, 7, 16), d(2026, 7, 26)),
            0,
            "還不滿一個月"
        );
        assert_eq!(
            service_months(d(2026, 7, 16), d(2026, 8, 15)),
            0,
            "差一天不算滿"
        );
        assert_eq!(service_months(d(2026, 7, 16), d(2026, 8, 16)), 1);
        assert_eq!(
            service_months(d(2026, 2, 16), d(2026, 7, 26)),
            5,
            "到職五個月"
        );
        assert_eq!(
            service_months(d(2019, 6, 17), d(2026, 7, 26)),
            85,
            "七年一個月"
        );
        assert_eq!(
            service_months(d(2015, 3, 2), d(2026, 7, 26)),
            136,
            "十一年四個月"
        );
        assert_eq!(service_months(d(2027, 1, 1), d(2026, 7, 26)), 0, "還沒到職");
    }

    /// Leave entitlement comes entirely from the `annual_leave` rules; this
    /// file holds no day count.
    ///
    /// A different ruleset gives a different answer for the same service
    /// length, without a change here.
    #[test]
    fn annual_leave_quota_comes_from_the_ruleset() {
        let rs = laysi_regulation::resolve("annual_leave", d(2026, 7, 26)).unwrap();
        let quota = |hired: NaiveDate| {
            laysi_regulation::annual_leave_days(rs, service_months(hired, d(2026, 7, 26))).unwrap()
        };
        assert_eq!(quota(d(2026, 7, 16)), 0, "到職十天，還沒有特休");
        assert_eq!(quota(d(2026, 2, 16)), 0, "到職五個月，差一個月");
        assert_eq!(quota(d(2025, 3, 3)), 7, "年資一年四個月");
        assert_eq!(
            quota(d(2024, 8, 19)),
            7,
            "年資一年十一個月：差一個月才跳到十日"
        );
        assert_eq!(quota(d(2022, 9, 5)), 14, "年資三年十個月");
        assert_eq!(quota(d(2019, 6, 17)), 15, "年資七年一個月");
        assert_eq!(
            quota(d(2015, 3, 2)),
            17,
            "年資十一年四個月：十六日再加一年一日"
        );

        // A different ruleset: 10 days after one full year. This file is
        // unchanged.
        let mut next = rs.clone();
        next.version = "2027.1".into();
        next.payload["tiers"][1]["days"] = json!("10");
        assert_eq!(
            laysi_regulation::annual_leave_days(
                &next,
                service_months(d(2025, 3, 3), d(2026, 7, 26))
            )
            .unwrap(),
            10
        );
    }

    #[test]
    fn service_labels_read_like_people_talk() {
        assert_eq!(service_label(0), "0 個月");
        assert_eq!(service_label(5), "5 個月");
        assert_eq!(service_label(12), "1 年");
        assert_eq!(service_label(85), "7 年 1 個月");
    }

    #[test]
    fn masking_keeps_the_first_three_and_last_two_characters() {
        assert_eq!(mask_id_no("A123456789"), "A12*****89");
        assert_eq!(mask_id_no("A123"), "A123");
    }

    /// The MCP grant for this tool says it carries no pay. Salary and the
    /// insured grades were added to the roster afterwards for the edit
    /// drawer, which made that promise false on the AI channel.
    #[test]
    fn the_ai_roster_carries_no_pay_and_no_account_numbers() {
        let mut row = json!({
            "emp_no": "E001",
            "name": "王小明",
            "id_no": "A123456789",
            "base_salary": "80000",
            "labor_insured_salary": "45800",
            "health_insured_salary": "45800",
            "pension_wage": "45800",
            "bank_accounts": [{ "bank_name": "第一銀行", "account_no": "12345678901" }],
            "birthday": "1990-05-02",
        });
        redact_for_ai(&mut row);

        assert_eq!(row["id_no"], json!("A12*****89"));
        for gone in [
            "base_salary",
            "labor_insured_salary",
            "health_insured_salary",
            "pension_wage",
            "bank_accounts",
            "birthday",
        ] {
            assert!(row.get(gone).is_none(), "{gone} 不該留在 AI 讀到的名冊上");
        }
        assert_eq!(row["name"], json!("王小明"), "姓名與編號仍然要回");
    }
}
