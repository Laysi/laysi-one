//! Employee roster, onboarding and offboarding movements, HR dashboard.
//!
//! [`ListEmployees`] is `ReadOnly` and returns neither monthly salary nor
//! insured salary brackets. Per-person amounts have exactly two exits:
//! `list_payslips` (boss only) and `get_insurance_summary` (insured brackets
//! are what was filed with the labour insurance bureau, and accounting needs
//! them to check the statements). The roster returns an explanatory line
//! instead of blank columns.
//!
//! Annual leave entitlement is `annual_leave_days(ruleset, service_months)`,
//! never stored; days taken are summed from `leave_request`.
//!
//! Which approval steps onboarding, offboarding, pay changes and transfers
//! require lives in `hr_flow_step`. This module only compares completed steps
//! against that table to determine where a movement is stuck.
//!
//! Closing a movement writes its change to the employee record in the same
//! transaction. Completing a leaving flow marks the employee `left` and closes
//! their access: the two are one event, because the roster says the company
//! has stopped employing this person, and nothing checks the effective date
//! afterwards, so a login left open is open for good. The last remaining
//! administrator is the exception, because no operation grants a role and the
//! installation would have nobody able to open the accounts screen. A pay
//! change writes `base_salary` and a transfer writes `dept_id`, both read from
//! `to_value`, which is why that field is checked when the movement is raised
//! rather than only at the last step.
//!
//! Each of the four HR-to-finance links on the dashboard carries an
//! `implemented` flag: the ones that are wired report their evidence (voucher
//! number, reconciliation result), the ones that are not state what is
//! missing.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
use laysi_store::{EmployeeMovement, HrFlowStep};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

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
        Box::new(ListEmployeeMovements),
        Box::new(RecordEmployeeMovement),
        Box::new(CompleteMovementStep),
    ]
}

/// Completed months of service up to a date. Calendar arithmetic, not
/// regulation.
pub(crate) fn service_months(hired: NaiveDate, as_of: NaiveDate) -> u32 {
    if as_of < hired {
        return 0;
    }
    let mut months = (as_of.year() - hired.year()) * 12 + as_of.month() as i32 - hired.month() as i32;
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

pub(crate) fn status_label(status: &str) -> &'static str {
    match status {
        "leaving" => "離職中",
        "left" => "已離職",
        _ => "在職",
    }
}

pub(crate) fn movement_kind_label(kind: &str) -> &'static str {
    match kind {
        "hire" => "到職",
        "leave" => "離職",
        "salary_change" => "調薪",
        "transfer" => "調部門",
        _ => "異動",
    }
}

/// JSON shape of a movement. Which step it has reached is derived, not a
/// stored string.
fn movement_json(m: &EmployeeMovement, flow: &[HrFlowStep]) -> Value {
    let steps: Vec<&HrFlowStep> = flow.iter().filter(|s| s.kind == m.kind).collect();
    let next = steps.iter().find(|s| !m.step_done(s.step_no));
    json!({
        "movement_no": m.movement_no,
        "emp_no": m.emp_no,
        "name": m.name,
        "dept_label": m.dept_label,
        "kind": m.kind,
        "kind_label": movement_kind_label(&m.kind),
        "effective_on": m.effective_on,
        "from_value": m.from_value,
        "to_value": m.to_value,
        "reason": m.reason,
        "status": m.status,
        "done_count": m.done_steps.len(),
        "step_count": steps.len(),
        "step_label": match (&m.status[..], next) {
            ("done", _) => "已完成".to_string(),
            (_, Some(s)) => format!("等「{}」", s.label),
            (_, None) => "等結案".to_string(),
        },
        "next_step": next.map(|s| json!({ "step_no": s.step_no, "label": s.label, "note": s.note })),
        "steps": steps.iter().map(|s| json!({
            "step_no": s.step_no,
            "label": s.label,
            "note": s.note,
            "done": m.step_done(s.step_no),
            "done_by": m.done_steps.iter().find(|d| d.step_no == s.step_no).map(|d| d.done_by.clone()),
            "done_at": m.done_steps.iter().find(|d| d.step_no == s.step_no).map(|d| d.done_at),
            "done_note": m.done_steps.iter().find(|d| d.step_no == s.step_no).map(|d| d.note.clone()),
        })).collect::<Vec<_>>(),
        "created_at": m.created_at,
        "created_by": m.created_by,
    })
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
        "讀人資總覽：在職人數、本月到離職、待簽核的休假單與異動單，以及人資與財務各條銜接的接上狀態"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: crate::Empty = parse(input)?;
        let month = ctx.today.format("%Y-%m").to_string();
        let staff = ctx.store.employees(true, None).await?;
        let flow = ctx.store.hr_flow_steps().await?;
        let movements = ctx.store.employee_movements(200).await?;
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
            .filter(|e| e.left_on.is_some_and(|d| d.format("%Y-%m").to_string() == month))
            .count();
        let open_movements = movements.iter().filter(|m| m.status == "open").count();

        let latest = runs.first();
        let (posted, in_transit) = ctx.store.payroll_totals().await?;
        let gl = ctx.store.balance_of(&["6111"], true).await?;
        let gap = posted - gl;

        let stats = json!([
            { "kicker": "在職人數", "value": active.to_string(),
              "note": format!("其中 {leaving} 位離職中") },
            { "kicker": "這個月進出", "value": format!("+{joined} / -{departed}"),
              "note": format!("{open_movements} 張異動單還在跑") },
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
        if open_movements > 0 {
            todos.push(json!({
                "count": open_movements.to_string(),
                "label": "張異動單尚未結案",
                "due": "到離職與異動",
                "screen": "hr_mv",
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
                "detail": "薪資傳票的借方逐部門各一行，因此「預算與管理」的部門損益可看到各部門人事成本。未指定部門的員工彙總為「未指定」一行，不分攤。",
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
            "讀人資總覽 · 在職 {active} 人 · 本月 +{joined}/-{departed} · {} 張休假單待簽 · {open_movements} 張異動單進行中",
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
                "flow_kinds": flow.iter().map(|s| s.kind.clone()).collect::<Vec<_>>(),
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
        let taken = ctx.store.annual_leave_taken(ctx.today.year()).await?;

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
                "hired_on": e.hired_on,
                "left_on": e.left_on,
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

        let avg_months = if active == 0 { 0 } else { months_total / active as u32 };
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
                "avg_service_months": avg_months,
                "avg_service_label": service_label(avg_months),
                "annual_leave_ruleset": crate::ops::insurance::ruleset_json(rs),
                // A blank column reads as "no data" more easily than a sentence
                // explaining where the figure lives.
                "salary_note": "個人薪資明細見「薪資作業」，限負責人檢視；投保級距見「勞健保與勞退」，該數字為向勞保局申報值，供會計核對繳款單。",
            }),
            summary,
        ))
    }
}

// Onboarding, offboarding and movements

struct ListEmployeeMovements;

#[async_trait]
impl Operation for ListEmployeeMovements {
    fn name(&self) -> &'static str {
        "list_employee_movements"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀到離職與異動單：每一張目前的關卡與剩餘關卡。流程本身（到職四步、離職四步…）為公司設定。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::LimitInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: crate::LimitInput = parse(input)?;
        let flow = ctx.store.hr_flow_steps().await?;
        let movements = ctx.store.employee_movements(i.limit.unwrap_or(50)).await?;
        let departments = ctx.store.departments().await?;

        let mut kinds: Vec<Value> = Vec::new();
        for kind in ["hire", "leave", "salary_change", "transfer"] {
            let steps: Vec<Value> = flow
                .iter()
                .filter(|s| s.kind == kind)
                .map(|s| json!({ "step_no": s.step_no, "label": s.label, "note": s.note }))
                .collect();
            if !steps.is_empty() {
                kinds.push(json!({
                    "kind": kind,
                    "label": movement_kind_label(kind),
                    "steps": steps,
                }));
            }
        }

        let open = movements.iter().filter(|m| m.status == "open").count();
        let summary = format!(
            "讀到離職與異動 · {} 張（{open} 張進行中）· 流程依公司設定",
            movements.len()
        );
        Ok(OpOutcome::new(
            json!({
                "movements": movements.iter().map(|m| movement_json(m, &flow)).collect::<Vec<_>>(),
                "flows": kinds,
                "open_count": open,
                // A transfer names its destination department, so the screen
                // can offer the list instead of asking for a code from memory.
                "departments": departments.iter()
                    .filter(|d| d.active)
                    .map(|d| json!({ "code": d.code, "label": d.label }))
                    .collect::<Vec<_>>(),
            }),
            summary,
        ))
    }
}

struct RecordEmployeeMovement;

#[derive(Deserialize, JsonSchema)]
pub struct RecordMovementInput {
    /// Employee number, e.g. `E006`.
    pub emp_no: String,
    /// `hire`, `leave`, `salary_change` or `transfer`.
    pub kind: String,
    /// Effective date (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub effective_on: Option<NaiveDate>,
    /// Pay and department changes must show what changed from what.
    #[serde(default)]
    pub from_value: Option<String>,
    /// The new monthly base salary for `salary_change`, the destination
    /// department for `transfer`. Closing the movement writes it to the
    /// employee record, so it is checked here.
    #[serde(default)]
    pub to_value: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[async_trait]
impl Operation for RecordEmployeeMovement {
    fn name(&self) -> &'static str {
        "record_employee_movement"
    }
    fn risk(&self) -> Risk {
        // Only raises the request. Enrolment, withdrawal and pay changes take
        // effect once the steps are complete.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "開立到職／離職／調薪／調部門的異動單。調薪填調整後的月底薪，調部門填調任部門，最後一關完成時寫入員工主檔。開立離職單時，員工就標示為「離職中」並記錄預定離職日，名冊與薪資作業立即反映，不等最後一關完成。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RecordMovementInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RecordMovementInput = parse(input)?;
        let flow = ctx.store.hr_flow_steps().await?;
        if !flow.iter().any(|s| s.kind == i.kind) {
            return Err(OpError::BadInput(format!(
                "「{}」不是有效的異動種類。目前有：{}",
                i.kind,
                flow.iter()
                    .map(|s| s.kind.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("、")
            )));
        }
        let e = ctx.store.employee(&i.emp_no).await?;
        if e.status == "left" && i.kind != "hire" {
            return Err(OpError::BadInput(format!(
                "{} {} 已經離職了，開不了「{}」異動單",
                e.emp_no,
                e.name,
                movement_kind_label(&i.kind)
            )));
        }

        // The last step of a pay change writes `base_salary` and the last step
        // of a transfer writes `dept_id`. Reading `to_value` here means an
        // unusable target is refused now, not after four approvals.
        let to_value = i.to_value.as_deref().unwrap_or("").trim();
        match i.kind.as_str() {
            "salary_change" if laysi_store::movement_salary_target(to_value).is_none() => {
                return Err(OpError::BadInput(
                    "調整後月底薪要填金額，例如 45000。".to_string(),
                ));
            }
            "transfer" => {
                let departments = ctx.store.departments().await?;
                if laysi_store::match_department(&departments, to_value).is_none() {
                    let hint = laysi_store::department_hint(&departments);
                    return Err(OpError::BadInput(if to_value.is_empty() {
                        format!("調任部門要選一個部門。{hint}")
                    } else {
                        format!("調任部門「{to_value}」不是現有部門。{hint}")
                    }));
                }
            }
            _ => {}
        }

        let m = ctx
            .store
            .record_employee_movement(laysi_store::RecordMovement {
                emp_no: &i.emp_no,
                kind: &i.kind,
                effective_on: i.effective_on.unwrap_or(ctx.today),
                from_value: i.from_value.as_deref().unwrap_or("").trim(),
                to_value,
                reason: i.reason.as_deref().unwrap_or(""),
                actor: &ctx.actor,
            })
            .await?;

        let steps = flow.iter().filter(|s| s.kind == m.kind).count();
        let summary = format!(
            "{} · {} {} {} 生效日 {} · 要走 {} 關",
            m.movement_no,
            m.emp_no,
            m.name,
            movement_kind_label(&m.kind),
            m.effective_on,
            steps
        );
        Ok(OpOutcome::new(json!({ "movement": movement_json(&m, &flow) }), summary))
    }
}

struct CompleteMovementStep;

#[derive(Deserialize, JsonSchema)]
pub struct CompleteMovementStepInput {
    /// Movement number, e.g. `MV2607-001`.
    pub movement_no: String,
    /// Which step to complete. Defaults to the next incomplete one.
    #[serde(default)]
    pub step_no: Option<i32>,
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for CompleteMovementStep {
    fn name(&self) -> &'static str {
        "complete_movement_step"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "將異動單的某一關標記為完成。全部關卡完成時異動單自動結案，並把異動寫入員工主檔：調薪寫入本薪，下一期薪資作業採用新的數字；調部門寫入部門歸屬；離職標示為「已離職」，登入帳號一併停用、登入工作階段結束、MCP Token 全數撤銷，不等生效日。要重新啟用帳號，由系統管理員在「帳號管理」處理。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CompleteMovementStepInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CompleteMovementStepInput = parse(input)?;
        let flow = ctx.store.hr_flow_steps().await?;
        let m = ctx.store.employee_movement(&i.movement_no).await?;

        let step_no = match i.step_no {
            Some(n) => n,
            None => flow
                .iter()
                .filter(|s| s.kind == m.kind)
                .find(|s| !m.step_done(s.step_no))
                .map(|s| s.step_no)
                .ok_or_else(|| {
                    OpError::BadInput(format!("{} 的關卡都完成了", m.movement_no))
                })?,
        };

        let done = ctx
            .store
            .complete_movement_step(laysi_store::CompleteMovementStep {
                movement_no: &i.movement_no,
                step_no,
                actor: &ctx.actor,
                note: i.note.as_deref().unwrap_or(""),
                keep_active_role: crate::ROLE_ADMIN,
            })
            .await?;
        let updated = done.movement;

        let label = flow
            .iter()
            .find(|s| s.kind == updated.kind && s.step_no == step_no)
            .map(|s| s.label.clone())
            .unwrap_or_default();
        let total = flow.iter().filter(|s| s.kind == updated.kind).count();
        let mut closed_note = String::new();
        if let Some(pay) = &done.applied.base_salary {
            closed_note.push_str(&format!(" · 本薪 {} → {}", money(pay.from), money(pay.to)));
        }
        if let Some(dept) = &done.applied.department {
            closed_note.push_str(&format!(
                " · 部門 {} → {}",
                dept.from.as_deref().unwrap_or("未指定"),
                dept.to
            ));
        }
        if !done.closed.logins.is_empty() {
            closed_note.push_str(&format!(" · 停用登入帳號 {}", done.closed.logins.join("、")));
        }
        if done.closed.tokens > 0 {
            closed_note.push_str(&format!(" · 撤銷 {} 組 MCP Token", done.closed.tokens));
        }
        let summary = format!(
            "{} {} {} · 完成第 {step_no} 關「{label}」（{}/{total}）{}{closed_note}",
            updated.movement_no,
            updated.emp_no,
            updated.name,
            updated.done_steps.len(),
            if updated.status == "done" { " · 已結案" } else { "" }
        );
        Ok(OpOutcome::new(
            json!({
                "movement": movement_json(&updated, &flow),
                "step_taken": step_no,
                "finished": updated.status == "done",
                "closed_access": done.closed,
                // What the close actually wrote to the employee record, so the
                // screen can say it instead of only "已結案".
                "applied": {
                    "base_salary": done.applied.base_salary.map(|p| json!({
                        "from": p.from.to_string(),
                        "to": p.to.to_string(),
                    })),
                    "department": done.applied.department.map(|d| json!({
                        "from": d.from,
                        "to": d.to,
                    })),
                },
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
        assert_eq!(service_months(d(2026, 7, 16), d(2026, 7, 26)), 0, "還不滿一個月");
        assert_eq!(service_months(d(2026, 7, 16), d(2026, 8, 15)), 0, "差一天不算滿");
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
            laysi_regulation::annual_leave_days(&next, service_months(d(2025, 3, 3), d(2026, 7, 26)))
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
}
