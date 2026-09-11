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
    /// May add, suspend and reset the other members of their own partner.
    /// A vendor admin is one everywhere; see `may_manage`.
    pub is_admin: bool,
    /// The password was generated rather than chosen. Every page redirects to
    /// the change form until it is cleared.
    pub must_change_password: bool,
}

impl Actor {
    /// Whether this actor may manage the members of `partner_id`. Only admins
    /// manage: a vendor admin every partner, a reseller admin their own. A
    /// plain member manages nobody -- including themselves -- and that holds
    /// in the vendor's own partner too, where every member is `is_vendor`.
    pub fn may_manage(&self, partner_id: i32) -> bool {
        self.is_admin && (self.is_vendor || self.partner_id == partner_id)
    }

    /// The partners whose licences this actor may see: their own only. The
    /// vendor sees all, which is the empty filter.
    ///
    /// Written into the statement rather than bound, because it is a whole
    /// predicate rather than a value. What it interpolates is an `i32` read
    /// back out of the session row, so there is no string that could carry a
    /// quote; a partner id that ever becomes text has to become a bind.
    pub fn scope_sql(&self) -> String {
        if self.is_vendor {
            "TRUE".into()
        } else {
            format!("l.partner_id = {id}", id = self.partner_id)
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
        "SELECT u.id, u.name, u.email, u.partner_id, u.is_vendor, u.is_admin, u.must_change_password,
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
        is_admin: r.get("is_admin"),
        must_change_password: r.get("must_change_password"),
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
        conflicts: r.get("conflicts"),
        denials: r.get("denials"),
    }
}

const LICENSE_SELECT: &str = "SELECT l.license_no, l.licensee, l.licensee_tax_id,
            p.name AS partner_name, p.partner_no, l.key_prefix, l.expires_on,
            l.status, l.note, l.fingerprint, l.bound_at,
            l.last_seen_at, l.version, l.conflicts,
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

/// One term sold, as the monthly statement lists it. The partner columns
/// repeat down the rows; the page groups on them rather than the query, so
/// that a partner's subtotal and the lines behind it come from one read and
/// cannot disagree.
pub struct BillingRow {
    pub partner_no: String,
    pub partner_name: String,
    pub tax_id: String,
    pub license_no: String,
    pub licensee: String,
    pub from_on: NaiveDate,
    pub to_on: NaiveDate,
    /// `numeric(12, 0)`, so it arrives whole and adds up without a decimal
    /// type on this side.
    pub amount: i64,
    /// The first term of its licence. Derived rather than stored: a licence
    /// is issued once and renewed after that, and the row order says which.
    pub first_term: bool,
}

/// Every term invoiced in `month`, oldest partner name first.
///
/// Grouped by `billed_on` rather than `from_on`: a term sold in advance is
/// invoiced in the month it was sold, which is the month the vendor is
/// billing the partner for.
pub async fn billing(pool: &PgPool, month: NaiveDate) -> Result<Vec<BillingRow>> {
    let rows = sqlx::query(
        "SELECT p.partner_no, p.name AS partner_name, p.tax_id,
                l.license_no, l.licensee, t.from_on, t.to_on, t.amount::bigint AS amount,
                t.id = (SELECT min(t2.id) FROM license_term t2
                         WHERE t2.license_id = t.license_id) AS first_term
           FROM license_term t
           JOIN license l ON l.id = t.license_id
           JOIN partner p ON p.id = l.partner_id
          WHERE t.billed_on >= $1 AND t.billed_on < ($1 + interval '1 month')::date
          ORDER BY p.name, p.partner_no, l.license_no, t.from_on",
    )
    .bind(month)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| BillingRow {
            partner_no: r.get("partner_no"),
            partner_name: r.get("partner_name"),
            tax_id: r.get("tax_id"),
            license_no: r.get("license_no"),
            licensee: r.get("licensee"),
            from_on: r.get("from_on"),
            to_on: r.get("to_on"),
            amount: r.get("amount"),
            first_term: r.get("first_term"),
        })
        .collect())
}

/// The months that have anything to invoice, newest first. The picker offers
/// these and nothing else: a month with no sales is not a month to open.
pub async fn billing_months(pool: &PgPool) -> Result<Vec<NaiveDate>> {
    let rows = sqlx::query(
        "SELECT DISTINCT date_trunc('month', billed_on)::date AS month
           FROM license_term ORDER BY 1 DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(|r| r.get("month")).collect())
}

pub struct PartnerRow {
    pub partner_no: String,
    pub name: String,
    pub tax_id: String,
    pub unit_price: String,
    pub status: String,
    pub licenses: i64,
    pub users: i64,
}

pub async fn partners(pool: &PgPool) -> Result<Vec<PartnerRow>> {
    let rows = sqlx::query(
        "SELECT p.partner_no, p.name, p.tax_id,
                p.unit_price::text AS unit_price, p.status,
                (SELECT count(*) FROM license l WHERE l.partner_id = p.id) AS licenses,
                (SELECT count(*) FROM partner_user u WHERE u.partner_id = p.id) AS users
           FROM partner p
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
            unit_price: r.get("unit_price"),
            status: r.get("status"),
            licenses: r.get("licenses"),
            users: r.get("users"),
        })
        .collect())
}

/// One row of the member list.
pub struct MemberRow {
    pub id: i32,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
    pub status: String,
}

/// Where a member sits. The partner id is what `Actor::may_manage` is asked
/// about, so a member of another partner is refused by the same check that
/// refuses a member who does not exist.
pub struct MemberRef {
    pub partner_id: i32,
    pub is_admin: bool,
    pub email: String,
}

pub async fn partner_by_no(pool: &PgPool, no: &str) -> Result<Option<(i32, String)>> {
    Ok(
        sqlx::query("SELECT id, name FROM partner WHERE partner_no = $1")
            .bind(no)
            .fetch_optional(pool)
            .await
            .context("讀取經銷商失敗")?
            .map(|r| (r.get("id"), r.get("name"))),
    )
}

pub async fn members(pool: &PgPool, partner_id: i32) -> Result<Vec<MemberRow>> {
    Ok(sqlx::query(
        "SELECT id, email, name, is_admin, status FROM partner_user
          WHERE partner_id = $1 ORDER BY is_admin DESC, id",
    )
    .bind(partner_id)
    .fetch_all(pool)
    .await
    .context("讀取成員失敗")?
    .into_iter()
    .map(|r| MemberRow {
        id: r.get("id"),
        email: r.get("email"),
        name: r.get("name"),
        is_admin: r.get("is_admin"),
        status: r.get("status"),
    })
    .collect())
}

pub async fn member_ref(pool: &PgPool, id: i32) -> Result<Option<MemberRef>> {
    Ok(
        sqlx::query("SELECT partner_id, is_admin, email FROM partner_user WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("讀取成員失敗")?
            .map(|r| MemberRef {
                partner_id: r.get("partner_id"),
                is_admin: r.get("is_admin"),
                email: r.get("email"),
            }),
    )
}

/// Active admins of one partner. Suspending or demoting the last one would
/// leave nobody who can add a member or reset a password, and no operation
/// here hands the flag back. Only `is_admin` counts, as in `may_manage`: a
/// plain member of the vendor's partner is `is_vendor` and manages nobody.
pub async fn active_admins(pool: &PgPool, partner_id: i32) -> Result<i64> {
    Ok(sqlx::query(
        "SELECT count(*) AS n FROM partner_user
          WHERE partner_id = $1 AND is_admin AND status = 'active'",
    )
    .bind(partner_id)
    .fetch_one(pool)
    .await
    .context("清點管理員失敗")?
    .get("n"))
}

/// `false` when the address already belongs to an account. The column is
/// unique across every partner, so this is not a per-partner check.
///
/// `is_vendor` is inherited from the partner rather than passed in: a member
/// added to the vendor's own partner is vendor staff, and anyone added to a
/// reseller is not. Deriving it from the row keeps `P-000` out of the code,
/// where it is only written by `create-vendor`.
pub async fn create_member(
    pool: &PgPool,
    partner_id: i32,
    email: &str,
    name: &str,
    is_admin: bool,
    password_hash: &str,
) -> Result<bool> {
    let done = sqlx::query(
        "INSERT INTO partner_user
             (partner_id, email, password_hash, name, is_admin, is_vendor, must_change_password)
         SELECT $1, lower($2), $3, $4, $5,
                EXISTS (SELECT 1 FROM partner_user v WHERE v.partner_id = $1 AND v.is_vendor),
                true
         ON CONFLICT (email) DO NOTHING",
    )
    .bind(partner_id)
    .bind(email)
    .bind(password_hash)
    .bind(name)
    .bind(is_admin)
    .execute(pool)
    .await
    .context("建立成員失敗")?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_member_status(pool: &PgPool, id: i32, status: &str) -> Result<()> {
    // Sessions are not deleted: `actor_of` joins on `status = 'active'`, so a
    // suspension takes effect on the next request either way.
    sqlx::query("UPDATE partner_user SET status = $2 WHERE id = $1")
        .bind(id)
        .bind(status)
        .execute(pool)
        .await
        .context("變更成員狀態失敗")?;
    Ok(())
}

pub async fn set_member_admin(pool: &PgPool, id: i32, is_admin: bool) -> Result<()> {
    sqlx::query("UPDATE partner_user SET is_admin = $2 WHERE id = $1")
        .bind(id)
        .bind(is_admin)
        .execute(pool)
        .await
        .context("變更成員權限失敗")?;
    Ok(())
}

/// Sets the password and signs the account out everywhere, in one statement
/// pair: a password that is changed while the old sessions stay live is not
/// changed.
pub async fn set_member_password(pool: &PgPool, id: i32, password_hash: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE partner_user SET password_hash = $2, must_change_password = true WHERE id = $1",
    )
    .bind(id)
    .bind(password_hash)
    .execute(&mut *tx)
    .await
    .context("重設密碼失敗")?;
    sqlx::query("DELETE FROM partner_session WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("撤銷登入工作階段失敗")?;
    tx.commit().await?;
    Ok(())
}

pub async fn partner(pool: &PgPool, no: &str) -> Result<Option<PartnerRow>> {
    Ok(sqlx::query(
        "SELECT p.partner_no, p.name, p.tax_id,
                p.unit_price::text AS unit_price, p.status,
                (SELECT count(*) FROM license l WHERE l.partner_id = p.id) AS licenses,
                (SELECT count(*) FROM partner_user u WHERE u.partner_id = p.id) AS users
           FROM partner p WHERE p.partner_no = $1",
    )
    .bind(no)
    .fetch_optional(pool)
    .await
    .context("讀取經銷商失敗")?
    .map(|r| PartnerRow {
        partner_no: r.get("partner_no"),
        name: r.get("name"),
        tax_id: r.get("tax_id"),
        unit_price: r.get("unit_price"),
        status: r.get("status"),
        licenses: r.get("licenses"),
        users: r.get("users"),
    }))
}

/// The account changing its own password. Sessions are left alone: the
/// caller has just proved they hold the old password, and signing them out
/// of the browser they are typing in would be a strange reward for it.
pub async fn set_own_password(pool: &PgPool, id: i32, password_hash: &str) -> Result<()> {
    sqlx::query(
        "UPDATE partner_user SET password_hash = $2, must_change_password = false WHERE id = $1",
    )
    .bind(id)
    .bind(password_hash)
    .execute(pool)
    .await
    .context("變更密碼失敗")?;
    Ok(())
}

/// Where the statement is posted: the address, and the secret a signed Lark
/// bot checks it against (empty when the bot does not verify).
pub struct Webhook {
    pub url: String,
    pub secret: String,
}

/// The single `vendor_setting` row: the statement address and the last month
/// announced to it.
pub async fn vendor_setting(pool: &PgPool) -> Result<(Webhook, Option<NaiveDate>)> {
    let row = sqlx::query(
        "SELECT statement_webhook_url, statement_webhook_secret, notified_month
           FROM vendor_setting WHERE id = 1",
    )
    .fetch_one(pool)
    .await
    .context("讀取原廠設定失敗")?;
    Ok((
        Webhook {
            url: row.get("statement_webhook_url"),
            secret: row.get("statement_webhook_secret"),
        },
        row.get("notified_month"),
    ))
}

/// Store the statement address, and take the month before `today` as already
/// announced: an address saved in the middle of a month starts announcing at
/// the end of that month rather than replaying whatever came before it.
pub async fn set_statement_webhook(pool: &PgPool, hook: &Webhook, today: NaiveDate) -> Result<()> {
    sqlx::query(
        "UPDATE vendor_setting
            SET statement_webhook_url = $1, statement_webhook_secret = $2, updated_at = now(),
                notified_month = (date_trunc('month', $3::date) - interval '1 month')::date
          WHERE id = 1",
    )
    .bind(&hook.url)
    .bind(&hook.secret)
    .bind(today)
    .execute(pool)
    .await
    .context("儲存推送網址失敗")?;
    Ok(())
}

/// Claim `month` as the one to announce, returning the address to announce it
/// to. `None` when it is already claimed or no address is set.
///
/// The claim is the UPDATE itself, so two servers against one database cannot
/// both send: the second matches no row. Delivery happens after the claim and
/// hands the month back on failure, which is the pairing that neither sends
/// twice nor drops a month for good.
pub async fn claim_statement_month(pool: &PgPool, month: NaiveDate) -> Result<Option<Webhook>> {
    let row = sqlx::query(
        "UPDATE vendor_setting SET notified_month = $1
          WHERE id = 1 AND statement_webhook_url <> ''
            AND (notified_month IS NULL OR notified_month < $1)
      RETURNING statement_webhook_url, statement_webhook_secret",
    )
    .bind(month)
    .fetch_optional(pool)
    .await
    .context("認領月結推送失敗")?;
    Ok(row.map(|r| Webhook {
        url: r.get("statement_webhook_url"),
        secret: r.get("statement_webhook_secret"),
    }))
}

/// Hand a claimed month back after a failed delivery, so the next tick tries
/// it again. Guarded on the value this task claimed, so a save that happened
/// meanwhile is not overwritten.
pub async fn release_statement_month(
    pool: &PgPool,
    month: NaiveDate,
    previous: Option<NaiveDate>,
) -> Result<()> {
    sqlx::query(
        "UPDATE vendor_setting SET notified_month = $2 WHERE id = 1 AND notified_month = $1",
    )
    .bind(month)
    .bind(previous)
    .execute(pool)
    .await
    .context("退回月結推送失敗")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Actor;

    fn actor(partner_id: i32, is_vendor: bool, is_admin: bool) -> Actor {
        Actor {
            user_id: 1,
            name: String::new(),
            email: String::new(),
            partner_id,
            partner_no: String::new(),
            partner_name: String::new(),
            is_vendor,
            is_admin,
            must_change_password: false,
        }
    }

    #[test]
    fn an_admin_manages_only_their_own_partner() {
        assert!(
            actor(1, true, true).may_manage(9),
            "vendor admin: every partner"
        );
        assert!(
            !actor(1, true, false).may_manage(1),
            "vendor member: nobody"
        );
        assert!(
            !actor(1, true, false).may_manage(9),
            "vendor member: nobody"
        );
        assert!(actor(1, false, true).may_manage(1), "admin: their own");
        assert!(!actor(1, false, true).may_manage(9), "admin: not another's");
        assert!(!actor(1, false, false).may_manage(1), "member: nobody");
    }
}
