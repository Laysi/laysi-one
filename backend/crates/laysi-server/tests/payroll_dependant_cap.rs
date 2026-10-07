//! Payroll with no `health_dependents` version in force.
//!
//! `premiums_on` resolves the dependant cap whatever the count, so a missing
//! version stops the run rather than charging every employee for one head.
//! The regulation cache is process-wide, which is why this has a test binary
//! of its own: the reduced set installed here cannot reach any other test.

use anyhow::{ensure, Context};
use chrono::NaiveDate;
use laysi_ops::{Channel, OpCtx, OpError, Registry, ROLE_BOSS};
use laysi_regulation::RegulationError;
use laysi_store::Store;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

mod common;
use common::with_database;

/// Run with:
/// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-server
/// payroll_stops_when_no_dependant_cap_is_in_force -- --ignored`
#[tokio::test]
#[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
async fn payroll_stops_when_no_dependant_cap_is_in_force() {
    let url = std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("應可連上測試資料庫");
    let name = format!("laysi_payroll_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("應可建立拋棄式資料庫");

    let outcome = pay_without_cap(&with_database(&url, &name)).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .expect("應可刪掉拋棄式資料庫");
    outcome.unwrap();
}

async fn pay_without_cap(database_url: &str) -> anyhow::Result<()> {
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

    // No dependants at all: the cap is resolved even when there is nobody
    // for it to cap.
    registry
        .dispatch(
            &ctx,
            "create_employee",
            json!({
                "name": "無眷屬員工",
                "hired_on": "2026-01-01",
                "base_salary": "53000",
                "labor_insured_salary": "45800",
                "health_insured_salary": "53000",
                "pension_wage": "53000",
            }),
        )
        .await?;

    // Straight into the cache: `reload_regulations` seeds a domain that has
    // no version, so deleting the row would bring the factory one back.
    let without_cap: Vec<_> = store
        .regulations()
        .await?
        .into_iter()
        .filter(|r| r.domain != "health_dependents")
        .collect();
    laysi_regulation::install(without_cap);

    let refused = registry
        .dispatch(
            &ctx,
            "run_payroll",
            json!({ "period": "2026-06", "pay_on": "2026-07-05" }),
        )
        .await;
    match refused {
        Err(OpError::Regulation(RegulationError::NotFound { domain, .. }))
            if domain == "health_dependents" => {}
        other => anyhow::bail!("沒有眷口上限時計薪應回查無法規版本，實際為 {other:?}"),
    }

    let slips: i64 = sqlx::query_scalar("SELECT count(*) FROM payslip")
        .fetch_one(store.pool())
        .await?;
    ensure!(slips == 0, "計薪失敗不應留下薪資單，實際 {slips} 張");
    Ok(())
}
