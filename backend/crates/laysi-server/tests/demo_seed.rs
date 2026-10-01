//! `laysi-admin seed-demo` on a fresh installation, end to end.
//!
//! The demo site is rebuilt by this command every night. A seed that stops
//! half way leaves the `demo` login missing and the shared logins unfrozen,
//! so the binary is run as the rebuild runs it, not `demo::seed()` alone.

use anyhow::{ensure, Context};
use chrono::NaiveDate;
use laysi_store::Store;
use sqlx::postgres::PgPoolOptions;

/// 2026-10-01: the personal leave five days back lands on Saturday the 26th,
/// and the Friday before it is 中秋節. Stepping back over weekends alone
/// files a request with no working hours in it, which `request_leave`
/// refuses, and the seed stopped there.
///
/// Run with:
/// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
/// seed_demo_steps_over_a_holiday_next_to_a_weekend -- --ignored`
#[tokio::test]
#[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
async fn seed_demo_steps_over_a_holiday_next_to_a_weekend() {
    let url = std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("應可連上測試資料庫");
    let name = format!("laysi_demo_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("應可建立拋棄式資料庫");

    // Every step that can fail runs before the database is dropped, so a
    // failure leaves nothing behind on the server.
    let outcome = seed_and_check(&with_database(&url, &name)).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .expect("應可刪掉拋棄式資料庫");
    outcome.unwrap();
}

async fn seed_and_check(database_url: &str) -> anyhow::Result<()> {
    let holiday = NaiveDate::from_ymd_opt(2026, 9, 25).context("有效日期")?;
    let store = Store::connect(database_url, 2).await?;
    store.migrate().await?;
    // What the server's first boot imports from the published calendar.
    store
        .import_work_calendar(&[(holiday, "holiday".into(), "中秋節".into())], "")
        .await?;

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_laysi-admin"))
        .args(["seed-demo", "--password", "seed-demo-integration-test"])
        // No object storage, so the run cannot reach anything but this
        // database; the documents step skips itself.
        .env_clear()
        .env("DATABASE_URL", database_url)
        .env("LAYSI_TODAY", "2026-10-01")
        .output()
        .context("無法執行 laysi-admin")?;
    ensure!(
        out.status.success(),
        "seed-demo 失敗：{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let staff = store.employee("E003").await?;
    let demo = store
        .user_by_username("demo")
        .await?
        .context("seed-demo 跑完應該建出 demo 帳號")?;
    ensure!(demo.employee_id == Some(staff.id), "demo 帳號應連到 E003");

    let leaves: i64 = sqlx::query_scalar("SELECT count(*) FROM leave_request")
        .fetch_one(store.pool())
        .await?;
    ensure!(leaves == 3, "應種入三張休假單，實際 {leaves} 張");

    // Two working weeks, the holiday among them made up for further back.
    let punch_days: i64 =
        sqlx::query_scalar("SELECT count(DISTINCT punched_on) FROM attendance_punch")
            .fetch_one(store.pool())
            .await?;
    ensure!(
        punch_days == 10,
        "應有 10 個工作日的打卡，實際 {punch_days} 天"
    );

    let on_holiday: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM leave_request
                  WHERE $1 BETWEEN from_at::date AND to_at::date)
              + (SELECT count(*) FROM overtime_request WHERE from_at::date = $1)
              + (SELECT count(*) FROM attendance_punch WHERE punched_on = $1)",
    )
    .bind(holiday)
    .fetch_one(store.pool())
    .await?;
    ensure!(
        on_holiday == 0,
        "{holiday} 是國定假日，不應有示範休假、加班或打卡，實際 {on_holiday} 筆"
    );
    Ok(())
}

/// `url` with its database replaced by `name`, keeping any query string.
fn with_database(url: &str, name: &str) -> String {
    let (head, query) = url
        .split_once('?')
        .map_or((url, None), |(head, query)| (head, Some(query)));
    let authority = head.find("://").map_or(0, |i| i + 3);
    let host = head[authority..]
        .rfind('@')
        .map_or(authority, |i| authority + i + 1);
    let base = head[host..].find('/').map_or(head, |i| &head[..host + i]);
    match query {
        Some(query) => format!("{base}/{name}?{query}"),
        None => format!("{base}/{name}"),
    }
}

#[test]
fn with_database_replaces_only_the_database() {
    assert_eq!(
        with_database("postgres://laysi:ci@localhost:5432/laysi_ci", "x"),
        "postgres://laysi:ci@localhost:5432/x"
    );
    assert_eq!(
        with_database("postgres://u:p@h/db?sslmode=disable", "x"),
        "postgres://u:p@h/x?sslmode=disable"
    );
    assert_eq!(
        with_database("postgres://h:5432", "x"),
        "postgres://h:5432/x"
    );
}
