//! Withdrawing and revoking leave and overtime requests, through the registry.
//!
//! A database of its own: a revocation reads every computed payroll in the
//! installation, and a shared database holds other tests' payroll runs.

use anyhow::{bail, ensure, Context};
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_ops::{Channel, OpCtx, OpError, Registry, ROLE_ADMIN, ROLE_BOSS, ROLE_STAFF};
use laysi_store::Store;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;

mod common;
use common::with_database;

/// Run with:
/// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
/// requests_are_taken_back_until_payroll_reads_them -- --ignored`
#[tokio::test]
#[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
async fn requests_are_taken_back_until_payroll_reads_them() {
    let url = std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("應可連上測試資料庫");
    let name = format!("laysi_take_back_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("應可建立拋棄式資料庫");

    let outcome = take_back(&with_database(&url, &name)).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .expect("應可刪掉拋棄式資料庫");
    outcome.unwrap();
}

fn ctx(store: &Store, actor: &str, role: &str, employee_id: Option<i32>) -> OpCtx {
    OpCtx {
        store: store.clone(),
        objects: None,
        actor: actor.into(),
        actor_role: role.into(),
        username: None,
        employee_id,
        user_id: None,
        token_id: None,
        public_base: None,
        prompt: None,
        channel: Channel::Ui,
        client_ip: None,
        today: NaiveDate::from_ymd_opt(2026, 10, 20).expect("有效日期"),
        offset: laysi_ops::business_offset(),
    }
}

struct Run<'a> {
    registry: &'a Registry,
}

impl Run<'_> {
    async fn ok(&self, ctx: &OpCtx, op: &str, input: Value) -> anyhow::Result<Value> {
        self.registry
            .dispatch(ctx, op, input)
            .await
            .map_err(|e| anyhow::anyhow!("{op} 應成功，實際為：{e}"))
    }

    async fn refused(&self, ctx: &OpCtx, op: &str, input: Value) -> anyhow::Result<String> {
        match self.registry.dispatch(ctx, op, input).await {
            Ok(_) => bail!("{op} 應被拒絕"),
            Err(e @ (OpError::BadInput(_) | OpError::Store(_))) => Ok(e.to_string()),
            Err(e) => bail!("{op} 應以單據的理由拒絕，實際為：{e}"),
        }
    }
}

fn no_of(data: &Value, key: &str) -> anyhow::Result<String> {
    data["request"][key]
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("回應沒有 request.{key}"))
}

fn listed_row<'a>(listed: &'a Value, key: &str, no: &str) -> anyhow::Result<&'a Value> {
    listed["requests"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r[key] == no))
        .with_context(|| format!("清單裡沒有 {no}"))
}

async fn payslip(store: &Store, period: &str, emp_no: &str) -> anyhow::Result<(Money, Money)> {
    let row: (Money, Money) = sqlx::query_as(
        "SELECT s.overtime, s.gross
           FROM payslip s
           JOIN payroll_run p ON p.id = s.run_id
           JOIN employee e    ON e.id = s.employee_id
          WHERE p.period = $1 AND e.emp_no = $2",
    )
    .bind(period)
    .bind(emp_no)
    .fetch_one(store.pool())
    .await?;
    Ok(row)
}

async fn take_back(database_url: &str) -> anyhow::Result<()> {
    // Room for a held revocation, a waiting payroll run and the polling.
    let store = Store::connect(database_url, 5).await?;
    store.migrate().await?;
    store.reload_regulations().await?;
    let registry = Registry::new();
    let run = Run {
        registry: &registry,
    };

    let admin = ctx(&store, "系統管理員", ROLE_ADMIN, None);
    let boss = ctx(&store, "負責人", ROLE_BOSS, None);
    for (code, value) in [("work_start", "09:00"), ("work_end", "18:00")] {
        run.ok(
            &admin,
            "update_company_setting",
            json!({ "code": code, "value": value }),
        )
        .await?;
    }
    run.ok(
        &boss,
        "create_bank_account",
        json!({ "name": "薪轉戶", "account_no": "000-111", "gl_code": "1102" }),
    )
    .await?;

    let mut people = Vec::new();
    for name in ["撤回測試", "另一位"] {
        let made = run
            .ok(
                &boss,
                "create_employee",
                json!({
                    "name": name,
                    "hired_on": "2026-01-01",
                    "base_salary": "36000",
                    "labor_insured_salary": "36300",
                    "health_insured_salary": "36300",
                    "pension_wage": "36300",
                }),
            )
            .await?;
        let emp_no = made["employee"]["emp_no"]
            .as_str()
            .context("回應沒有員工編號")?
            .to_string();
        let id = store.employee(&emp_no).await?.id;
        people.push((emp_no, id, name));
    }
    let (emp_no, id, name) = people[0].clone();
    let me = ctx(&store, name, ROLE_STAFF, Some(id));
    let other = ctx(&store, people[1].2, ROLE_STAFF, Some(people[1].1));
    let day = |date: &str| {
        json!({
            "kind": "personal",
            "from_at": format!("{date}T09:00:00"),
            "to_at": format!("{date}T18:00:00"),
        })
    };

    // Withdrawn while pending, the same hours go out again.
    let first = no_of(
        &run.ok(&me, "request_leave", day("2026-10-14")).await?,
        "leave_no",
    )?;
    let theirs = run
        .refused(
            &other,
            "withdraw_my_leave_request",
            json!({ "leave_no": first }),
        )
        .await?;
    ensure!(
        theirs == format!("查無休假單 {first}"),
        "撤回別人的單應回查無此單，實際：{theirs}"
    );
    let withdrawn = run
        .ok(
            &me,
            "withdraw_my_leave_request",
            json!({ "leave_no": first, "reason": "改期" }),
        )
        .await?;
    ensure!(withdrawn["request"]["status"] == "withdrawn");
    ensure!(withdrawn["request"]["cancelled"]["reason"] == "改期");
    let again = no_of(
        &run.ok(&me, "request_leave", day("2026-10-14")).await?,
        "leave_no",
    )?;

    // Approved, it is HR's to revoke, and only with a reason.
    run.ok(
        &boss,
        "decide_leave_request",
        json!({ "leave_no": again, "approve": true }),
    )
    .await?;
    let approved = run
        .refused(
            &me,
            "withdraw_my_leave_request",
            json!({ "leave_no": again }),
        )
        .await?;
    ensure!(approved.contains("請洽人資撤銷"), "{approved}");
    let reasonless = run
        .refused(
            &boss,
            "revoke_leave_request",
            json!({ "leave_no": again, "reason": "  " }),
        )
        .await?;
    ensure!(reasonless == "請填寫撤銷理由", "{reasonless}");
    let as_hr_on_own = ctx(&store, name, "hr", Some(id));
    let own = run
        .refused(
            &as_hr_on_own,
            "revoke_leave_request",
            json!({ "leave_no": again, "reason": "本人撤銷" }),
        )
        .await?;
    ensure!(own.contains("由本人送出"), "{own}");

    // September: approved overtime is paid and posted, and then no longer
    // revocable at all.
    let september = no_of(
        &run.ok(
            &me,
            "submit_my_overtime_request",
            json!({ "from_at": "2026-09-16T18:00:00", "to_at": "2026-09-16T20:00:00" }),
        )
        .await?,
        "request_no",
    )?;
    run.ok(
        &boss,
        "decide_overtime_request",
        json!({ "request_no": september, "approve": true }),
    )
    .await?;
    run.ok(
        &boss,
        "run_payroll",
        json!({ "period": "2026-09", "pay_on": "2026-10-05" }),
    )
    .await?;
    let voucher: String =
        sqlx::query_scalar("SELECT voucher_no FROM payroll_run WHERE period = '2026-09'")
            .fetch_one(store.pool())
            .await?;
    run.ok(&boss, "post_voucher", json!({ "no": voucher }))
        .await?;
    let posted = run
        .refused(
            &boss,
            "revoke_overtime_request",
            json!({ "request_no": september, "reason": "沒有加班" }),
        )
        .await?;
    ensure!(
        posted == "2026-09 的薪資已過帳，無法撤銷；差額請在下一期補發或補扣",
        "{posted}"
    );

    // October: a day of unpaid leave and two hours of overtime are both in
    // the payslip, and revoking either waits for the period to be voided.
    let october = no_of(
        &run.ok(
            &me,
            "submit_my_overtime_request",
            json!({ "from_at": "2026-10-13T18:00:00", "to_at": "2026-10-13T20:00:00" }),
        )
        .await?,
        "request_no",
    )?;
    run.ok(
        &boss,
        "decide_overtime_request",
        json!({ "request_no": october, "approve": true }),
    )
    .await?;
    run.ok(
        &boss,
        "run_payroll",
        json!({ "period": "2026-10", "pay_on": "2026-10-20" }),
    )
    .await?;
    let (overtime, gross) = payslip(&store, "2026-10", &emp_no).await?;
    ensure!(overtime > Money::ZERO, "核准的加班應計入加班費");
    ensure!(
        gross < Money::from(36000) + overtime,
        "核准的事假應扣工資，實際應發 {gross}"
    );

    let blocked = "2026-10 已計算薪資，撤銷前請先到「薪資作業」作廢";
    let refused = run
        .refused(
            &boss,
            "revoke_leave_request",
            json!({ "leave_no": again, "reason": "核准錯誤" }),
        )
        .await?;
    ensure!(refused == blocked, "{refused}");
    let refused = run
        .refused(
            &boss,
            "revoke_overtime_request",
            json!({ "request_no": october, "reason": "沒有加班" }),
        )
        .await?;
    ensure!(refused == blocked, "{refused}");
    let listed = run
        .ok(
            &boss,
            "list_leave_requests",
            json!({ "month": "2026-10", "include_done": true }),
        )
        .await?;
    let row = listed["requests"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["leave_no"] == again.as_str()))
        .context("清單裡沒有那張休假單")?;
    ensure!(row["can_revoke"] == false);
    ensure!(row["revoke_blocked_reason"] == blocked, "{row}");

    // Approving into a computed period is refused the same way; rejecting
    // changes nothing payroll read and stays open.
    let late = no_of(
        &run.ok(&me, "request_leave", day("2026-10-16")).await?,
        "leave_no",
    )?;
    let refused = run
        .refused(
            &boss,
            "decide_leave_request",
            json!({ "leave_no": late, "approve": true }),
        )
        .await?;
    ensure!(
        refused == "2026-10 已計算薪資，核准前請先到「薪資作業」作廢",
        "{refused}"
    );
    let listed = run
        .ok(
            &boss,
            "list_leave_requests",
            json!({ "month": "2026-10", "include_done": true }),
        )
        .await?;
    let row = listed["requests"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["leave_no"] == late.as_str()))
        .context("清單裡沒有那張休假單")?;
    ensure!(
        row["can_decide"] == true && row["can_approve"] == false,
        "{row}"
    );
    ensure!(row["approve_blocked_reason"] == refused.as_str(), "{row}");
    let rejected = run
        .ok(
            &boss,
            "decide_leave_request",
            json!({ "leave_no": late, "approve": false }),
        )
        .await?;
    ensure!(rejected["request"]["status"] == "rejected");
    let twice = run
        .refused(
            &boss,
            "decide_leave_request",
            json!({ "leave_no": late, "approve": false }),
        )
        .await?;
    ensure!(
        twice == format!("休假單 {late} 已為「已駁回」，只有等簽核的單可以簽核"),
        "{twice}"
    );
    let late_overtime = no_of(
        &run.ok(
            &me,
            "submit_my_overtime_request",
            json!({ "from_at": "2026-10-15T18:00:00", "to_at": "2026-10-15T20:00:00" }),
        )
        .await?,
        "request_no",
    )?;
    let refused = run
        .refused(
            &boss,
            "decide_overtime_request",
            json!({ "request_no": late_overtime, "approve": true }),
        )
        .await?;
    ensure!(
        refused == "2026-10 已計算薪資，核准前請先到「薪資作業」作廢",
        "{refused}"
    );
    run.ok(
        &boss,
        "decide_overtime_request",
        json!({ "request_no": late_overtime, "approve": false }),
    )
    .await?;

    run.ok(
        &boss,
        "void_payroll_run",
        json!({ "period": "2026-10", "reason": "撤銷休假與加班" }),
    )
    .await?;
    let revoked = run
        .ok(
            &boss,
            "revoke_leave_request",
            json!({ "leave_no": again, "reason": "核准錯誤" }),
        )
        .await?;
    ensure!(revoked["request"]["status"] == "revoked");
    ensure!(
        revoked["request"]["decided_by"] == "負責人",
        "原本的核准要留著"
    );
    ensure!(revoked["request"]["cancelled"]["by"] == "負責人");
    run.ok(
        &boss,
        "revoke_overtime_request",
        json!({ "request_no": october, "reason": "沒有加班" }),
    )
    .await?;

    run.ok(
        &boss,
        "run_payroll",
        json!({ "period": "2026-10", "pay_on": "2026-10-20" }),
    )
    .await?;
    let (overtime, gross) = payslip(&store, "2026-10", &emp_no).await?;
    ensure!(overtime == Money::ZERO, "撤銷的加班仍計入 {overtime}");
    ensure!(
        gross == Money::from(36000),
        "撤銷的事假仍扣工資，應發 {gross}"
    );

    // A revoked request gives its hours back as well.
    run.ok(&me, "request_leave", day("2026-10-14")).await?;

    // On the roster only after September and October were computed: neither
    // period paid this person, so their requests in both are approved and
    // the next run of each pays them, posted September included. Once a
    // payslip of theirs exists, the rule holds for them as well.
    let joined = run
        .ok(
            &boss,
            "create_employee",
            json!({
                "name": "後到職",
                "hired_on": "2026-09-21",
                "base_salary": "40000",
                "labor_insured_salary": "40100",
                "health_insured_salary": "40100",
                "pension_wage": "40100",
            }),
        )
        .await?;
    let late_no = joined["employee"]["emp_no"]
        .as_str()
        .context("回應沒有員工編號")?
        .to_string();
    let late_id = store.employee(&late_no).await?.id;
    let newcomer = ctx(&store, "後到職", ROLE_STAFF, Some(late_id));
    let evening = |date: &str| {
        json!({
            "from_at": format!("{date}T19:00:00"),
            "to_at": format!("{date}T21:00:00"),
        })
    };
    let in_september = no_of(
        &run.ok(
            &newcomer,
            "submit_my_overtime_request",
            evening("2026-09-24"),
        )
        .await?,
        "request_no",
    )?;
    let in_october = no_of(
        &run.ok(
            &newcomer,
            "submit_my_overtime_request",
            evening("2026-10-14"),
        )
        .await?,
        "request_no",
    )?;
    let leave_in_october = no_of(
        &run.ok(&newcomer, "request_leave", day("2026-10-15"))
            .await?,
        "leave_no",
    )?;

    let listed = run
        .ok(
            &boss,
            "list_overtime_requests",
            json!({ "month": "2026-10", "include_done": true }),
        )
        .await?;
    let row = listed_row(&listed, "request_no", &in_october)?;
    ensure!(
        row["can_approve"] == true && row["approve_blocked_reason"].is_null(),
        "{row}"
    );
    let listed = run
        .ok(
            &boss,
            "list_leave_requests",
            json!({ "month": "2026-10", "include_done": true }),
        )
        .await?;
    let row = listed_row(&listed, "leave_no", &leave_in_october)?;
    ensure!(
        row["can_approve"] == true && row["approve_blocked_reason"].is_null(),
        "{row}"
    );
    for no in [&in_september, &in_october] {
        run.ok(
            &boss,
            "decide_overtime_request",
            json!({ "request_no": no, "approve": true }),
        )
        .await?;
    }
    run.ok(
        &boss,
        "decide_leave_request",
        json!({ "leave_no": leave_in_october, "approve": true }),
    )
    .await?;

    run.ok(
        &boss,
        "run_payroll",
        json!({ "period": "2026-09", "pay_on": "2026-10-05" }),
    )
    .await?;
    let (overtime, _) = payslip(&store, "2026-09", &late_no).await?;
    ensure!(overtime > Money::ZERO, "已過帳的期別補算時應計入核准的加班");
    run.ok(
        &boss,
        "run_payroll",
        json!({ "period": "2026-10", "pay_on": "2026-10-20" }),
    )
    .await?;
    let (overtime, _) = payslip(&store, "2026-10", &late_no).await?;
    ensure!(overtime > Money::ZERO, "補算時應計入核准的加班");
    let note: String = sqlx::query_scalar(
        "SELECT s.note
           FROM payslip s
           JOIN payroll_run p ON p.id = s.run_id
          WHERE p.period = '2026-10' AND s.employee_id = $1",
    )
    .bind(late_id)
    .fetch_one(store.pool())
    .await?;
    ensure!(
        note.contains("事假 1 日扣"),
        "補算時應扣核准的事假，備註：{note}"
    );

    let refused = run
        .refused(
            &boss,
            "revoke_overtime_request",
            json!({ "request_no": in_october, "reason": "沒有加班" }),
        )
        .await?;
    ensure!(
        refused == "2026-10 已計算薪資，撤銷前請先到「薪資作業」作廢",
        "{refused}"
    );
    let listed = run
        .ok(
            &boss,
            "list_overtime_requests",
            json!({ "month": "2026-10", "include_done": true }),
        )
        .await?;
    let row = listed_row(&listed, "request_no", &in_october)?;
    ensure!(
        row["can_revoke"] == false && row["revoke_blocked_reason"] == refused.as_str(),
        "{row}"
    );
    let refused = run
        .refused(
            &boss,
            "revoke_overtime_request",
            json!({ "request_no": in_september, "reason": "沒有加班" }),
        )
        .await?;
    ensure!(
        refused == "2026-09 的薪資已過帳，無法撤銷；差額請在下一期補發或補扣",
        "{refused}"
    );

    // A revocation, and an approval, that commit while payroll is between
    // reading and writing stop the run rather than being paid around. Each is
    // held open on a connection of its own, holding the lock the real ones
    // take, until the run is waiting for it.
    let november = no_of(
        &run.ok(
            &me,
            "submit_my_overtime_request",
            json!({ "from_at": "2026-11-10T18:00:00", "to_at": "2026-11-10T20:00:00" }),
        )
        .await?,
        "request_no",
    )?;
    run.ok(
        &boss,
        "decide_overtime_request",
        json!({ "request_no": november, "approve": true }),
    )
    .await?;
    race(
        &store,
        "UPDATE overtime_request
            SET status = 'revoked', cancelled_by = '測試', cancelled_role = 'hr',
                cancelled_at = now(), cancel_reason = '計薪途中撤銷'
          WHERE request_no = $1",
        &november,
        "2026-11",
    )
    .await?;
    let (overtime, _) = payslip(&store, "2026-11", &emp_no).await?;
    ensure!(overtime == Money::ZERO, "重新計算後不應計入撤銷的加班");

    let december = no_of(
        &run.ok(
            &me,
            "submit_my_overtime_request",
            json!({ "from_at": "2026-12-08T18:00:00", "to_at": "2026-12-08T20:00:00" }),
        )
        .await?,
        "request_no",
    )?;
    race(
        &store,
        "UPDATE overtime_request
            SET status = 'approved', decided_by = '測試', decided_role = 'hr',
                decided_at = now()
          WHERE request_no = $1",
        &december,
        "2026-12",
    )
    .await?;
    let (overtime, _) = payslip(&store, "2026-12", &emp_no).await?;
    ensure!(overtime > Money::ZERO, "重新計算後應計入核准的加班");

    // Two people deciding one request at once: the second waits for the
    // first and is told what it decided, in words.
    let contested = no_of(
        &run.ok(&me, "request_leave", day("2026-12-16")).await?,
        "leave_no",
    )?;
    let mut first = store.pool().begin().await?;
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first)
        .await?;
    sqlx::query(
        "UPDATE leave_request
            SET status = 'rejected', decided_by = '測試', decided_role = 'hr',
                decided_at = now()
          WHERE leave_no = $1",
    )
    .bind(&contested)
    .execute(&mut *first)
    .await?;
    let second = ctx(&store, "負責人", ROLE_BOSS, None);
    let deciding = tokio::spawn({
        let no = contested.clone();
        async move {
            Registry::new()
                .dispatch(
                    &second,
                    "decide_leave_request",
                    json!({ "leave_no": no, "approve": false }),
                )
                .await
        }
    });
    ensure!(blocked_by(&store, holder).await?, "第二位沒有等第一位提交");
    first.commit().await?;
    match deciding.await? {
        Err(OpError::Store(e)) => ensure!(
            e.to_string() == format!("休假單 {contested} 已為「已駁回」，只有等簽核的單可以簽核"),
            "{e}"
        ),
        other => bail!("第二位應被告知單據已駁回，實際為 {other:?}"),
    }
    Ok(())
}

/// Hold `change` to request `no` open under the payroll lock, start a payroll
/// run for `period`, let the change commit once the run waits, and expect the
/// run to refuse and write nothing. Then compute the period again.
async fn race(store: &Store, change: &str, no: &str, period: &str) -> anyhow::Result<()> {
    let mut held = store.pool().begin().await?;
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(laysi_store::PAYROLL_INPUT_LOCK_KEY)
        .execute(&mut *held)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(change.to_string()))
        .bind(no)
        .execute(&mut *held)
        .await?;

    let payer = ctx(store, "負責人", ROLE_BOSS, None);
    let input = json!({ "period": period, "pay_on": "2026-10-20" });
    let paying = tokio::spawn({
        let input = input.clone();
        async move { Registry::new().dispatch(&payer, "run_payroll", input).await }
    });
    let waiting = blocked_by(store, holder).await?;
    held.commit().await?;
    ensure!(waiting, "計薪沒有等在計薪鎖上");
    match paying.await? {
        Err(OpError::Store(e)) => ensure!(
            e.to_string() == format!("{no} 在計算期間被核准或撤銷，請重新計算本期薪資"),
            "{e}"
        ),
        other => bail!("計薪途中改動的單應讓計薪失敗，實際為 {other:?}"),
    }
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM payroll_run WHERE period = $1")
        .bind(period)
        .fetch_one(store.pool())
        .await?;
    ensure!(runs == 0, "失敗的計薪不應留下紀錄");
    Registry::new()
        .dispatch(&ctx(store, "負責人", ROLE_BOSS, None), "run_payroll", input)
        .await
        .map_err(|e| anyhow::anyhow!("重新計算應成功，實際為：{e}"))?;
    Ok(())
}

/// Whether a session comes to wait on a lock held by backend `holder`, polled
/// for up to ten seconds. Naming the holder keeps an unrelated wait elsewhere
/// in the database from releasing the held transaction early.
async fn blocked_by(store: &Store, holder: i32) -> anyhow::Result<bool> {
    for _ in 0..200 {
        let blocked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
              WHERE $1 = ANY(pg_blocking_pids(pid))",
        )
        .bind(holder)
        .fetch_one(store.pool())
        .await?;
        if blocked > 0 {
            return Ok(true);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(false)
}
