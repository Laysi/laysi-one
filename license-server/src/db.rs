//! Connection pool, schema and the queries shared by more than one route.
//!
//! Queries are run through `sqlx::query` rather than the macros, so a build
//! needs no database. The schema is one idempotent file applied at start-up:
//! this server owns its own tables and nothing else writes them, so a
//! migration runner would be a mechanism with one user.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

/// The tables, applied on every boot. See the file for why it is idempotent.
const SCHEMA: &str = include_str!("../schema.sql");

pub async fn connect(url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(url)
        .await
        .context("connecting to PostgreSQL")?;
    // `IF NOT EXISTS` throughout, so this is a no-op against an existing
    // database and there is no version to keep in step. `raw_sql` takes the
    // whole file, which is also what keeps the statements from having to be
    // split apart by a rule that a semicolon inside a string would break.
    sqlx::raw_sql(SCHEMA)
        .execute(&pool)
        .await
        .context("applying the schema")?;
    Ok(pool)
}

/// Who is signed in to the console.
#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: i32,
    pub name: String,
    pub email: String,
    pub partner_id: i32,
    pub partner_no: String,
    pub partner_name: String,
    /// The vendor's own staff. Sees every partner, and is the only one that
    /// can create them.
    pub is_vendor: bool,
}

impl Actor {
    /// The partners whose licences this actor may see: their own, plus anyone
    /// they signed. The vendor sees all, which is the empty filter.
    ///
    /// Written into the statement rather than bound, because it is a whole
    /// predicate rather than a value. What it interpolates is an `i32` read
    /// back out of the session row, so there is no string that could carry a
    /// quote; a partner id that ever becomes text has to become a bind.
    pub fn scope_sql(&self) -> String {
        if self.is_vendor {
            "TRUE".into()
        } else {
            format!(
                "(l.partner_id = {id} OR p.parent_id = {id})",
                id = self.partner_id
            )
        }
    }
}

pub fn hash_token(raw: &str) -> String {
    blake3::hash(raw.as_bytes()).to_hex().to_string()
}

/// Resolve a session cookie to its actor, or `None` for anything that is not
/// a live session: absent, unknown, expired, or a disabled account.
pub async fn actor_of(pool: &PgPool, token: &str) -> Result<Option<Actor>> {
    let row = sqlx::query(
        "SELECT u.id, u.name, u.email, u.partner_id, u.is_vendor,
                p.partner_no, p.name AS partner_name
           FROM partner_session s
           JOIN partner_user u ON u.id = s.user_id
           JOIN partner p ON p.id = u.partner_id
          WHERE s.token_hash = $1 AND s.expires_at > now()
            AND u.status = 'active' AND p.status = 'active'",
    )
    .bind(hash_token(token))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Actor {
        user_id: r.get("id"),
        name: r.get("name"),
        email: r.get("email"),
        partner_id: r.get("partner_id"),
        partner_no: r.get("partner_no"),
        partner_name: r.get("partner_name"),
        is_vendor: r.get("is_vendor"),
    }))
}

/// One row of the licence list.
pub struct LicenseRow {
    pub license_no: String,
    pub licensee: String,
    pub licensee_tax_id: String,
    pub partner_name: String,
    pub partner_no: String,
    pub key_prefix: String,
    pub expires_on: NaiveDate,
    pub status: String,
    pub note: String,
    pub fingerprint: Option<String>,
    pub bound_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub version: String,
    pub employee_count: i32,
    pub conflicts: i32,
    pub denials: i64,
}

fn license_row(r: &sqlx::postgres::PgRow) -> LicenseRow {
    LicenseRow {
        license_no: r.get("license_no"),
        licensee: r.get("licensee"),
        licensee_tax_id: r.get("licensee_tax_id"),
        partner_name: r.get("partner_name"),
        partner_no: r.get("partner_no"),
        key_prefix: r.get("key_prefix"),
        expires_on: r.get("expires_on"),
        status: r.get("status"),
        note: r.get("note"),
        fingerprint: r.get("fingerprint"),
        bound_at: r.get("bound_at"),
        last_seen_at: r.get("last_seen_at"),
        version: r.get("version"),
        employee_count: r.get("employee_count"),
        conflicts: r.get("conflicts"),
        denials: r.get("denials"),
    }
}

const LICENSE_SELECT: &str = "SELECT l.license_no, l.licensee, l.licensee_tax_id,
            p.name AS partner_name, p.partner_no, l.key_prefix, l.expires_on,
            l.status, l.note, l.fingerprint, l.bound_at,
            l.last_seen_at, l.version, l.employee_count, l.conflicts,
            (SELECT count(*) FROM license_denial d WHERE d.license_id = l.id) AS denials
       FROM license l JOIN partner p ON p.id = l.partner_id";

pub async fn licenses(pool: &PgPool, actor: &Actor, search: &str) -> Result<Vec<LicenseRow>> {
    let sql = format!(
        "{LICENSE_SELECT} WHERE {} AND ($1 = '' OR l.licensee ILIKE '%' || $1 || '%'
             OR l.license_no ILIKE '%' || $1 || '%' OR l.licensee_tax_id ILIKE '%' || $1 || '%')
         ORDER BY l.issued_at DESC LIMIT 300",
        actor.scope_sql()
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(search)
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(license_row).collect())
}

pub async fn license(pool: &PgPool, actor: &Actor, no: &str) -> Result<Option<LicenseRow>> {
    let sql = format!(
        "{LICENSE_SELECT} WHERE {} AND l.license_no = $1",
        actor.scope_sql()
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(no)
        .fetch_optional(pool)
        .await?;
    Ok(row.as_ref().map(license_row))
}

/// Next licence number for this month: `L{yy}{mm}-{nnn}`.
///
/// Keyed on the period as well as the prefix, which is the same lesson the
/// product's own document numbers carry: a counter keyed on the prefix alone
/// hands out a number that is already taken as soon as anyone works a month
/// out of order.
pub async fn next_license_no(tx: &mut sqlx::PgConnection, today: NaiveDate) -> Result<String> {
    let prefix = format!("L{}", today.format("%y%m"));
    let row = sqlx::query(
        "SELECT license_no FROM license WHERE license_no LIKE $1 || '-%'
          ORDER BY license_no DESC LIMIT 1",
    )
    .bind(&prefix)
    .fetch_optional(&mut *tx)
    .await?;
    let next = row
        .and_then(|r| {
            r.get::<String, _>("license_no")
                .rsplit_once('-')
                .and_then(|(_, n)| n.parse::<u32>().ok())
        })
        .unwrap_or(0)
        + 1;
    Ok(format!("{prefix}-{next:03}"))
}

pub struct TermRow {
    pub from_on: NaiveDate,
    pub to_on: NaiveDate,
    pub billed_on: NaiveDate,
    pub amount: String,
}

pub async fn terms(pool: &PgPool, license_no: &str) -> Result<Vec<TermRow>> {
    let rows = sqlx::query(
        "SELECT t.from_on, t.to_on, t.billed_on, t.amount::text AS amount
           FROM license_term t JOIN license l ON l.id = t.license_id
          WHERE l.license_no = $1 ORDER BY t.from_on DESC",
    )
    .bind(license_no)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| TermRow {
            from_on: r.get("from_on"),
            to_on: r.get("to_on"),
            billed_on: r.get("billed_on"),
            amount: r.get("amount"),
        })
        .collect())
}

pub struct DenialRow {
    pub at: DateTime<Utc>,
    pub fingerprint: String,
    pub ip: String,
    pub reason: String,
}

pub async fn denials(pool: &PgPool, license_no: &str) -> Result<Vec<DenialRow>> {
    let rows = sqlx::query(
        "SELECT d.at, d.fingerprint, d.ip, d.reason
           FROM license_denial d JOIN license l ON l.id = d.license_id
          WHERE l.license_no = $1 ORDER BY d.at DESC LIMIT 50",
    )
    .bind(license_no)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| DenialRow {
            at: r.get("at"),
            fingerprint: r.get("fingerprint"),
            ip: r.get("ip"),
            reason: r.get("reason"),
        })
        .collect())
}

pub struct BillingRow {
    pub month: String,
    pub partner_no: String,
    pub partner_name: String,
    pub terms: i64,
    pub amount: String,
}

/// Sold by month and by partner. Reads `license_term`, so a renewal counts
/// the same as a first sale, which is what is being invoiced.
pub async fn billing(pool: &PgPool, actor: &Actor) -> Result<Vec<BillingRow>> {
    let scope = if actor.is_vendor {
        "TRUE".to_string()
    } else {
        format!(
            "(l.partner_id = {id} OR p.parent_id = {id})",
            id = actor.partner_id
        )
    };
    let sql = format!(
        "SELECT to_char(t.billed_on, 'YYYY-MM') AS month, p.partner_no, p.name AS partner_name,
                count(*) AS terms, sum(t.amount)::text AS amount
           FROM license_term t
           JOIN license l ON l.id = t.license_id
           JOIN partner p ON p.id = l.partner_id
          WHERE {scope}
          GROUP BY 1, 2, 3 ORDER BY 1 DESC, 3"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .map(|r| BillingRow {
            month: r.get("month"),
            partner_no: r.get("partner_no"),
            partner_name: r.get("partner_name"),
            terms: r.get("terms"),
            amount: r.get("amount"),
        })
        .collect())
}

pub struct PartnerRow {
    pub partner_no: String,
    pub name: String,
    pub tax_id: String,
    pub parent_name: Option<String>,
    pub unit_price: String,
    pub status: String,
    pub licenses: i64,
    pub users: i64,
}

pub async fn partners(pool: &PgPool) -> Result<Vec<PartnerRow>> {
    let rows = sqlx::query(
        "SELECT p.partner_no, p.name, p.tax_id, parent.name AS parent_name,
                p.unit_price::text AS unit_price, p.status,
                (SELECT count(*) FROM license l WHERE l.partner_id = p.id) AS licenses,
                (SELECT count(*) FROM partner_user u WHERE u.partner_id = p.id) AS users
           FROM partner p LEFT JOIN partner parent ON parent.id = p.parent_id
          ORDER BY p.partner_no",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| PartnerRow {
            partner_no: r.get("partner_no"),
            name: r.get("name"),
            tax_id: r.get("tax_id"),
            parent_name: r.get("parent_name"),
            unit_price: r.get("unit_price"),
            status: r.get("status"),
            licenses: r.get("licenses"),
            users: r.get("users"),
        })
        .collect())
}
