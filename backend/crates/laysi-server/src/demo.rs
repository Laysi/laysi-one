//! Demo data for a throwaway installation.
//!
//! The product ships empty on purpose: a fresh install belongs to whoever
//! installed it, and preloading someone else's invoices would make the first
//! screen a lie. This module is the deliberate exception, reachable only by
//! running `laysi-admin seed-demo` by hand, and it refuses to run on an
//! installation that already has vouchers unless forced.
//!
//! Everything goes through `Registry::dispatch`. The point is data that
//! behaves: the receivables tie to the 1131 balance, the payroll run ties to
//! 6111, and every row here was produced by the same operation a user would
//! have called, with the same validation and the same audit trail. There are
//! three exceptions: the e-invoice track and the budget version, which no
//! operation can create, and past clock-in records, which no operation can
//! create *by design*. See [`seed_ledger`], [`seed_budget`] and
//! [`seed_punches`].
//!
//! Amounts are company data, invented for the demo. No regulation figure
//! appears here: insured salaries follow each employee's own salary rather
//! than a bracket table, and every rate comes from `laysi-regulation` by way
//! of the operations themselves.

use anyhow::{bail, Context};
use chrono::{Datelike, Months, NaiveDate, NaiveTime, Weekday};
use laysi_ops::ops::attendance::business_instant;
use laysi_ops::{Channel, OpCtx, Registry};
use laysi_store::Store;
use serde_json::{json, Value};

/// Employee number the CLI links its demo staff login to, so the
/// self-service screens have someone to be.
pub const STAFF_EMPLOYEE_NO: &str = "E003";

/// The roster's manager, whom an owner login is linked to. An owner is an
/// employee as well, with leave and a payslip of their own.
pub const OWNER_EMPLOYEE_NO: &str = "E001";

/// One employee in the demo roster.
struct Person {
    emp_no: &'static str,
    name: &'static str,
    dept: &'static str,
    title: &'static str,
    salary: &'static str,
    /// Months before today that this person was hired. Varying it gives the
    /// annual-leave entitlements something to differ over.
    hired_months_ago: u32,
}

const ROSTER: &[Person] = &[
    Person { emp_no: "E001", name: "林敬淵", dept: "ADMIN", title: "總經理", salary: "96000", hired_months_ago: 64 },
    Person { emp_no: "E002", name: "陳映築", dept: "SALES", title: "業務經理", salary: "62000", hired_months_ago: 39 },
    Person { emp_no: "E003", name: "黃柏叡", dept: "SALES", title: "業務專員", salary: "45000", hired_months_ago: 14 },
    Person { emp_no: "E004", name: "蘇宛柔", dept: "ADMIN", title: "會計", salary: "52000", hired_months_ago: 27 },
    Person { emp_no: "E005", name: "何允中", dept: "TECH", title: "工程師", salary: "58000", hired_months_ago: 7 },
];

/// Runs an operation and fails loudly with the operation's own message.
///
/// A seeding step that silently skipped would leave a half-populated
/// installation that looks like a product bug.
async fn call(
    registry: &Registry,
    ctx: &OpCtx,
    name: &str,
    input: Value,
    calls: &mut usize,
) -> anyhow::Result<Value> {
    let out = registry
        .dispatch(ctx, name, input)
        .await
        .with_context(|| format!("種入示範資料時，操作 {name} 失敗"))?;
    *calls += 1;
    Ok(out)
}

/// The voucher number an operation reports having created.
fn voucher_no(out: &Value) -> anyhow::Result<String> {
    out.get("voucher")
        .and_then(|v| v.get("no"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("操作沒有回傳傳票號碼，無法接著過帳"))
}

fn ctx_for(
    store: &Store,
    actor: &str,
    role: &str,
    employee_id: Option<i32>,
    today: NaiveDate,
) -> OpCtx {
    OpCtx {
        store: store.clone(),
        // Same source the server uses. When the environment is incomplete the
        // document operations say so themselves, which is better than the
        // seed deciding in advance that documents are unavailable.
        objects: laysi_store::ObjectStore::from_env().ok(),
        actor: actor.to_string(),
        actor_role: role.to_string(),
        username: None,
        employee_id,
        prompt: None,
        // Not Mcp: this is a person running a command, and the MCP tool
        // grants exist to limit what AI may call.
        channel: Channel::Ui,
        today,
        offset: laysi_ops::business_offset(),
    }
}

/// `months` before `today`, on `day`, clamped into the target month.
fn back(today: NaiveDate, months: u32, day: u32) -> NaiveDate {
    let base = today.checked_sub_months(Months::new(months)).unwrap_or(today);
    NaiveDate::from_ymd_opt(base.year(), base.month(), day).unwrap_or(base)
}

fn plus_days(date: NaiveDate, days: i64) -> NaiveDate {
    date.checked_add_signed(chrono::Duration::days(days)).unwrap_or(date)
}

/// Seeds a demo company.
///
/// `force` is required once the installation holds vouchers: the guard is
/// there so nobody turns a real ledger into a demo by mistyping a command.
pub async fn seed(store: &Store, today: NaiveDate, force: bool) -> anyhow::Result<usize> {
    let registry = Registry::new();
    let boss = ctx_for(store, "laysi-admin", laysi_ops::ROLE_BOSS, None, today);
    // Company settings sit behind the admin floor, the books behind the owner
    // one. Seeding needs both, which is the same split a real install has.
    let admin = ctx_for(store, "laysi-admin", laysi_ops::ROLE_ADMIN, None, today);
    let mut calls = 0usize;

    let existing = call(&registry, &boss, "search_vouchers", json!({ "limit": 1 }), &mut calls).await?;
    let occupied = existing
        .get("vouchers")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty());
    if occupied && !force {
        bail!(
            "這個安裝已經有傳票了，拒絕種入示範資料。\n\
             示範資料會混進既有帳務，且沒有還原指令。\n\
             確定要在這個資料庫上加示範資料，請加 --force。"
        );
    }

    seed_company(&registry, &admin, &mut calls).await?;
    seed_people(&registry, &boss, today, &mut calls).await?;
    seed_ledger(&registry, &boss, today, &mut calls).await?;
    seed_budget(&registry, store, &boss, today, &mut calls).await?;
    seed_self_service(&registry, store, &boss, today, &mut calls).await?;
    seed_payroll(&registry, &boss, today, &mut calls).await?;
    seed_documents(&registry, &boss, &mut calls).await?;

    Ok(calls)
}

/// A few filed documents, each linked to a record that already exists.
///
/// Skipped rather than fatal when object storage is not configured: a demo
/// without RustFS is still worth having, and the document operations already
/// say plainly that storage is not connected.
async fn seed_documents(
    registry: &Registry,
    ctx: &OpCtx,
    calls: &mut usize,
) -> anyhow::Result<()> {
    if ctx.objects.is_none() {
        return Ok(());
    }
    // Placeholder bytes, not a forged PDF. The screen shows the number, the
    // link and the BLAKE3 digest; a real receipt image would add nothing to
    // that and would imply the demo shipped someone's paperwork.
    let files = [
        ("AP-2601 聯欣物流請款單.txt", "vendor_bill", "ap_bill", "AP-2601", "供應商請款單"),
        ("AP-2604 同茂貿易對帳單.txt", "vendor_bill", "ap_bill", "AP-2604", "月結對帳單"),
        ("AR-2605 宥昇國際訂購單.txt", "contract", "ar_invoice", "AR-2605", "客戶訂購單"),
    ];
    for (file_name, kind, link_type, link_ref, name) in files {
        let body = format!("{name}（示範檔案，非真實單據）\n");
        call(
            registry,
            ctx,
            "upload_document",
            json!({
                "file_name": file_name,
                "name": name,
                "kind": kind,
                "link_type": link_type,
                "link_ref": link_ref,
                "content_base64": base64(body.as_bytes()),
                "note": "由 laysi-admin seed-demo 種入",
            }),
            calls,
        )
        .await?;
    }
    Ok(())
}

/// Standard base64. Written out rather than pulled in as a dependency: this
/// is the only place in the workspace that needs an encoder.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Company identity, departments, the operating bank account and the opening
/// capital that everything else is paid out of.
async fn seed_company(registry: &Registry, ctx: &OpCtx, calls: &mut usize) -> anyhow::Result<()> {
    for (code, value) in [
        ("company_name", "承祐貿易有限公司"),
        ("tax_id", "24681353"),
        ("budget_alert_percent", "85"),
        // Without these the clock-in screens work but decline to say whether
        // anyone was late, which is correct behaviour and a poor
        // demonstration of it.
        ("work_start", "09:00"),
        ("work_end", "18:00"),
        ("late_grace_minutes", "5"),
    ] {
        call(registry, ctx, "update_company_setting", json!({ "code": code, "value": value }), calls).await?;
    }

    for (order, (code, label)) in
        [("SALES", "業務部"), ("ADMIN", "管理部"), ("TECH", "技術部")].iter().enumerate()
    {
        call(
            registry,
            ctx,
            "create_dimension_value",
            json!({
                "dimension_code": "department",
                "code": code,
                "label": label,
                "sort_order": (order as i32 + 1) * 10,
            }),
            calls,
        )
        .await?;
    }

    call(
        registry,
        ctx,
        "create_bank_account",
        json!({ "name": "第一銀行 · 營運帳戶", "account_no": "07310218842", "gl_code": "1102" }),
        calls,
    )
    .await?;
    Ok(())
}

/// The roster.
async fn seed_people(
    registry: &Registry,
    ctx: &OpCtx,
    today: NaiveDate,
    calls: &mut usize,
) -> anyhow::Result<()> {
    for person in ROSTER {
        call(
            registry,
            ctx,
            "create_employee",
            json!({
                "emp_no": person.emp_no,
                "name": person.name,
                "dept_code": person.dept,
                "title": person.title,
                "hired_on": back(today, person.hired_months_ago, 1),
                "base_salary": person.salary,
                // A real filing picks the bracket the salary falls into. The
                // demo files the salary itself, so no bracket table is
                // restated here; the rates still come from the ruleset.
                "labor_insured_salary": person.salary,
                "health_insured_salary": person.salary,
                "pension_wage": person.salary,
            }),
            calls,
        )
        .await?;
    }

    call(
        registry,
        ctx,
        "record_employee_movement",
        json!({
            "emp_no": "E003",
            "kind": "salary_change",
            "effective_on": back(today, 2, 1),
            "from_value": "42000",
            "to_value": "45000",
            "reason": "年度調薪",
        }),
        calls,
    )
    .await?;
    call(
        registry,
        ctx,
        "record_employee_movement",
        json!({
            "emp_no": "E005",
            "kind": "transfer",
            "effective_on": back(today, 1, 1),
            "from_value": "ADMIN",
            "to_value": "TECH",
            "reason": "轉任技術部",
        }),
        calls,
    )
    .await?;
    Ok(())
}

/// One step that ends up creating a voucher.
///
/// These are collected and sorted by date before any of them runs, because
/// voucher numbers are drawn from `doc_sequence`, which keeps one row per
/// prefix and restarts the counter whenever the period changes. Creating a
/// May voucher after a June one therefore reuses a number that already
/// exists. Every step below is dated, so the seed sorts rather than relying
/// on the order the tables happen to be written in.
enum Step {
    /// Shareholder capital. Everything else is paid out of it, so a bank
    /// balance that went negative in month one would be demonstrating a
    /// mistake.
    Opening,
    /// A monthly running cost paid straight from the bank.
    Cost { account: &'static str, amount: &'static str, memo: &'static str },
    ArIssue { no: &'static str, customer: &'static str, amount: &'static str, due: NaiveDate },
    ArReceipt { no: &'static str },
    ApBill {
        no: &'static str,
        vendor: &'static str,
        amount: &'static str,
        account: &'static str,
        due: NaiveDate,
        /// Left unposted for one bill, so the payables screen shows a row
        /// that is present but excluded from the batch total.
        post: bool,
    },
}

/// Three months of trading: capital, sales on credit, some of it collected,
/// supplier bills and the monthly running costs.
async fn seed_ledger(
    registry: &Registry,
    ctx: &OpCtx,
    today: NaiveDate,
    calls: &mut usize,
) -> anyhow::Result<()> {
    let mut steps: Vec<(NaiveDate, Step)> = vec![(back(today, 3, 1), Step::Opening)];

    let invoices = [
        ("AR-2601", "明泰科技股份有限公司", "486000", 3, 5, 30, true),
        ("AR-2602", "首鴻實業有限公司", "212500", 3, 18, 30, true),
        ("AR-2603", "合眾電子股份有限公司", "358000", 2, 8, 45, true),
        ("AR-2604", "大川五金行", "94200", 2, 22, 30, false),
        ("AR-2605", "宥昇國際貿易", "627400", 1, 6, 60, false),
        ("AR-2606", "明泰科技股份有限公司", "173800", 1, 24, 30, false),
    ];
    for (no, customer, amount, months, day, terms, collected) in invoices {
        let issued = back(today, months, day);
        let due = plus_days(issued, terms);
        steps.push((issued, Step::ArIssue { no, customer, amount, due }));
        if collected {
            steps.push((plus_days(due, 2), Step::ArReceipt { no }));
        }
    }

    let bills = [
        ("AP-2601", "聯欣物流有限公司", "68400", "6131", 3, 10, 30, true),
        ("AP-2602", "永信文具事務用品", "12750", "6199", 2, 14, 30, true),
        ("AP-2603", "宏碁資訊服務", "145000", "6199", 2, 26, 45, true),
        ("AP-2604", "同茂貿易股份有限公司", "392000", "5111", 1, 9, 30, true),
        ("AP-2605", "台灣大哥大", "8360", "6131", 1, 20, 15, false),
    ];
    for (no, vendor, amount, account, months, day, terms, post) in bills {
        let billed = back(today, months, day);
        let due = plus_days(billed, terms);
        steps.push((billed, Step::ApBill { no, vendor, amount, account, due, post }));
    }

    for months in 1..=3 {
        for (account, amount, memo) in [
            ("6115", "45000", "辦公室租金"),
            ("6118", "6800", "水電費"),
            ("6142", "28000", "網路廣告"),
        ] {
            steps.push((back(today, months, 5), Step::Cost { account, amount, memo }));
        }
    }

    steps.sort_by_key(|(date, _)| *date);

    for (date, step) in steps {
        match step {
            Step::Opening => {
                let out = call(
                    registry,
                    ctx,
                    "create_voucher",
                    json!({
                        "date": date,
                        "memo": "股東出資",
                        "lines": [
                            { "account_code": "1102", "debit": "3000000", "credit": "0" },
                            { "account_code": "3101", "debit": "0", "credit": "3000000" },
                        ],
                    }),
                    calls,
                )
                .await?;
                call(registry, ctx, "post_voucher", json!({ "no": voucher_no(&out)? }), calls).await?;
            }
            Step::Cost { account, amount, memo } => {
                let out = call(
                    registry,
                    ctx,
                    "create_voucher",
                    json!({
                        "date": date,
                        "memo": memo,
                        "lines": [
                            { "account_code": account, "debit": amount, "credit": "0" },
                            { "account_code": "1102", "debit": "0", "credit": amount },
                        ],
                    }),
                    calls,
                )
                .await?;
                call(registry, ctx, "post_voucher", json!({ "no": voucher_no(&out)? }), calls).await?;
            }
            Step::ArIssue { no, customer, amount, due } => {
                let out = call(
                    registry,
                    ctx,
                    "create_ar_invoice",
                    json!({
                        "invoice_no": no,
                        "customer": customer,
                        "amount": amount,
                        "issued_on": date,
                        "due_on": due,
                        "revenue_account_code": "4111",
                        "memo": "貨款",
                    }),
                    calls,
                )
                .await?;
                call(registry, ctx, "post_voucher", json!({ "no": voucher_no(&out)? }), calls).await?;
            }
            Step::ArReceipt { no } => {
                let out = call(
                    registry,
                    ctx,
                    "record_receipt",
                    json!({ "invoice_no": no, "received_on": date }),
                    calls,
                )
                .await?;
                call(registry, ctx, "post_voucher", json!({ "no": voucher_no(&out)? }), calls).await?;
            }
            Step::ApBill { no, vendor, amount, account, due, post } => {
                let out = call(
                    registry,
                    ctx,
                    "create_ap_bill",
                    json!({
                        "bill_no": no,
                        "vendor": vendor,
                        "amount": amount,
                        "billed_on": date,
                        "due_on": due,
                        "debit_account_code": account,
                        "memo": "供應商請款",
                    }),
                    calls,
                )
                .await?;
                if post {
                    call(registry, ctx, "post_voucher", json!({ "no": voucher_no(&out)? }), calls)
                        .await?;
                }
            }
        }
    }
    Ok(())
}

/// Annual budget lines, set high enough that one account is close to its
/// limit and the alert threshold has something to act on.
async fn seed_budget(
    registry: &Registry,
    store: &Store,
    ctx: &OpCtx,
    today: NaiveDate,
    calls: &mut usize,
) -> anyhow::Result<()> {
    // The second write that is not an operation, for the same reason as the
    // e-invoice track: `set_budget` files a line against the current budget
    // version, but nothing creates a version, so a fresh installation is
    // refused with "尚無現行的預算版本" and has no way forward.
    sqlx::query(
        "INSERT INTO budget_version (code, label, fiscal_year, is_current, created_by)
         VALUES ($1, $2, $3, true, 'laysi-admin') ON CONFLICT (code) DO NOTHING",
    )
    .bind(format!("FY{}", today.year()))
    .bind(format!("{} 年度預算", today.year()))
    .bind(today.year())
    .execute(store.pool())
    .await
    .context("建立示範用的預算版本失敗")?;

    let lines = [
        ("6111", None, "4200000", "全年人事成本"),
        ("6115", None, "540000", "辦公室租金"),
        ("6118", None, "96000", "水電"),
        ("6142", Some("SALES"), "300000", "業務部行銷預算"),
        ("6131", None, "180000", "郵電與物流"),
        ("6199", None, "240000", "其他營業費用"),
    ];
    for (account, dept, amount, note) in lines {
        call(
            registry,
            ctx,
            "set_budget",
            json!({ "account_code": account, "dept_code": dept, "amount": amount, "note": note }),
            calls,
        )
        .await?;
    }
    Ok(())
}

/// Expense claims and leave requests.
///
/// Leave runs under each person's own context: `request_leave` takes its
/// subject from `ctx.employee_id`, and seeding it any other way would file
/// everyone's leave under one name.
///
/// Claims cannot. `submit_expense_claim` belongs to the `request_payment`
/// permission, which the `staff` role does not hold, so a staff account can
/// list its own claims but not raise one. They are therefore entered on the
/// employee's behalf with an explicit `claimant`, which is also how a paper
/// claim reaches the system today.
async fn seed_self_service(
    registry: &Registry,
    store: &Store,
    boss: &OpCtx,
    today: NaiveDate,
    calls: &mut usize,
) -> anyhow::Result<()> {
    let claims = [
        ("E002", "客戶拜訪計程車資", "1840", "6122", 18, true),
        ("E002", "招待客戶餐敘", "4600", "6199", 11, true),
        ("E003", "南部客戶出差高鐵", "3260", "6122", 6, false),
        ("E005", "開發工具年費", "7200", "6199", 3, false),
    ];
    for (emp_no, title, amount, account, days_ago, approve) in claims {
        let employee = store.employee(emp_no).await.context("找不到示範員工")?;
        let out = call(
            registry,
            boss,
            "submit_expense_claim",
            json!({
                "title": title,
                "amount": amount,
                "spent_on": plus_days(today, -days_ago),
                "dept_code": employee.dept_code,
                // Matches what list_my_expense_claims looks the claim up by,
                // so it appears on that person's own screen.
                "claimant": employee.name,
            }),
            calls,
        )
        .await?;
        if approve {
            let claim_no = out
                .get("claim")
                .and_then(|c| c.get("claim_no"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("報支單沒有回傳單號"))?
                .to_string();
            // Approved by the owner, not the claimant: the operation refuses
            // to let anyone sign their own claim.
            call(
                registry,
                boss,
                "approve_expense_claim",
                json!({ "claim_no": claim_no, "account_code": account }),
                calls,
            )
            .await?;
        }
    }

    seed_punches(registry, store, boss, today, calls).await?;
    seed_overtime(registry, store, boss, today, calls).await?;

    // A full day, an afternoon and a two-day absence, so the screens show
    // that the length of a request comes from its range and not from a
    // number somebody typed.
    let leaves = [
        ("E003", "annual", 12, 2, "09:00:00", "18:00:00", true),
        ("E002", "personal", 5, 1, "13:30:00", "18:00:00", true),
        ("E005", "sick", -3, 1, "09:00:00", "18:00:00", false),
    ];
    for (emp_no, kind, days_ago, span, from_time, to_time, decide) in leaves {
        let employee = store.employee(emp_no).await.context("找不到示範員工")?;
        let staff = ctx_for(store, &employee.name, "staff", Some(employee.id), today);
        // A weekend is worth no working hours, so a request landing on one
        // would be refused. Step back to a week that holds the whole span.
        let mut from = plus_days(today, -days_ago);
        while [from, plus_days(from, span - 1)]
            .iter()
            .any(|day| matches!(day.weekday(), Weekday::Sat | Weekday::Sun))
        {
            from = plus_days(from, -1);
        }
        let out = call(
            registry,
            &staff,
            "request_leave",
            json!({
                "kind": kind,
                "from_at": format!("{from}T{from_time}"),
                "to_at": format!("{}T{to_time}", plus_days(from, span - 1)),
                "reason": "已與主管口頭確認",
            }),
            calls,
        )
        .await?;
        if decide {
            let leave_no = out
                .get("request")
                .and_then(|r| r.get("leave_no"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("休假單沒有回傳單號"))?
                .to_string();
            call(
                registry,
                boss,
                "decide_leave_request",
                json!({ "leave_no": leave_no, "approve": true }),
                calls,
            )
            .await?;
        }
    }
    Ok(())
}

/// One approved overtime request in the period payroll will run for, and one
/// still waiting, so the approval screen and the payslip both have something
/// to show and the link between them is visible.
async fn seed_overtime(
    registry: &Registry,
    store: &Store,
    boss: &OpCtx,
    today: NaiveDate,
    calls: &mut usize,
) -> anyhow::Result<()> {
    // Evenings, because the demo company works 09:00 to 18:00 and overtime
    // that overlaps normal hours is refused.
    let requests = [
        ("E003", back(today, 1, 15), "18:30:00", "20:30:00", true),
        ("E002", plus_days(today, -2), "18:00:00", "20:00:00", false),
    ];
    for (emp_no, day, from_time, to_time, decide) in requests {
        let mut worked_on = day;
        while matches!(worked_on.weekday(), Weekday::Sat | Weekday::Sun) {
            worked_on = plus_days(worked_on, -1);
        }
        let employee = store.employee(emp_no).await.context("找不到示範員工")?;
        let staff = ctx_for(store, &employee.name, "staff", Some(employee.id), today);
        let out = call(
            registry,
            &staff,
            "submit_my_overtime_request",
            json!({
                "from_at": format!("{worked_on}T{from_time}"),
                "to_at": format!("{worked_on}T{to_time}"),
                "day_type": "weekday",
                "reason": "結帳趕件",
            }),
            calls,
        )
        .await?;
        if decide {
            let request_no = out
                .get("request")
                .and_then(|r| r.get("request_no"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("加班申請沒有回傳單號"))?
                .to_string();
            call(
                registry,
                boss,
                "decide_overtime_request",
                json!({ "request_no": request_no, "approve": true }),
                calls,
            )
            .await?;
        }
    }
    Ok(())
}

/// Two weeks of clock-in records, so the punch screens have a month to show.
///
/// The third write here that is not an operation, and for a sharper reason
/// than the other two. `punch_clock` deliberately takes no time: it records
/// the moment it is called, which is the whole point of a punch and the
/// reason an employee cannot backdate one. A demonstration needs history, and
/// the only honest way to produce it is to say plainly that these rows were
/// inserted rather than clocked. Two days are then corrected through
/// `correct_punch` so the difference between a punch and a correction is
/// visible on screen, which is exactly what the `source` column exists for.
async fn seed_punches(
    registry: &Registry,
    store: &Store,
    boss: &OpCtx,
    today: NaiveDate,
    calls: &mut usize,
) -> anyhow::Result<()> {
    // Arrival and departure per person, chosen so the screen shows an
    // on-time day, a late one and a day still open. Seconds are real values,
    // not :00, because that is what a clock produces and the screens show
    // them.
    let pattern: [(&str, &str, &str); 5] = [
        ("E001", "08:40:12", "19:10:41"),
        ("E002", "09:12:03", "18:20:55"),
        ("E003", "08:55:37", "18:05:09"),
        ("E004", "08:50:44", "17:58:26"),
        ("E005", "09:30:18", "20:15:02"),
    ];

    // The working days to fill, most recent first. Weekends are skipped
    // rather than given punches: this system has no holiday calendar, so the
    // demo leaves those days empty instead of inventing one. Every later step
    // indexes into this list, because "four days ago" lands on a Sunday
    // roughly two times in seven and there would be no arrival to correct.
    let days: Vec<NaiveDate> = (1..=14)
        .map(|back| plus_days(today, -back))
        .filter(|day| !matches!(day.weekday(), Weekday::Sat | Weekday::Sun))
        .collect();

    for (emp_no, arrive, leave) in pattern {
        let employee = store.employee(emp_no).await.context("找不到示範員工")?;
        for (index, day) in days.iter().copied().enumerate() {
            let punches: &[(&str, &str)] = if index == 0 {
                // On the latest day everyone forgot to clock out. It is the
                // most common real state and the one the screen must handle.
                &[("in", arrive)]
            } else {
                &[("in", arrive), ("out", leave)]
            };
            for (kind, at) in punches {
                let time = NaiveTime::parse_from_str(at, "%H:%M:%S")
                    .context("示範打卡時間格式錯誤")?;
                sqlx::query(
                    "INSERT INTO attendance_punch
                       (employee_id, punched_on, punched_at, kind, source, note, created_by)
                     VALUES ($1,$2,$3,$4,'self','',$5)
                     ON CONFLICT (employee_id, punched_on, kind) DO NOTHING",
                )
                .bind(employee.id)
                .bind(day)
                .bind(business_instant(day, time, laysi_ops::business_offset()))
                .bind(kind)
                .bind(&employee.name)
                .execute(store.pool())
                .await
                .context("寫入示範打卡失敗")?;
            }
        }
    }

    // Two corrections, through the operation, so they carry the operator and
    // are marked as corrections on screen. Both target working days by index
    // into the list above, so they always land on a day with an arrival.
    for (emp_no, index, kind, at) in [("E003", 0, "out", "18:02"), ("E005", 3, "out", "21:40")] {
        let Some(day) = days.get(index) else { continue };
        call(
            registry,
            boss,
            "correct_punch",
            json!({
                "emp_no": emp_no,
                "punched_on": day,
                "kind": kind,
                "at": at,
                "note": "忘記打卡，主管確認後補登",
            }),
            calls,
        )
        .await?;
    }
    Ok(())
}

/// Last month's payroll, posted, so the HR reconciliation against 6111 has
/// two sides to compare.
async fn seed_payroll(
    registry: &Registry,
    ctx: &OpCtx,
    today: NaiveDate,
    calls: &mut usize,
) -> anyhow::Result<()> {
    let last_month = back(today, 1, 1);
    let out = call(
        registry,
        ctx,
        "run_payroll",
        json!({
            "period": last_month.format("%Y-%m").to_string(),
            "pay_on": back(today, 0, 5),
            "note": "示範資料",
        }),
        calls,
    )
    .await?;
    let no = out
        .get("run")
        .and_then(|r| r.get("voucher_no"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("薪資計算沒有產生傳票"))?
        .to_string();
    call(registry, ctx, "post_voucher", json!({ "no": no }), calls).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_lands_in_the_intended_month() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 20).expect("有效日期");
        assert_eq!(back(today, 3, 5), NaiveDate::from_ymd_opt(2026, 5, 5).expect("有效日期"));
        assert_eq!(back(today, 0, 5), NaiveDate::from_ymd_opt(2026, 8, 5).expect("有效日期"));
    }

    /// Going back from a long month into a short one must not overflow into
    /// the next month; 31 January minus one month is February, not March.
    #[test]
    fn back_clamps_a_day_the_target_month_does_not_have() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 31).expect("有效日期");
        let landed = back(today, 1, 31);
        assert_eq!(landed.month(), 2, "2 月沒有 31 日，不可溢位到 3 月");
    }

    /// Checked against the RFC 4648 vectors, including both pad lengths.
    #[test]
    fn base64_matches_the_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64("單據".as_bytes()), "5Zau5pOa");
    }

    /// The roster is what several screens are counted against; a duplicate
    /// employee number would fail half way through seeding.
    #[test]
    fn roster_numbers_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for person in ROSTER {
            assert!(seen.insert(person.emp_no), "員工編號 {} 重複", person.emp_no);
        }
    }
}
