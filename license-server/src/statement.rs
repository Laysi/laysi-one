//! Announcing the monthly statement to the vendor's own system.
//!
//! A month is settled when it ends, so nobody produces the statement: it
//! exists the moment the month is over, and this is the part that says so.
//! One address, typed on the console by whoever reads the statement.
//!
//! The payload carries both a `text` line and the figures as fields: an
//! accounting system wants the per-partner amounts, a chat room shows the
//! line. Lark reads it from a shape of its own and answers 200 to a body it
//! did not post, so a Lark address is sent that shape instead.

use anyhow::Result;
use base64::Engine;
use chrono::{Datelike, NaiveDate};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::time::Duration;

use crate::db::{self, Webhook};

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

const LARK: [&str; 2] = ["open.larksuite.com", "open.feishu.cn"];

/// Whether the address's host is Lark's or below it.
pub fn is_lark(url: &str) -> bool {
    let Some(host) = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
    else {
        return false;
    };
    LARK.iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// The body the receiver at this address reads. A signed Lark bot also wants
/// the second the message was signed at, and refuses one over an hour old.
fn body_for(hook: &Webhook, now: i64, statement: &Value) -> Value {
    if !is_lark(&hook.url) {
        return statement.clone();
    }
    let mut body = json!({ "msg_type": "text", "content": { "text": statement["text"] } });
    if !hook.secret.is_empty() {
        body["timestamp"] = json!(now.to_string());
        body["sign"] = json!(lark_sign(&hook.secret, now));
    }
    body
}

/// Lark's signature: base64 of the HMAC-SHA256 of an empty message, keyed by
/// the timestamp and the secret on two lines.
fn lark_sign(secret: &str, timestamp: i64) -> String {
    let mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(format!("{timestamp}\n{secret}").as_bytes())
            .expect("HMAC takes a key of any length");
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Post one payload and say what went wrong in the sentence the console
/// shows.
pub async fn deliver(hook: &Webhook, body: &Value) -> Result<(), String> {
    let url = &hook.url;
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| "推送失敗：連線設定有誤".to_string())?;
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .body(body_for(hook, chrono::Utc::now().timestamp(), body).to_string())
        .send()
        .await
        .map_err(|_| "推送失敗：無法連線到推送網址，請確認網址與對外連線".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "推送失敗：對方回應 {}，請確認推送網址",
            response.status().as_u16()
        ));
    }
    if is_lark(url) {
        // Lark puts a refusal (wrong shape, keyword, signature, IP list) in
        // the body of a 200, so the body is what says whether it posted.
        let reply = response
            .text()
            .await
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .unwrap_or_default();
        lark_accepted(&reply)?;
    }
    Ok(())
}

/// Whether Lark posted the message, from either reply format it uses. Only a
/// code of 0 says it did: a reply that cannot be read, or carries no code,
/// is not taken as a yes, since a yes claims the month for good.
fn lark_accepted(reply: &Value) -> Result<(), String> {
    match reply["code"]
        .as_i64()
        .or_else(|| reply["StatusCode"].as_i64())
    {
        Some(0) => Ok(()),
        Some(_) => Err(format!(
            "推送失敗：對方回應「{}」，請確認推送網址與機器人的安全設定",
            reply["msg"]
                .as_str()
                .or_else(|| reply["StatusMessage"].as_str())
                .unwrap_or_default()
        )),
        None => Err("推送失敗：對方沒有確認收到，請確認推送網址是 Lark 或飛書的機器人網址".into()),
    }
}

/// Announce `month` if it has not been announced yet.
///
/// The month is claimed before it is sent and handed back when the send
/// fails, so a broken address is retried on the next tick and a working one
/// is never sent twice. A month with nothing sold is claimed and not sent:
/// there is no invoice to announce, and silence says the same thing.
async fn announce(pool: &sqlx::PgPool, month: NaiveDate, previous: Option<NaiveDate>) {
    let Ok(Some(hook)) = db::claim_statement_month(pool, month).await else {
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
    match deliver(&hook, &payload(month, &rows)).await {
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
                Ok((hook, notified)) if !hook.url.trim().is_empty() => {
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

    /// Lark posts nothing for a body without `msg_type` and still answers
    /// 200, which is how the statement went missing.
    #[test]
    fn lark_gets_the_shape_it_reads() {
        let statement = payload(
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            &[row("P-001", 1)],
        );
        let text = statement["text"].clone();
        let lark = body_for(
            &hook("https://open.larksuite.com/open-apis/bot/v2/hook/x", ""),
            0,
            &statement,
        );
        assert_eq!(lark["msg_type"], json!("text"));
        assert_eq!(lark["content"]["text"], text);
        assert!(lark.get("sign").is_none());
        let feishu = body_for(
            &hook("https://open.feishu.cn/open-apis/bot/v2/hook/x", ""),
            0,
            &statement,
        );
        assert_eq!(feishu["content"]["text"], text);
        // Every other address keeps the figures, and a lookalike path is not
        // Lark.
        assert_eq!(
            body_for(&hook("https://erp.example.com/hooks", ""), 0, &statement),
            statement
        );
        assert_eq!(
            body_for(
                &hook("https://relay.example.com/open.larksuite.com/x", ""),
                0,
                &statement
            ),
            statement
        );
    }

    /// The expected value is Lark's own recipe run in Python:
    /// base64(hmac.new(b"1599360473\nabc", digestmod=sha256).digest()).
    #[test]
    fn a_signed_lark_bot_gets_the_timestamp_and_its_signature() {
        let statement = payload(
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            &[row("P-001", 1)],
        );
        let signed = body_for(
            &hook("https://open.feishu.cn/open-apis/bot/v2/hook/x", "abc"),
            1_599_360_473,
            &statement,
        );
        assert_eq!(signed["timestamp"], json!("1599360473"));
        assert_eq!(
            signed["sign"],
            json!("cgtpJzI2j6bUDggbdGYjskCK3FPgKpTkwosfWsMzKqM=")
        );
        assert_eq!(signed["content"]["text"], statement["text"]);
    }

    fn hook(url: &str, secret: &str) -> Webhook {
        Webhook {
            url: url.into(),
            secret: secret.into(),
        }
    }

    #[test]
    fn a_lark_refusal_in_a_200_is_a_failure() {
        let refused = json!({ "code": 19002, "data": {}, "msg": "params error, msg_type need" });
        assert!(lark_accepted(&refused)
            .unwrap_err()
            .contains("params error, msg_type need"));
        let old = json!({ "StatusCode": 19024, "StatusMessage": "Key Words Not Found" });
        assert!(lark_accepted(&old)
            .unwrap_err()
            .contains("Key Words Not Found"));
        assert!(lark_accepted(&json!({ "code": 0, "msg": "success", "StatusCode": 0 })).is_ok());
        assert!(lark_accepted(&json!({ "StatusCode": 0 })).is_ok());
        // An unreadable body or one without a code is not proof it posted.
        assert!(lark_accepted(&Value::Null).is_err());
        assert!(lark_accepted(&json!({ "ok": true })).is_err());
    }

    #[test]
    fn only_https_addresses_are_stored() {
        assert!(validate_url("https://example.com/hooks/statement").is_ok());
        assert!(validate_url("http://example.com/hooks/statement").is_err());
        assert!(validate_url("example.com").is_err());
        assert!(validate_url("").is_err());
    }
}
