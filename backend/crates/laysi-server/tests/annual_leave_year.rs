//! Annual leave balances across an anniversary, through the registry.
//!
//! Four operations print a balance: the leave list, the roster, the
//! employee's own home and leave screens, and the compliance check. Each is
//! asked about the same people on the same day and must give the same answer,
//! and that answer must survive the working day being changed afterwards.

use anyhow::{ensure, Context};
use chrono::NaiveDate;
use laysi_ops::{Channel, OpCtx, Registry, ROLE_ADMIN, ROLE_BOSS, ROLE_STAFF};
use laysi_store::Store;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

mod common;
use common::with_database;

/// Run with:
/// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
/// annual_leave_is_counted_in_the_year_it_belongs_to -- --ignored`
#[tokio::test]
#[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
async fn annual_leave_is_counted_in_the_year_it_belongs_to() {
    let url = std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("應可連上測試資料庫");
    let name = format!("laysi_annual_year_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("應可建立拋棄式資料庫");

    let outcome = annual_years(&with_database(&url, &name)).await;
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
        today: NaiveDate::from_ymd_opt(2026, 10, 6).expect("有效日期"),
        offset: laysi_ops::business_offset(),
    }
}

/// `(entitlement, taken, remaining, from_on)` with the decimals trimmed.
fn balance(entitlement: &Value, taken: &Value, remaining: &Value, from: &Value) -> String {
    let trim = |v: &Value| {
        let s = v.as_str().map_or_else(|| v.to_string(), str::to_string);
        match s.contains('.') {
            true => s.trim_end_matches('0').trim_end_matches('.').to_string(),
            false => s,
        }
    };
    format!(
        "額度 {} 已休 {} 剩 {} · {} 起",
        trim(entitlement),
        trim(taken),
        trim(remaining),
        trim(from)
    )
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
}

fn object(v: &Value) -> String {
    balance(
        &v["entitlement"],
        &v["taken"],
        &v["remaining"],
        &v["from_on"],
    )
}

async fn annual_years(database_url: &str) -> anyhow::Result<()> {
    let store = Store::connect(database_url, 2).await?;
    store.migrate().await?;
    store.reload_regulations().await?;
    let registry = Registry::new();
    let run = Run {
        registry: &registry,
    };

    let admin = ctx(&store, "系統管理員", ROLE_ADMIN, None);
    let boss = ctx(&store, "負責人", ROLE_BOSS, None);
    for (code, value) in [
        ("work_start", "09:00"),
        ("work_end", "18:00"),
        ("break_start", "12:00"),
        ("break_end", "13:00"),
    ] {
        run.ok(
            &admin,
            "update_company_setting",
            json!({ "code": code, "value": value }),
        )
        .await?;
    }

    // Past the second anniversary on 2026-09-02, half a year in on the same
    // day, and long enough in that a two-day request fails nothing.
    let mut people = Vec::new();
    for (name, hired_on) in [
        ("週年測試", "2024-09-02"),
        ("半年測試", "2026-03-02"),
        ("補班測試", "2020-01-01"),
    ] {
        let made = run
            .ok(
                &boss,
                "create_employee",
                json!({ "name": name, "hired_on": hired_on }),
            )
            .await?;
        let emp_no = made["employee"]["emp_no"]
            .as_str()
            .context("回應沒有員工編號")?
            .to_string();
        let id = store.employee(&emp_no).await?.id;
        people.push((emp_no, id, name));
    }

    // A day off inside one request, so the split it is sent with has a gap,
    // and a Saturday made up as a working day inside another, which the
    // government calendar files as a working day.
    for (on_date, kind, name) in [
        ("2026-10-01", "holiday", "測試假日"),
        ("2026-09-12", "workday", "補行上班"),
    ] {
        run.ok(
            &boss,
            "set_work_calendar_day",
            json!({ "on_date": on_date, "kind": kind, "name": name }),
        )
        .await?;
    }

    let leave = |from: &str, to: &str| {
        json!({
            "kind": "annual",
            "from_at": format!("{from}T09:00:00"),
            "to_at": format!("{to}T18:00:00"),
        })
    };
    // The first person's are three days before the anniversary, one day on
    // each side of it, and three working days after it.
    let ranges = [
        (0, "2026-07-27", "2026-07-29"),
        (0, "2026-09-01", "2026-09-02"),
        (0, "2026-09-29", "2026-10-02"),
        (1, "2026-08-03", "2026-08-03"),
        (1, "2026-09-07", "2026-09-10"),
        (2, "2026-09-11", "2026-09-12"),
    ];
    for (who, from, to) in ranges {
        let (_, id, name) = &people[who];
        let me = ctx(&store, name, ROLE_STAFF, Some(*id));
        let sent = run.ok(&me, "request_leave", leave(from, to)).await?;
        let no = sent["request"]["leave_no"]
            .as_str()
            .context("回應沒有休假單號")?
            .to_string();
        run.ok(
            &boss,
            "decide_leave_request",
            json!({ "leave_no": no, "approve": true }),
        )
        .await?;
    }

    let expected = [
        "額度 10 已休 4 剩 6 · 2026-09-02 起",
        "額度 3 已休 4 剩 -1 · 2026-09-02 起",
    ];

    let list = run
        .ok(
            &boss,
            "list_leave_requests",
            json!({ "include_done": true }),
        )
        .await?;
    let roster = run.ok(&boss, "list_employees", json!({})).await?;
    let compliance = run.ok(&boss, "get_hr_compliance", json!({})).await?;
    let annual_check = compliance["checks"]
        .as_array()
        .context("法遵回應沒有 checks")?
        .iter()
        .find(|c| c["name"] == "特休額度與已核准天數")
        .context("法遵回應沒有特休檢核")?;

    for ((emp_no, id, name), want) in people.iter().zip(expected) {
        let listed = list["annual_leave_balances"]
            .as_array()
            .context("沒有 annual_leave_balances")?
            .iter()
            .find(|b| b["emp_no"] == emp_no.as_str())
            .context("休假頁沒有這個人的特休餘額")?;
        ensure!(object(listed) == want, "休假頁 {name}：{}", object(listed));

        let row = roster["employees"]
            .as_array()
            .context("名冊沒有 employees")?
            .iter()
            .find(|e| e["emp_no"] == emp_no.as_str())
            .context("名冊沒有這個人")?;
        let on_roster = balance(
            &row["annual_leave_days"],
            &row["annual_leave_taken"],
            &row["annual_leave_left"],
            &row["annual_leave_from"],
        );
        ensure!(on_roster == want, "名冊 {name}：{on_roster}");

        let me = ctx(&store, name, ROLE_STAFF, Some(*id));
        let home = run.ok(&me, "get_my_home", json!({})).await?;
        ensure!(
            object(&home["annual_leave"]) == want,
            "個人專區 {name}：{}",
            object(&home["annual_leave"])
        );
        let mine = run.ok(&me, "list_my_leave_requests", json!({})).await?;
        ensure!(
            object(&mine["annual_leave"]) == want,
            "我的休假 {name}：{}",
            object(&mine["annual_leave"])
        );
    }

    // Only the one over its year's entitlement fails, and with that year's
    // numbers rather than the calendar year's five days.
    let failures = annual_check["failures"]
        .as_array()
        .context("特休檢核沒有 failures")?;
    ensure!(failures.len() == 1, "特休檢核應只有一筆：{failures:?}");
    let failure = &failures[0];
    ensure!(failure["emp_no"] == people[1].0.as_str());
    let in_check = balance(
        &failure["entitlement"],
        &failure["taken"],
        &json!(format!(
            "-{}",
            failure["difference"].as_str().unwrap_or("?")
        )),
        &failure["from_on"],
    );
    ensure!(in_check == expected[1], "法遵檢核：{in_check}");

    // A request is weighed against the year it starts in.
    let rows = list["requests"].as_array().context("沒有 requests")?;
    let july = rows
        .iter()
        .find(|r| r["emp_no"] == people[0].0.as_str() && r["from_on"] == "2026-07-27")
        .context("休假頁沒有七月那張單")?;
    ensure!(
        object(&july["annual_leave"]) == "額度 7 已休 4 剩 3 · 2025-09-02 起",
        "七月那張單：{}",
        object(&july["annual_leave"])
    );

    // Requests sent before the split was kept are backfilled by SQL, and it
    // has to give what the server records today.
    let split = |store: &Store| {
        let pool = store.pool().clone();
        async move {
            sqlx::query(
                "SELECT leave_request_id, on_date, seconds FROM leave_request_day
                  ORDER BY leave_request_id, on_date",
            )
            .fetch_all(&pool)
            .await
            .map(|rows| {
                rows.iter()
                    .map(|r| {
                        let on: NaiveDate = r.get("on_date");
                        let seconds: i32 = r.get("seconds");
                        (r.get::<i32, _>("leave_request_id"), on, seconds)
                    })
                    .collect::<Vec<_>>()
            })
        }
    };
    let recorded = split(&store).await?;
    ensure!(
        recorded.len() == 15,
        "應記下 15 個有上班時數的日期：{recorded:?}"
    );
    let make_up = NaiveDate::from_ymd_opt(2026, 9, 12).context("有效日期")?;
    ensure!(
        recorded.iter().any(|(_, on, _)| *on == make_up),
        "補行上班的星期六應有上班時數：{recorded:?}"
    );
    let migration = include_str!("../../../migrations/0024_leave_request_day.sql");
    let backfill = &migration[migration
        .find("WITH clock AS")
        .context("找不到補寫的敘述")?..];
    sqlx::query("DELETE FROM leave_request_day")
        .execute(store.pool())
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(backfill.to_string()))
        .execute(store.pool())
        .await?;
    ensure!(
        split(&store).await? == recorded,
        "補寫的結果與送出時記下的不同"
    );

    // Clearing the start of the working day leaves nothing to split by; the
    // recorded split still puts one day on each side of the anniversary.
    run.ok(
        &admin,
        "update_company_setting",
        json!({ "code": "work_start", "value": "" }),
    )
    .await?;
    let after = run
        .ok(
            &boss,
            "list_leave_requests",
            json!({ "include_done": true }),
        )
        .await?;
    let first = after["annual_leave_balances"]
        .as_array()
        .context("沒有 annual_leave_balances")?
        .iter()
        .find(|b| b["emp_no"] == people[0].0.as_str())
        .context("休假頁沒有這個人的特休餘額")?;
    ensure!(
        object(first) == expected[0],
        "清掉上班時間之後：{}",
        object(first)
    );

    // Without a start to the working day the backfill writes nothing, and a
    // request longer than a day does not turn into one date of 33 hours.
    sqlx::query("DELETE FROM leave_request_day")
        .execute(store.pool())
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(backfill.to_string()))
        .execute(store.pool())
        .await
        .context("上班時間空白時補寫不應失敗")?;
    let unsplit = split(&store).await?;
    ensure!(unsplit.is_empty(), "上班時間空白時不應補寫：{unsplit:?}");
    Ok(())
}
