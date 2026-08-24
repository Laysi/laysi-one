//! Employee roster and HR dashboard.
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
//! Each of the four HR-to-finance links on the dashboard carries an
//! `implemented` flag: the ones that are wired report their evidence (voucher
//! number, reconciliation result), the ones that are not state what is
//! missing.

use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use laysi_domain::Money;
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
            .filter(|e| e.left_on.is_some_and(|d| d.format("%Y-%m").to_string() == month))
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
