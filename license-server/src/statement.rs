//! Announcing the monthly statement to the vendor's own system.
//!
//! A month is settled when it ends, so nobody produces the statement: it
//! exists the moment the month is over, and this is the part that says so.
//! One address, typed on the console by whoever reads the statement.
//!
//! The payload carries both a `text` line and the figures as fields. The two
//! readers are not the same: an accounting system wants the per-partner
//! amounts, a chat room shows whatever is in `text` and ignores the rest.
//! Sending both costs one string and removes the question of which kind of
//! address this is.

use anyhow::Result;
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};
use std::time::Duration;

use crate::db;

const TIMEOUT: Duration = Duration::from_secs(10);

/// How often the task looks at the calendar. A month boundary is the only
/// thing it can act on, so the interval decides how late an announcement can
/// be after a restart, not how often anything is sent.
const TICK: Duration = Duration::from_secs(3600);

/// An address a statement can be posted to.
pub fn validate_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| {
        "推送網址請填完整的網址，例如 https://example.com/hooks/statement".to_string()
    })?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err("推送網址要以 https:// 開頭".into());
    }
    Ok(())
}

/// First day of the month before the one `today` falls in.
pub fn previous_month(today: NaiveDate) -> NaiveDate {
    let first = today.with_day(1).unwrap_or(today);
    first
        .checked_sub_months(chrono::Months::new(1))
        .unwrap_or(first)
}

/// What the vendor's system is sent: the month, the total, and one entry per
/// partner, which is one invoice each.
pub fn payload(month: NaiveDate, rows: &[db::BillingRow]) -> Value {
    let mut partners: Vec<Value> = Vec::new();
    for chunk in rows.chunk_by(|a, b| a.partner_no == b.partner_no) {
        let first = &chunk[0];
        partners.push(json!({
            "partner_no": first.partner_no,
            "name": first.partner_name,
            "tax_id": first.tax_id,
            "terms": chunk.len(),
            "amount": chunk.iter().map(|r| r.amount).sum::<i64>(),
        }));
    }
    let total: i64 = rows.iter().map(|r| r.amount).sum();
    let label = month.format("%Y-%m").to_string();
    json!({
        "event": "monthly_statement",
        "month": label,
        "partners": partners,
        "terms": rows.len(),
        "total": total,
        "currency": "TWD",
        "tax_included": false,
        "text": format!(
            "{label} 月結報告：{} 家經銷商 · {} 筆 · 授權費 {}（未含營業稅）",
            partners.len(),
            rows.len(),
            total
        ),
    })
}

/// Post one payload and say what went wrong in the sentence the console
/// shows.
pub async fn deliver(url: &str, body: &Value) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| "推送失敗：連線設定有誤".to_string())?;
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|_| "推送失敗：無法連線到推送網址，請確認網址與對外連線".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "推送失敗：對方回應 {}，請確認推送網址",
            response.status().as_u16()
        ));
    }
    Ok(())
}

/// Announce `month` if it has not been announced yet.
///
/// The month is claimed before it is sent and handed back when the send
/// fails, so a broken address is retried on the next tick and a working one
/// is never sent twice. A month with nothing sold is claimed and not sent:
/// there is no invoice to announce, and silence says the same thing.
async fn announce(pool: &sqlx::PgPool, month: NaiveDate, previous: Option<NaiveDate>) {
    let Ok(Some(url)) = db::claim_statement_month(pool, month).await else {
        return;
    };
    let rows = match db::billing(pool, month).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, "reading the statement to announce");
            let _ = db::release_statement_month(pool, month, previous).await;
            return;
        }
    };
    if rows.is_empty() {
        tracing::info!(month = %month, "statement is empty, nothing announced");
        return;
    }
    match deliver(&url, &payload(month, &rows)).await {
        Ok(()) => tracing::info!(month = %month, "statement announced"),
        Err(note) => {
            tracing::warn!(month = %month, note, "statement not announced, will retry");
            let _ = db::release_statement_month(pool, month, previous).await;
        }
    }
}

/// Watch the calendar for the rest of the process's life.
///
/// Spawned rather than awaited: a webhook nobody has set up yet must not
/// decide whether the server starts.
pub fn watch(state: crate::Shared) {
    tokio::spawn(async move {
        loop {
            let month = previous_month(state.today());
            match db::vendor_setting(&state.pool).await {
                Ok((url, notified)) if !url.trim().is_empty() => {
                    if notified.is_none_or(|last| last < month) {
                        announce(&state.pool, month, notified).await;
                    }
                }
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "reading the statement webhook"),
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(partner_no: &str, amount: i64) -> db::BillingRow {
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        db::BillingRow {
            partner_no: partner_no.into(),
            partner_name: format!("經銷商 {partner_no}"),
            tax_id: "12345678".into(),
            license_no: "L2609-001".into(),
            licensee: "客戶".into(),
            from_on: day(2026, 9, 1),
            to_on: day(2027, 9, 1),
            amount,
            first_term: true,
        }
    }

    #[test]
    fn the_month_announced_is_the_one_that_just_ended() {
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(previous_month(day(2026, 10, 1)), day(2026, 9, 1));
        assert_eq!(previous_month(day(2026, 10, 31)), day(2026, 9, 1));
        // Across a year boundary the month before January is last December.
        assert_eq!(previous_month(day(2027, 1, 5)), day(2026, 12, 1));
        // A 31st has no counterpart in the month before; the first is taken
        // before the subtraction, so it cannot slide two months back.
        assert_eq!(previous_month(day(2026, 3, 31)), day(2026, 2, 1));
    }

    #[test]
    fn a_partner_is_one_entry_carrying_their_own_total() {
        let out = payload(
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            &[
                row("P-001", 36_000),
                row("P-001", 12_000),
                row("P-002", 24_000),
            ],
        );
        assert_eq!(out["month"], json!("2026-09"));
        assert_eq!(out["total"], json!(72_000));
        assert_eq!(out["terms"], json!(3));
        assert_eq!(out["partners"].as_array().unwrap().len(), 2);
        assert_eq!(out["partners"][0]["partner_no"], json!("P-001"));
        assert_eq!(out["partners"][0]["amount"], json!(48_000));
        assert_eq!(out["partners"][0]["terms"], json!(2));
        assert_eq!(out["partners"][1]["amount"], json!(24_000));
        // A chat room reads this and ignores everything above it.
        assert!(out["text"].as_str().unwrap().contains("2026-09"));
    }

    #[test]
    fn only_https_addresses_are_stored() {
        assert!(validate_url("https://example.com/hooks/statement").is_ok());
        assert!(validate_url("http://example.com/hooks/statement").is_err());
        assert!(validate_url("example.com").is_err());
        assert!(validate_url("").is_err());
    }
}
