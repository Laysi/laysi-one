//! The partner console: server-rendered HTML, no build step and no client
//! framework.
//!
//! Every page is a form post and a redirect. That is not a stylistic
//! preference so much as the size of the job — issue a licence, renew it,
//! unbind it, read the month — and a console that mints licences is better
//! off with no bundler in its supply chain.
//!
//! The design vocabulary is the product's: lines and rules rather than filled
//! blocks, figures in a tabular face. Screen text is Traditional Chinese.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use sqlx::Row;
use std::collections::HashMap;

use crate::db::{self, Actor};
use crate::token;
use crate::{auth, Shared};

/// HTML-escape. Everything a partner typed passes through here on the way to
/// a page; a company name with an angle bracket in it is a mistake, not an
/// opportunity.
fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const STYLE: &str = r#"
:root{--ink:#1c1c1a;--paper:#faf8f4;--rule:#d8d2c6;--muted:#6b665c;--accent:#234b7d;--warn:#8a3324}
*{box-sizing:border-box}
body{margin:0;background:var(--paper);color:var(--ink);font:15px/1.6 "Inter","Noto Sans TC",system-ui,sans-serif}
.num{font-variant-numeric:tabular-nums lining-nums;font-feature-settings:"tnum"}
header{border-bottom:1px solid var(--rule);padding:0 24px;display:flex;align-items:center;gap:24px;flex-wrap:wrap}
header b{font-size:17px;letter-spacing:.02em;padding:16px 0}
header nav{display:flex;gap:18px;flex:1}
header a{color:var(--ink);text-decoration:none;padding:16px 0;border-bottom:2px solid transparent}
header a:hover,header a.on{border-bottom-color:var(--accent)}
header form{margin:0}
main{max-width:1100px;margin:0 auto;padding:28px 24px 64px}
h1{font-size:20px;font-weight:600;margin:0 0 4px}
h2{font-size:15px;font-weight:600;margin:34px 0 14px;padding-bottom:6px;border-bottom:1px solid var(--rule)}
.lede{color:var(--muted);margin:0 0 24px}
table{width:100%;border-collapse:collapse;font-size:14px}
th{text-align:left;font-weight:600;color:var(--muted);border-bottom:1px solid var(--rule);padding:8px 18px 8px 0}
td{border-bottom:1px solid var(--rule);padding:11px 18px 11px 0;vertical-align:middle}
td.r,th.r{text-align:right}
/* Only the last column may sit flush against the table edge. */
td:last-child,th:last-child{padding-right:0}
a{color:var(--accent)}
form.card{border:1px solid var(--rule);padding:18px;margin:0 0 24px;background:#fff}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:14px}
label{display:flex;flex-direction:column;gap:4px;font-size:13px;color:var(--muted)}
input,select{font:inherit;padding:7px 9px;border:1px solid var(--rule);background:#fff;color:var(--ink);border-radius:0}
input:focus,select:focus{outline:2px solid var(--accent);outline-offset:-2px}
button{font:inherit;padding:7px 16px;border:1px solid var(--accent);background:transparent;color:var(--accent);cursor:pointer}
button:hover{background:var(--accent);color:#fff}
button.plain{border-color:var(--rule);color:var(--muted)}
button.plain:hover{background:var(--rule);color:var(--ink)}
button.warn{border-color:var(--warn);color:var(--warn)}
button.warn:hover{background:var(--warn);color:#fff}
.actions{display:flex;gap:10px;flex-wrap:wrap;margin-top:14px;align-items:center}
.tag{font-size:12px;line-height:1.5;border:1px solid var(--rule);padding:3px 8px;color:var(--muted);white-space:nowrap}
.cell{display:flex;align-items:center;gap:10px}
.tag.on{border-color:var(--accent);color:var(--accent)}
.tag.off{border-color:var(--warn);color:var(--warn)}
.key{border:1px solid var(--accent);padding:16px;margin:0 0 24px;background:#fff}
.key code{font-size:20px;letter-spacing:.08em;display:block;margin:8px 0}
.note{color:var(--muted);font-size:13px;margin:6px 0 0}
.fp{font-size:12px;color:var(--muted);word-break:break-all}
.empty{color:var(--muted);padding:24px 0}
textarea{font:13px/1.5 ui-monospace,monospace;width:100%;min-height:120px;padding:9px;border:1px solid var(--rule)}
form.inline{display:inline-flex;gap:6px;align-items:center;margin:0}
form.inline input{width:6.5em;text-align:right}
label.row{flex-direction:row;align-items:center;gap:8px}
label.row input{width:auto}
@media (max-width:640px){.wide{display:none}}
"#;

fn page(title: &str, actor: Option<&Actor>, body: &str) -> Html<String> {
    let nav = match actor {
        Some(a) => {
            let partners = if a.is_vendor {
                r#"<a href="/partners">經銷商</a>"#
            } else {
                ""
            };
            // Drawn only for the people it works for: a plain member manages
            // nobody, and the page would refuse them on arrival.
            // Pricing and the monthly statement are the vendor's side of the
            // relationship; a reseller neither sets a price nor reconciles one.
            let billing = if a.is_vendor {
                r#"<a href="/billing">月結對帳</a>"#
            } else {
                ""
            };
            let members = if a.is_vendor || a.is_admin {
                r#"<a href="/members">成員</a>"#
            } else {
                ""
            };
            let who = if a.name.trim().is_empty() {
                a.email.clone()
            } else {
                a.name.clone()
            };
            format!(
                r#"<nav><a href="/licenses">授權</a>{billing}{partners}{members}</nav>
                   <span class="tag">{}（{}）· {}</span>
                   <form method="post" action="/logout"><button class="plain">登出</button></form>"#,
                esc(&a.partner_name),
                esc(&a.partner_no),
                esc(&who)
            )
        }
        None => String::new(),
    };
    Html(format!(
        r#"<!doctype html><html lang="zh-Hant"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{} · Laysi One 授權管理</title><style>{STYLE}</style></head>
<body><header><b>Laysi One 授權管理</b>{nav}</header><main>{body}</main></body></html>"#,
        esc(title)
    ))
}

fn date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn instant(t: Option<chrono::DateTime<Utc>>) -> String {
    match t {
        Some(t) => t
            .with_timezone(&crate::business_offset())
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => "—".into(),
    }
}

/// Licence status as the console says it, with the contract date folded in:
/// a licence still marked active whose year ended is expired, and saying
/// "啟用中" next to a date in the past is the screen contradicting itself.
fn status_label(
    status: &str,
    expires_on: NaiveDate,
    today: NaiveDate,
) -> (&'static str, &'static str) {
    match status {
        "revoked" => ("已註銷", "off"),
        "suspended" => ("已暫停", "off"),
        _ if expires_on < today => ("已到期", "off"),
        "issued" => ("尚未啟用", ""),
        _ => ("使用中", "on"),
    }
}

// Sign in

pub async fn login_form(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if actor(&s, &headers).await.is_some() {
        return Redirect::to("/licenses").into_response();
    }
    page(
        "登入",
        None,
        r#"<h1>登入</h1><p class="lede">使用經銷商帳號登入。</p>
        <form class="card" method="post" action="/login" style="max-width:420px">
          <div class="grid" style="grid-template-columns:1fr">
            <label>電子郵件<input name="email" type="email" autocomplete="username" required></label>
            <label>密碼<input name="password" type="password" autocomplete="current-password" required></label>
          </div>
          <div class="actions"><button>登入</button></div>
        </form>"#,
    )
    .into_response()
}

#[derive(Deserialize)]
pub struct LoginForm {
    email: String,
    password: String,
}

pub async fn login(
    State(s): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    match auth::login(&s.pool, &form.email, &form.password).await {
        Ok(Some(session)) => (
            [(
                axum::http::header::SET_COOKIE,
                auth::set_cookie(&session, auth::secure(&headers)),
            )],
            Redirect::to("/licenses"),
        )
            .into_response(),
        Ok(None) => page(
            "登入",
            None,
            r#"<h1>登入</h1><p class="lede">電子郵件或密碼不正確，請再試一次。</p>
            <form class="card" method="post" action="/login" style="max-width:420px">
              <div class="grid" style="grid-template-columns:1fr">
                <label>電子郵件<input name="email" type="email" required></label>
                <label>密碼<input name="password" type="password" required></label>
              </div>
              <div class="actions"><button>登入</button></div>
            </form>"#,
        )
        .into_response(),
        Err(error) => fail(error),
    }
}

pub async fn logout(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(token) = auth::cookie(&headers) {
        let _ = auth::logout(&s.pool, &token).await;
    }
    (
        [(
            axum::http::header::SET_COOKIE,
            auth::clear_cookie(auth::secure(&headers)),
        )],
        Redirect::to("/login"),
    )
        .into_response()
}

async fn actor(s: &Shared, headers: &HeaderMap) -> Option<Actor> {
    let token = auth::cookie(headers)?;
    db::actor_of(&s.pool, &token).await.ok().flatten()
}

fn fail(error: anyhow::Error) -> Response {
    tracing::error!(%error, "console request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        page(
            "錯誤",
            None,
            r#"<h1>暫時無法處理</h1><p class="lede">請稍後再試。</p>"#,
        ),
    )
        .into_response()
}

/// Every console route starts here: no session means the sign-in page, and
/// there is no third case.
macro_rules! signed_in {
    ($s:expr, $headers:expr) => {
        match actor(&$s, &$headers).await {
            // A generated password has one destination until it is replaced.
            Some(a) if a.must_change_password => return Redirect::to("/password").into_response(),
            Some(a) => a,
            None => return Redirect::to("/login").into_response(),
        }
    };
}

// Licences

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    q: String,
    /// The key just issued, shown once and only on the redirect that follows
    /// its own creation.
    #[serde(default)]
    key: String,
    #[serde(default)]
    no: String,
}

pub async fn licenses(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let a = signed_in!(s, headers);
    let rows = match db::licenses(&s.pool, &a, query.q.trim()).await {
        Ok(rows) => rows,
        Err(error) => return fail(error),
    };
    let today = s.today();

    let issued = if query.key.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="key"><strong>{} 的啟動金鑰</strong>
            <code class="num">{}</code>
            <p class="note">交給客戶，在建立系統管理員帳號之前輸入。離開本頁之後無法再顯示，遺失時重新開立。</p>
            </div>"#,
            esc(&query.no),
            esc(&query.key)
        )
    };

    let body: String = rows
        .iter()
        .map(|r| {
            let (label, tone) = status_label(&r.status, r.expires_on, today);
            let bound = match &r.fingerprint {
                Some(_) => format!("已綁定 · {}", instant(r.last_seen_at)),
                None => "尚未啟用".to_string(),
            };
            // Attempts that were turned away belong on the row a person
            // scans, not only on the page they have to open first: a refusal
            // nobody sees is the same as not refusing.
            let mut flag = String::new();
            if r.conflicts > 0 {
                flag.push_str(&format!(
                    r#" <span class="tag off">重複安裝 {}</span>"#,
                    r.conflicts
                ));
            }
            if r.denials > 0 {
                flag.push_str(&format!(
                    r#" <span class="tag off">啟動被拒 {} 次</span>"#,
                    r.denials
                ));
            }
            format!(
                r#"<tr><td><a href="/licenses/{no}">{no}</a></td>
                <td>{licensee}<br><span class="fp">{tax}</span></td>
                <td class="wide">{partner}</td>
                <td><div class="cell"><span class="tag {tone}">{label}</span>{flag}</div></td>
                <td class="num">{expires}</td>
                <td class="wide">{bound}</td></tr>"#,
                no = esc(&r.license_no),
                licensee = esc(&r.licensee),
                tax = esc(&r.licensee_tax_id),
                partner = esc(&r.partner_name),
                expires = date(r.expires_on),
            )
        })
        .collect();

    let table = if rows.is_empty() {
        r#"<p class="empty">請用上方表單開立第一組授權。</p>"#.to_string()
    } else {
        format!(
            r#"<table><thead><tr><th>授權編號</th><th>被授權單位</th><th class="wide">經銷商</th>
            <th>狀態</th><th>到期日</th><th class="wide">安裝</th></tr></thead>
            <tbody>{body}</tbody></table>"#
        )
    };

    let default_expiry = date(today + chrono::Duration::days(365));
    page(
        "授權",
        Some(&a),
        &format!(
            r#"<h1>授權</h1>
            <p class="lede">開立後把啟動金鑰交給客戶，客戶在建立系統管理員帳號之前輸入。</p>
            {issued}
            <form class="card" method="post" action="/licenses">
              <div class="grid">
                <label>被授權單位<input name="licensee" required></label>
                <label>統一編號<input name="licensee_tax_id" inputmode="numeric" maxlength="8"></label>
                <label>到期日<input name="expires_on" type="date" value="{default_expiry}" required></label>
                <label>備註<input name="note"></label>
              </div>
              <div class="actions"><button>開立授權</button></div>
            </form>
            <form method="get" action="/licenses" class="actions" style="margin-bottom:18px">
              <input name="q" value="{q}" placeholder="搜尋單位名稱、統編或授權編號" style="min-width:260px">
              <button class="plain">搜尋</button>
            </form>
            {table}"#,
            q = esc(&query.q)
        ),
    )
    .into_response()
}

#[derive(Deserialize)]
pub struct IssueForm {
    licensee: String,
    #[serde(default)]
    licensee_tax_id: String,
    expires_on: NaiveDate,
    #[serde(default)]
    note: String,
}

pub async fn issue(
    State(s): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<IssueForm>,
) -> Response {
    let a = signed_in!(s, headers);
    match issue_inner(&s, &a, &form).await {
        Ok((no, key)) => Redirect::to(&format!(
            "/licenses?no={}&key={}",
            urlencode(&no),
            urlencode(&key)
        ))
        .into_response(),
        Err(error) => fail(error),
    }
}

async fn issue_inner(s: &Shared, a: &Actor, form: &IssueForm) -> anyhow::Result<(String, String)> {
    let today = s.today();
    let mut tx = s.pool.begin().await?;
    let no = db::next_license_no(&mut tx, today).await?;
    let key = token::new_activation_key();
    let id: i32 = sqlx::query(
        "INSERT INTO license (license_no, partner_id, licensee, licensee_tax_id,
                              key_hash, key_prefix, expires_on, note, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
    )
    .bind(&no)
    .bind(a.partner_id)
    .bind(form.licensee.trim())
    .bind(form.licensee_tax_id.trim())
    .bind(token::key_hash(&key))
    .bind(token::key_prefix(&key))
    .bind(form.expires_on)
    .bind(form.note.trim())
    .bind(a.user_id)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    // The first term, priced from the partner rate the vendor set. The figure
    // is copied rather than looked up later: a price change must not rewrite
    // what was already sold.
    sqlx::query(
        "INSERT INTO license_term (license_id, from_on, to_on, amount, created_by)
         VALUES ($1, $2, $3, (SELECT unit_price FROM partner WHERE id = $4), $5)",
    )
    .bind(id)
    .bind(today)
    .bind(form.expires_on)
    .bind(a.partner_id)
    .bind(a.user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((no, key))
}

pub async fn detail(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(no): Path<String>,
) -> Response {
    let a = signed_in!(s, headers);
    let found = match db::license(&s.pool, &a, &no).await {
        Ok(row) => row,
        Err(error) => return fail(error),
    };
    let Some(r) = found else {
        return (
            StatusCode::NOT_FOUND,
            page("查無授權", Some(&a), r#"<h1>查無此授權編號</h1>"#),
        )
            .into_response();
    };
    let (terms, denials) = match (
        db::terms(&s.pool, &no).await,
        db::denials(&s.pool, &no).await,
    ) {
        (Ok(t), Ok(d)) => (t, d),
        (Err(e), _) | (_, Err(e)) => return fail(e),
    };
    let today = s.today();
    let (label, tone) = status_label(&r.status, r.expires_on, today);

    // The fee column belongs to the vendor: the partner rate is set and
    // reconciled on that side, and a reseller reading this page is checking
    // dates rather than an invoice.
    let (fee_head, fee_cell) = if a.is_vendor {
        (r#"<th class="r">授權費</th>"#, true)
    } else {
        ("", false)
    };
    let term_rows: String = terms
        .iter()
        .map(|t| {
            let fee = if fee_cell {
                format!(r#"<td class="num r">{}</td>"#, esc(&t.amount))
            } else {
                String::new()
            };
            format!(
                r#"<tr><td class="num">{} 至 {}</td><td class="num">{}</td>{}</tr>"#,
                date(t.from_on),
                date(t.to_on),
                date(t.billed_on),
                fee
            )
        })
        .collect();

    let denial_rows: String = denials
        .iter()
        .map(|d| {
            format!(
                r#"<tr><td class="num">{}</td><td>{}</td><td class="num wide">{}</td>
                <td class="fp wide">{}</td></tr>"#,
                instant(Some(d.at)),
                denial_label(&d.reason),
                esc(&d.ip),
                esc(&d.fingerprint)
            )
        })
        .collect();
    let denial_block = if denials.is_empty() {
        String::new()
    } else {
        format!(
            r#"<h2>被拒絕的檢查</h2>
            <table><thead><tr><th>時間</th><th>原因</th><th class="wide">來源位址</th>
            <th class="wide">主機識別碼</th></tr></thead><tbody>{denial_rows}</tbody></table>"#
        )
    };

    let bound_block = match &r.fingerprint {
        Some(fp) => format!(
            r#"<tr><td>綁定主機</td><td class="fp">{}<br>綁定於 {} · 最後回報 {} · 版本 {}</td></tr>"#,
            esc(fp),
            instant(r.bound_at),
            instant(r.last_seen_at),
            esc(&r.version)
        ),
        None => r#"<tr><td>綁定主機</td><td>尚未啟用；客戶第一次輸入金鑰時綁定，更換主機後請在本頁解除綁定。</td></tr>"#.to_string(),
    };

    // Absolute: a relative action on /licenses/{no} resolves against the
    // parent, so "unbind" posts to /licenses/unbind -- which matches the
    // detail route, accepts GET only, and answers 405.
    let path = urlencode(&no);
    let unbind = if r.fingerprint.is_some() {
        format!(
            r#"<form method="post" action="/licenses/{path}/unbind" style="margin:0"><button class="plain">解除綁定</button></form>"#
        )
    } else {
        String::new()
    };
    let toggle = if r.status == "suspended" || r.status == "revoked" {
        r#"<button name="status" value="active" class="plain">恢復使用</button>"#
    } else {
        r#"<button name="status" value="suspended" class="warn">暫停</button>"#
    };

    page(
        &no,
        Some(&a),
        &format!(
            r#"<h1>{no_esc}</h1><p class="lede">{licensee}</p>
            <table><tbody>
              <tr><td style="width:120px">狀態</td><td><span class="tag {tone}">{label}</span></td></tr>
              <tr><td>統一編號</td><td class="num">{tax}</td></tr>
              <tr><td>經銷商</td><td>{partner}（{partner_no}）</td></tr>
              <tr><td>啟動金鑰</td><td class="num">{prefix}…（僅開立當下顯示完整金鑰）</td></tr>
              <tr><td>到期日</td><td class="num">{expires}</td></tr>
              {bound_block}
              <tr><td>備註</td><td>{note}</td></tr>
            </tbody></table>
            <div class="actions">
              {unbind}
              <form method="post" action="/licenses/{path}/status" style="margin:0">{toggle}</form>
            </div>

            <h2>續約</h2>
            <form class="card" method="post" action="/licenses/{path}/renew">
              <div class="grid">
                <label>新到期日<input name="expires_on" type="date" value="{next_year}" required></label>
              </div>
              <div class="actions"><button>續約</button>
                <span class="note">新增一筆授權期間，客戶的系統在下一次回報時取得新的到期日。</span></div>
            </form>

            <h2>授權期間</h2>
            <table><thead><tr><th>期間</th><th>計費月份</th>{fee_head}</tr></thead>
            <tbody>{term_rows}</tbody></table>
            {denial_block}"#,
            no_esc = esc(&no),
            path = path,
            licensee = esc(&r.licensee),
            tax = esc(&r.licensee_tax_id),
            partner = esc(&r.partner_name),
            partner_no = esc(&r.partner_no),
            prefix = esc(&r.key_prefix),
            expires = date(r.expires_on),
            note = esc(&r.note),
            next_year = date(r.expires_on.max(today) + chrono::Duration::days(365)),
        ),
    )
    .into_response()
}

fn denial_label(reason: &str) -> &'static str {
    match reason {
        "bound_elsewhere" => "已綁定另一台主機",
        "revoked" => "授權已註銷",
        "suspended" => "授權已暫停",
        "partner_suspended" => "經銷商已停用",
        "duplicate_install" => "疑似重複安裝：同一組授權出現兩份資料",
        _ => "其他",
    }
}

#[derive(Deserialize)]
pub struct RenewForm {
    expires_on: NaiveDate,
}

pub async fn renew(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(no): Path<String>,
    Form(form): Form<RenewForm>,
) -> Response {
    let a = signed_in!(s, headers);
    let sql = format!(
        "WITH target AS (
             SELECT l.id, l.expires_on, l.partner_id FROM license l
               JOIN partner p ON p.id = l.partner_id
              WHERE l.license_no = $1 AND {}
         ), extended AS (
             UPDATE license SET expires_on = $2,
                    status = CASE WHEN status = 'revoked' THEN status ELSE 'active' END
              WHERE id IN (SELECT id FROM target) RETURNING id
         )
         INSERT INTO license_term (license_id, from_on, to_on, amount, created_by)
         SELECT t.id, GREATEST(t.expires_on, $3), $2,
                (SELECT unit_price FROM partner WHERE id = t.partner_id),
                $4
           FROM target t WHERE EXISTS (SELECT 1 FROM extended)",
        a.scope_sql()
    );
    // The new term starts where the old one ended, or today when the licence
    // already lapsed: a renewal bought late is a year from now, not a year
    // backdated over months nobody was licensed for.
    match sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&no)
        .bind(form.expires_on)
        .bind(s.today())
        .bind(a.user_id)
        .execute(&s.pool)
        .await
    {
        Ok(_) => Redirect::to(&format!("/licenses/{}", urlencode(&no))).into_response(),
        Err(error) => fail(error.into()),
    }
}

pub async fn unbind(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(no): Path<String>,
) -> Response {
    let a = signed_in!(s, headers);
    // The nonce goes too. Leaving it would make the next check from the new
    // machine look like a duplicate install on its very first call.
    let sql = format!(
        "UPDATE license l SET fingerprint = NULL, install_id = NULL, bound_at = NULL,
                nonce = NULL, conflicts = 0, last_conflict_at = NULL
           FROM partner p
          WHERE p.id = l.partner_id AND l.license_no = $1 AND {}",
        a.scope_sql()
    );
    match sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&no)
        .execute(&s.pool)
        .await
    {
        Ok(_) => Redirect::to(&format!("/licenses/{}", urlencode(&no))).into_response(),
        Err(error) => fail(error.into()),
    }
}

#[derive(Deserialize)]
pub struct StatusForm {
    status: String,
}

pub async fn set_status(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(no): Path<String>,
    Form(form): Form<StatusForm>,
) -> Response {
    let a = signed_in!(s, headers);
    if !["active", "suspended", "revoked"].contains(&form.status.as_str()) {
        return (StatusCode::BAD_REQUEST, "unknown status").into_response();
    }
    let sql = format!(
        "UPDATE license l SET status = $2 FROM partner p
          WHERE p.id = l.partner_id AND l.license_no = $1 AND {}",
        a.scope_sql()
    );
    match sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&no)
        .bind(&form.status)
        .execute(&s.pool)
        .await
    {
        Ok(_) => Redirect::to(&format!("/licenses/{}", urlencode(&no))).into_response(),
        Err(error) => fail(error.into()),
    }
}
// Billing

pub async fn billing(State(s): State<Shared>, headers: HeaderMap) -> Response {
    let a = signed_in!(s, headers);
    if !a.is_vendor {
        return Redirect::to("/licenses").into_response();
    }
    let rows = match db::billing(&s.pool, &a).await {
        Ok(rows) => rows,
        Err(error) => return fail(error),
    };
    let body: String = rows
        .iter()
        .map(|r| {
            format!(
                r#"<tr><td class="num">{}</td><td>{}（{}）</td><td class="num r">{}</td>
                <td class="num r">{}</td></tr>"#,
                esc(&r.month),
                esc(&r.partner_name),
                esc(&r.partner_no),
                r.terms,
                esc(&r.amount)
            )
        })
        .collect();
    let table = if rows.is_empty() {
        r#"<p class="empty">尚無開立或續約紀錄。</p>"#.to_string()
    } else {
        format!(
            r#"<table><thead><tr><th>月份</th><th>經銷商</th><th class="r">筆數</th>
            <th class="r">授權費</th></tr></thead><tbody>{body}</tbody></table>"#
        )
    };
    page(
        "月結對帳",
        Some(&a),
        &format!(
            r#"<h1>月結對帳</h1>
            <p class="lede">開立與續約各計一筆，金額為開立當下的單價。</p>{table}"#
        ),
    )
    .into_response()
}

// Partners (vendor only)

pub async fn partners(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let a = signed_in!(s, headers);
    if !a.is_vendor {
        return Redirect::to("/licenses").into_response();
    }
    let rows = match db::partners(&s.pool).await {
        Ok(rows) => rows,
        Err(error) => return fail(error),
    };
    let body: String = rows
        .iter()
        .map(|r| {
            // The listing reads; the actions live on the partner's own page.
            // A price field and a suspend button on every row are two things
            // to press by accident while looking something up.
            format!(
                r#"<tr><td class="num"><a href="/partners/{no}">{no}</a></td>
                <td>{name}<br><span class="fp">{tax}</span></td>
                <td class="num r">{price}</td>
                <td class="num r">{licenses}</td>
                <td class="num r"><a href="/members?partner={no}">{users}</a></td>
                <td><span class="tag {tone}">{status}</span></td></tr>"#,
                no = esc(&r.partner_no),
                name = esc(&r.name),
                tax = esc(&r.tax_id),
                price = esc(&r.unit_price),
                licenses = r.licenses,
                users = r.users,
                tone = if r.status == "active" { "on" } else { "off" },
                status = if r.status == "active" {
                    "使用中"
                } else {
                    "已停用"
                },
            )
        })
        .collect();

    let created = password_card(&q);

    page(
        "經銷商",
        Some(&a),
        &format!(
            r#"<h1>經銷商</h1>
            <p class="lede">年費一家一個價，開立與續約時複製到該筆授權上，改價不影響已經賣出的授權。</p>
            {created}
            <form class="card" method="post" action="/partners">
              <div class="grid">
                <label>名稱<input name="name" required></label>
                <label>統一編號<input name="tax_id" inputmode="numeric" maxlength="8"></label>
                <label>每組授權年費<input name="unit_price" class="num" inputmode="numeric" value="0"></label>
                <label>登入帳號<input name="email" type="email" required></label>
                <label>聯絡人<input name="contact"></label>
              </div>
              <div class="actions"><button>建立經銷商與帳號</button></div>
            </form>
            <table><thead><tr><th>代號</th><th>名稱</th><th class="r">年費</th>
            <th class="r">授權數</th><th class="r">帳號</th><th>狀態</th></tr></thead>
            <tbody>{body}</tbody></table>"#
        ),
    )
    .into_response()
}

#[derive(Deserialize)]
pub struct PartnerForm {
    name: String,
    #[serde(default)]
    tax_id: String,
    #[serde(default)]
    unit_price: String,
    email: String,
    #[serde(default)]
    contact: String,
}

pub async fn create_partner(
    State(s): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<PartnerForm>,
) -> Response {
    let a = signed_in!(s, headers);
    if !a.is_vendor {
        return Redirect::to("/licenses").into_response();
    }
    match create_partner_inner(&s, &form).await {
        Ok(password) => Redirect::to(&format!(
            "/partners?{}",
            password_result(&s, &headers, form.email.trim(), &password).await
        ))
        .into_response(),
        Err(error) => fail(error),
    }
}

async fn create_partner_inner(s: &Shared, form: &PartnerForm) -> anyhow::Result<String> {
    let mut tx = s.pool.begin().await?;
    let seq: i64 = sqlx::query("SELECT count(*) AS n FROM partner")
        .fetch_one(&mut *tx)
        .await?
        .get("n");
    let partner_no = format!("P-{:03}", seq + 1);
    let price: i64 = form.unit_price.trim().parse().unwrap_or(0);

    let partner_id: i32 = sqlx::query(
        "INSERT INTO partner (partner_no, name, tax_id, unit_price)
         VALUES ($1,$2,$3,$4::numeric) RETURNING id",
    )
    .bind(&partner_no)
    .bind(form.name.trim())
    .bind(form.tax_id.trim())
    .bind(price)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    let password = temporary_password();
    sqlx::query(
        "INSERT INTO partner_user
             (partner_id, email, password_hash, name, is_admin, must_change_password)
         VALUES ($1, lower($2), $3, $4, true, true)",
    )
    .bind(partner_id)
    .bind(form.email.trim())
    .bind(auth::hash_password(&password)?)
    .bind(form.contact.trim())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(password)
}

#[derive(Deserialize)]
pub struct PartnerEditForm {
    #[serde(default)]
    unit_price: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// Change what moves after a partner exists: their price, or whether they are
/// still trading.
///
/// The price is copied onto each term as it is sold, so a change here sets
/// what the next licence-year costs and leaves every invoice already raised
/// exactly as it was.
/// One partner, with the two things that move after it exists: the rate and
/// whether it may still issue. Both were on the listing, where a field and a
/// button sat on every row and could be pressed while reading one.
pub async fn partner_detail(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(no): Path<String>,
) -> Response {
    let a = signed_in!(s, headers);
    if !a.is_vendor {
        return Redirect::to("/licenses").into_response();
    }
    let r = match db::partner(&s.pool, &no).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return page("經銷商", Some(&a), r#"<h1>查無此經銷商代號</h1>"#).into_response()
        }
        Err(error) => return fail(error),
    };
    let path = urlencode(&r.partner_no);
    let (next_status, next_label, next_class) = if r.status == "active" {
        ("suspended", "停用", "warn")
    } else {
        ("active", "啟用", "plain")
    };
    page(
        &r.name,
        Some(&a),
        &format!(
            r#"<h1>{name}</h1><p class="lede">{no_esc}</p>
            <table><tbody>
              <tr><td style="width:120px">統一編號</td><td class="num">{tax}</td></tr>
              <tr><td>狀態</td><td><span class="tag {tone}">{status}</span></td></tr>
              <tr><td>開立授權</td><td class="num">{licenses}</td></tr>
              <tr><td>登入帳號</td><td class="num"><a href="/members?partner={path}">{users}</a></td></tr>
            </tbody></table>
            <div class="actions">
              <form method="post" action="/partners/{path}" style="margin:0">
                <input type="hidden" name="status" value="{next_status}">
                <button class="{next_class}">{next_label}</button></form>
            </div>

            <h2>年費</h2>
            <form class="card" method="post" action="/partners/{path}">
              <div class="grid">
                <label>每組授權年費<input name="unit_price" class="num" inputmode="numeric" value="{price}"></label>
              </div>
              <div class="actions"><button>儲存</button>
                <span class="note">開立與續約時複製到該筆授權上，已經賣出的不受影響。</span></div>
            </form>"#,
            name = esc(&r.name),
            no_esc = esc(&r.partner_no),
            tax = esc(&r.tax_id),
            price = esc(&r.unit_price),
            licenses = r.licenses,
            users = r.users,
            tone = if r.status == "active" { "on" } else { "off" },
            status = if r.status == "active" {
                "使用中"
            } else {
                "已停用"
            },
        ),
    )
    .into_response()
}

pub async fn update_partner(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(no): Path<String>,
    Form(form): Form<PartnerEditForm>,
) -> Response {
    let a = signed_in!(s, headers);
    if !a.is_vendor {
        return Redirect::to("/licenses").into_response();
    }
    // Back to the page the button was on, not to the listing: the figure
    // that was just saved is the one the reader wants to see.
    let back = format!("/partners/{}", urlencode(&no));
    if let Some(raw) = form.unit_price.as_deref() {
        // A price that is not a number is a typo. Storing zero for it would
        // quietly make the next renewal free.
        let Ok(price) = raw.trim().parse::<i64>() else {
            return Redirect::to(&back).into_response();
        };
        if let Err(error) =
            sqlx::query("UPDATE partner SET unit_price = $2::numeric WHERE partner_no = $1")
                .bind(&no)
                .bind(price.max(0))
                .execute(&s.pool)
                .await
        {
            return fail(error.into());
        }
    }
    if let Some(status) = form.status.as_deref() {
        if !["active", "suspended"].contains(&status) {
            return (StatusCode::BAD_REQUEST, "unknown status").into_response();
        }
        if let Err(error) = sqlx::query("UPDATE partner SET status = $2 WHERE partner_no = $1")
            .bind(&no)
            .bind(status)
            .execute(&s.pool)
            .await
        {
            return fail(error.into());
        }
    }
    Redirect::to(&back).into_response()
}

/// A password that survives being read out over the telephone: no `0`/`o`,
/// no `1`/`l`/`i`.
pub fn temporary_password() -> String {
    const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    let limit = (u8::MAX as usize / ALPHABET.len() * ALPHABET.len()) as u8;
    let mut out = String::with_capacity(12);
    let mut buffer = [0u8; 32];
    while out.len() < 12 {
        getrandom::fill(&mut buffer).expect("the OS random source is unavailable");
        for byte in buffer {
            if byte < limit {
                out.push(ALPHABET[byte as usize % ALPHABET.len()] as char);
                if out.len() == 12 {
                    break;
                }
            }
        }
    }
    out
}

/// Percent-encode for a query string. Small enough not to be worth a crate,
/// and the only characters that reach it are from a licence number, an email
/// address or a base64url token.
fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub async fn home(State(s): State<Shared>, headers: HeaderMap) -> Response {
    match actor(&s, &headers).await {
        Some(_) => Redirect::to("/licenses").into_response(),
        None => Redirect::to("/login").into_response(),
    }
}

#[derive(Deserialize)]
pub struct MemberForm {
    /// Empty for one's own partner. Only the vendor may name another.
    #[serde(default)]
    partner_no: String,
    email: String,
    #[serde(default)]
    name: String,
    /// A checkbox: present when ticked, absent otherwise.
    #[serde(default)]
    is_admin: Option<String>,
}

#[derive(Deserialize)]
pub struct MemberEditForm {
    #[serde(default)]
    partner_no: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    is_admin: Option<String>,
    #[serde(default)]
    reset: Option<String>,
}

/// The partner whose members this request is about, once the actor is allowed
/// to manage them. `None` covers both refusals -- no such partner, and one
/// this actor may not touch -- because telling them apart would say whether a
/// partner code exists to someone with no business asking.
async fn manageable(
    s: &Shared,
    a: &Actor,
    requested: &str,
) -> anyhow::Result<Option<(i32, String)>> {
    let (id, name) = match requested.trim() {
        "" => (a.partner_id, a.partner_name.clone()),
        no => match db::partner_by_no(&s.pool, no).await? {
            Some(found) => found,
            None => return Ok(None),
        },
    };
    Ok(a.may_manage(id).then_some((id, name)))
}

pub async fn members(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let a = signed_in!(s, headers);
    let requested = q.get("partner").map(String::as_str).unwrap_or_default();
    let (partner_id, partner_name) = match manageable(&s, &a, requested).await {
        Ok(Some(found)) => found,
        Ok(None) => return Redirect::to("/licenses").into_response(),
        Err(error) => return fail(error),
    };
    let rows = match db::members(&s.pool, partner_id).await {
        Ok(rows) => rows,
        Err(error) => return fail(error),
    };

    let body: String = rows
        .iter()
        .map(|r| {
            let hidden = if requested.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<input type="hidden" name="partner_no" value="{}">"#,
                    esc(requested)
                )
            };
            // The three actions are drawn only where they would work. A
            // button that is always refused states the rule twice and gets
            // one of them wrong.
            let self_row = r.id == a.user_id;
            let (next_status, status_label) = if r.status == "active" {
                ("disabled", "停用")
            } else {
                ("active", "啟用")
            };
            let status_action = if self_row {
                "<span class=\"fp\">本帳號</span>".to_string()
            } else {
                format!(
                    r#"<form method="post" action="/members/{id}" class="inline">{hidden}
                      <input type="hidden" name="status" value="{next_status}">
                      <button class="plain">{status_label}</button></form>"#,
                    id = r.id
                )
            };
            let admin_action = if self_row {
                String::new()
            } else {
                format!(
                    r#"<form method="post" action="/members/{id}" class="inline">{hidden}
                      <input type="hidden" name="is_admin" value="{next}">
                      <button class="plain">{label}</button></form>"#,
                    id = r.id,
                    next = if r.is_admin { "false" } else { "true" },
                    label = if r.is_admin {
                        "改為一般成員"
                    } else {
                        "設為管理員"
                    }
                )
            };
            format!(
                r#"<tr><td>{email}<br><span class="fp">{name}</span></td>
                <td><div class="cell"><span class="tag {atone}">{arole}</span>{admin_action}</div></td>
                <td><div class="cell"><span class="tag {tone}">{status}</span>{status_action}</div></td>
                <td class="r"><form method="post" action="/members/{id}" class="inline">{hidden}
                  <input type="hidden" name="reset" value="1">
                  <button class="plain">重設密碼</button></form></td></tr>"#,
                id = r.id,
                email = esc(&r.email),
                name = esc(&r.name),
                atone = if r.is_admin { "on" } else { "off" },
                arole = if r.is_admin {
                    "管理員"
                } else {
                    "一般成員"
                },
                tone = if r.status == "active" { "on" } else { "off" },
                status = if r.status == "active" {
                    "使用中"
                } else {
                    "已停用"
                },
            )
        })
        .collect();

    let created = password_card(&q);

    let scope = if requested.is_empty() {
        String::new()
    } else {
        format!(
            r#"<input type="hidden" name="partner_no" value="{}">"#,
            esc(requested)
        )
    };

    page(
        "成員",
        Some(&a),
        &format!(
            r#"<h1>成員 · {name}</h1>
            <p class="lede">管理員可以新增成員、停用帳號與重設密碼。至少保留一位使用中的管理員。</p>
            {created}
            <form class="card" method="post" action="/members">{scope}
              <div class="grid">
                <label>登入帳號<input name="email" type="email" required></label>
                <label>姓名<input name="name"></label>
                <label class="row"><input type="checkbox" name="is_admin"> 設為管理員</label>
              </div>
              <div class="actions"><button>建立成員</button></div>
            </form>
            <table><thead><tr><th>帳號</th><th>權限</th><th>狀態</th><th class="r">密碼</th></tr></thead>
            <tbody>{body}</tbody></table>"#,
            name = esc(&partner_name)
        ),
    )
    .into_response()
}

pub async fn create_member(
    State(s): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<MemberForm>,
) -> Response {
    let a = signed_in!(s, headers);
    let partner_id = match manageable(&s, &a, &form.partner_no).await {
        Ok(Some((id, _))) => id,
        Ok(None) => return Redirect::to("/licenses").into_response(),
        Err(error) => return fail(error),
    };
    let back = members_path(&form.partner_no);
    let email = form.email.trim();
    if email.is_empty() {
        return Redirect::to(&back).into_response();
    }
    let password = temporary_password();
    let hash = match auth::hash_password(&password) {
        Ok(hash) => hash,
        Err(error) => return fail(error),
    };
    match db::create_member(
        &s.pool,
        partner_id,
        email,
        form.name.trim(),
        form.is_admin.is_some(),
        &hash,
    )
    .await
    {
        Ok(true) => Redirect::to(&format!(
            "{back}{}{}",
            if back.contains('?') { "&" } else { "?" },
            password_result(&s, &headers, email, &password).await
        ))
        .into_response(),
        // The address is unique across every partner, so the account it
        // already belongs to may be one this actor cannot see. The message
        // says what to do rather than where the row is.
        Ok(false) => fail(anyhow::anyhow!(
            "{email} 已被其他帳號使用，請改用另一個地址。"
        )),
        Err(error) => fail(error),
    }
}

pub async fn update_member(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<i32>,
    Form(form): Form<MemberEditForm>,
) -> Response {
    let a = signed_in!(s, headers);
    let target = match db::member_ref(&s.pool, id).await {
        Ok(Some(target)) => target,
        Ok(None) => return Redirect::to("/licenses").into_response(),
        Err(error) => return fail(error),
    };
    if !a.may_manage(target.partner_id) {
        return Redirect::to("/licenses").into_response();
    }
    // Locking oneself out is the one mistake this page could make
    // irreversible: the account that would fix it is the one being changed.
    if id == a.user_id && (form.status.is_some() || form.is_admin.is_some()) {
        return fail(anyhow::anyhow!(
            "無法變更本帳號的權限或狀態，請由其他管理員處理。"
        ));
    }
    let back = members_path(&form.partner_no);

    if let Some(status) = form.status.as_deref() {
        let status = if status == "disabled" {
            "disabled"
        } else {
            "active"
        };
        if status == "disabled" && target.is_admin && last_admin(&s, target.partner_id).await {
            return fail(anyhow::anyhow!(
                "{} 是唯一使用中的管理員，請先指定另一位管理員。",
                target.email
            ));
        }
        if let Err(error) = db::set_member_status(&s.pool, id, status).await {
            return fail(error);
        }
    }

    if let Some(flag) = form.is_admin.as_deref() {
        let wanted = flag == "true";
        if !wanted && target.is_admin && last_admin(&s, target.partner_id).await {
            return fail(anyhow::anyhow!(
                "{} 是唯一使用中的管理員，請先指定另一位管理員。",
                target.email
            ));
        }
        if let Err(error) = db::set_member_admin(&s.pool, id, wanted).await {
            return fail(error);
        }
    }

    if form.reset.is_some() {
        let password = temporary_password();
        let hash = match auth::hash_password(&password) {
            Ok(hash) => hash,
            Err(error) => return fail(error),
        };
        if let Err(error) = db::set_member_password(&s.pool, id, &hash).await {
            return fail(error);
        }
        return Redirect::to(&format!(
            "{back}{}{}",
            if back.contains('?') { "&" } else { "?" },
            password_result(&s, &headers, &target.email, &password).await
        ))
        .into_response();
    }

    Redirect::to(&back).into_response()
}

async fn last_admin(s: &Shared, partner_id: i32) -> bool {
    db::active_admins(&s.pool, partner_id)
        .await
        .map(|n| n <= 1)
        .unwrap_or(true)
}

fn members_path(partner_no: &str) -> String {
    match partner_no.trim() {
        "" => "/members".to_string(),
        no => format!("/members?partner={}", urlencode(no)),
    }
}

#[derive(Deserialize)]
pub struct PasswordForm {
    #[serde(default)]
    current: String,
    password: String,
    confirm: String,
}

/// The change form. Reached by choice, and by every other page while the
/// stored password is one this server generated.
fn password_page(a: &Actor, note: &str) -> Response {
    let first = a.must_change_password;
    // Nothing to say on the voluntary path: the heading and the three fields
    // are the whole of it.
    let lede = if first {
        r#"<p class="lede">請先設定新密碼，才能使用其他功能。</p>"#
    } else {
        ""
    };
    // Asking for a password the account presented one screen ago adds a step
    // and no assurance; asking for it later is what stops a borrowed session
    // from changing the password on the way out.
    let current = if first {
        String::new()
    } else {
        r#"<label>目前密碼<input name="current" type="password"
             autocomplete="current-password" required></label>"#
            .to_string()
    };
    let message = if note.is_empty() {
        String::new()
    } else {
        format!(r#"<p class="lede">{}</p>"#, esc(note))
    };
    page(
        "變更密碼",
        if first { None } else { Some(a) },
        &format!(
            r#"<h1>變更密碼</h1>{lede}{message}
            <form class="card" method="post" action="/password" style="max-width:420px">
              <div class="grid" style="grid-template-columns:1fr">
                {current}
                <label>新密碼<input name="password" type="password"
                  autocomplete="new-password" minlength="{min}" required></label>
                <label>再輸入一次<input name="confirm" type="password"
                  autocomplete="new-password" minlength="{min}" required></label>
              </div>
              <div class="actions"><button>設定密碼</button></div>
            </form>"#,
            min = auth::MIN_PASSWORD_CHARS
        ),
    )
    .into_response()
}

pub async fn password_form(State(s): State<Shared>, headers: HeaderMap) -> Response {
    match actor(&s, &headers).await {
        Some(a) => password_page(&a, ""),
        None => Redirect::to("/login").into_response(),
    }
}

pub async fn change_password(
    State(s): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> Response {
    // Not `signed_in!`: that macro sends every request here, so this handler
    // is the one place that has to resolve the actor itself.
    let Some(a) = actor(&s, &headers).await else {
        return Redirect::to("/login").into_response();
    };
    if !a.must_change_password {
        match auth::password_is(&s.pool, a.user_id, &form.current).await {
            Ok(true) => {}
            Ok(false) => return password_page(&a, "目前密碼不正確。"),
            Err(error) => return fail(error),
        }
    }
    if form.password.chars().count() < auth::MIN_PASSWORD_CHARS {
        return password_page(
            &a,
            &format!("新密碼至少 {} 個字元。", auth::MIN_PASSWORD_CHARS),
        );
    }
    if form.password != form.confirm {
        return password_page(&a, "兩次輸入的新密碼不一致。");
    }
    let hash = match auth::hash_password(&form.password) {
        Ok(hash) => hash,
        Err(error) => return fail(error),
    };
    if let Err(error) = db::set_own_password(&s.pool, a.user_id, &hash).await {
        return fail(error);
    }
    Redirect::to("/licenses").into_response()
}

/// The absolute address of this console, taken from the request rather than
/// from a setting: the same reason the session cookie reads its Secure flag
/// off the proxy's header.
fn public_base(headers: &HeaderMap) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    let scheme = if auth::secure(headers) {
        "https"
    } else {
        "http"
    };
    format!("{scheme}://{host}")
}

/// Delivers a generated password and reports how, as the query the redirect
/// carries. Mail that does not go out is not an error worth failing the whole
/// operation over: the account exists either way, and the password still has
/// to reach somebody, so it falls back to the screen.
async fn password_result(s: &Shared, headers: &HeaderMap, email: &str, password: &str) -> String {
    let mailed = match s.mailer.as_ref() {
        None => false,
        Some(mailer) => match mailer
            .send_password(email, password, &public_base(headers))
            .await
        {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "mailing the password failed");
                false
            }
        },
    };
    if mailed {
        format!("email={}&mailed=1", urlencode(email))
    } else {
        format!(
            "email={}&password={}",
            urlencode(email),
            urlencode(password)
        )
    }
}

/// The card that reports a newly generated password, on both the partner and
/// the member page.
fn password_card(q: &HashMap<String, String>) -> String {
    let Some(email) = q.get("email") else {
        return String::new();
    };
    if q.contains_key("mailed") {
        return format!(
            r#"<div class="key"><strong>密碼已寄出</strong><code>{}</code>
            <p class="note">首次登入後請設定新密碼。</p></div>"#,
            esc(email)
        );
    }
    match q.get("password") {
        Some(password) if !password.is_empty() => format!(
            r#"<div class="key"><strong>密碼僅顯示一次</strong>
            <code>{}</code><code class="num">{}</code>
            <p class="note">離開本頁之後無法再顯示。首次登入後請設定新密碼。</p></div>"#,
            esc(email),
            esc(password)
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_company_name_cannot_carry_markup_onto_the_page() {
        let hostile = r#"<script>alert('x')</script>"#;
        let out = esc(hostile);
        assert!(!out.contains('<'), "跳脫後不得留下標籤");
        assert!(out.contains("&lt;script&gt;"));
        assert_eq!(esc("正常公司 & 有限公司"), "正常公司 &amp; 有限公司");
    }

    #[test]
    fn the_status_column_never_says_active_next_to_a_past_date() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
        let past = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let future = NaiveDate::from_ymd_opt(2027, 9, 1).unwrap();
        assert_eq!(status_label("active", future, today).0, "使用中");
        assert_eq!(status_label("active", past, today).0, "已到期");
        assert_eq!(status_label("issued", future, today).0, "尚未啟用");
        // A revoked licence reads as revoked whatever the date says.
        assert_eq!(status_label("revoked", future, today).0, "已註銷");
        assert_eq!(status_label("suspended", past, today).0, "已暫停");
    }

    #[test]
    fn a_token_survives_the_round_trip_through_a_query_string() {
        let token = "abc-DEF_123.xyz~";
        assert_eq!(urlencode(token), "abc-DEF_123.xyz~");
        assert_eq!(urlencode("L2609-001?&="), "L2609-001%3F%26%3D");
    }
}
