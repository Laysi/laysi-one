//! The arrears wage payment fund of a period paid in two runs.
//!
//! The fund is applied once to the month's whole total, so a run paying
//! somebody late records the period's fund less what the earlier run
//! recorded, and the two add up to one computation over both payslips. An
//! earlier run from before the fund was recorded holds zero, and its payslips
//! still count towards the total.

use anyhow::{ensure, Context};
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_ops::{Channel, OpCtx, Registry, ROLE_BOSS};
use laysi_store::Store;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

mod common;
use common::with_database;

/// Run with:
/// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
/// a_late_run_completes_the_periods_arrears_fund -- --ignored`
#[tokio::test]
#[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
async fn a_late_run_completes_the_periods_arrears_fund() {
    with_scratch_database(|url| async move { pay_twice(&url, false).await }).await;
}

/// The first run of the period as an install computed it before upgrading:
/// no arrears recorded and no versions for the three new rules.
#[tokio::test]
#[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
async fn a_late_run_counts_payslips_from_before_the_fund_was_recorded() {
    with_scratch_database(|url| async move { pay_twice(&url, true).await }).await;
}

async fn with_scratch_database<F, Fut>(test: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let url = std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("應可連上測試資料庫");
    let name = format!("laysi_arrears_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("應可建立拋棄式資料庫");

    let outcome = test(with_database(&url, &name)).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .expect("應可刪掉拋棄式資料庫");
    outcome.unwrap();
}

async fn pay_twice(database_url: &str, first_run_predates_the_fund: bool) -> anyhow::Result<()> {
    let store = Store::connect(database_url, 2).await?;
    store.migrate().await?;
    store.reload_regulations().await?;
    let registry = Registry::new();
    let ctx = OpCtx {
        store: store.clone(),
        objects: None,
        actor: "負責人".into(),
        actor_role: ROLE_BOSS.into(),
        username: None,
        employee_id: None,
        user_id: None,
        token_id: None,
        public_base: None,
        prompt: None,
        channel: Channel::Ui,
        client_ip: None,
        today: NaiveDate::from_ymd_opt(2026, 7, 5).context("有效日期")?,
        offset: laysi_ops::business_offset(),
    };
    let hire = |name: &str, hired_on: &str| {
        json!({
            "name": name,
            "hired_on": hired_on,
            "base_salary": "53000",
            "labor_insured_salary": "45800",
            "health_insured_salary": "53000",
            "pension_wage": "53000",
        })
    };
    let pay = json!({ "period": "2026-06", "pay_on": "2026-07-05" });
    let run_arrears = |out: &serde_json::Value| -> anyhow::Result<Money> {
        let value = out["run"]["wage_arrears"]
            .as_str()
            .context("計薪結果沒有墊償基金")?;
        Ok(value.parse()?)
    };

    registry
        .dispatch(
            &ctx,
            "create_bank_account",
            json!({ "name": "薪轉戶", "account_no": "000-111", "gl_code": "1102" }),
        )
        .await?;

    // Hired on the 9th: 22 days, 33,586.6667 × 0.025% = 8.40.
    registry
        .dispatch(&ctx, "create_employee", hire("月中到職", "2026-06-09"))
        .await?;
    let first = registry.dispatch(&ctx, "run_payroll", pay.clone()).await?;
    ensure!(
        run_arrears(&first)? == Money::from(8),
        "第一次計算的墊償基金應為 8，實際 {}",
        run_arrears(&first)?
    );
    let recorded_first = match first_run_predates_the_fund {
        false => Money::from(8),
        true => {
            sqlx::query(
                "UPDATE payroll_run
                    SET wage_arrears = 0, employment_version = NULL,
                        accident_version = NULL, wage_arrears_version = NULL",
            )
            .execute(store.pool())
            .await?;
            Money::ZERO
        }
    };

    // Paid late in a second run: the month's total is 79,386.6667 × 0.025%
    // = 19.85, so the period comes to 20. This run records what the first
    // did not, where its own share would round to 11.
    registry
        .dispatch(&ctx, "create_employee", hire("補發員工", "2026-01-01"))
        .await?;
    let second = registry.dispatch(&ctx, "run_payroll", pay).await?;
    let want = Money::from(20) - recorded_first;
    ensure!(
        run_arrears(&second)? == want,
        "補算的墊償基金應為 {want}，實際 {}",
        run_arrears(&second)?
    );

    let period: Money =
        sqlx::query_scalar("SELECT sum(wage_arrears) FROM payroll_run WHERE period = '2026-06'")
            .fetch_one(store.pool())
            .await?;
    ensure!(
        period == Money::from(20),
        "同一期兩次計算的墊償基金合計應為 20，實際 {period}"
    );
    Ok(())
}
