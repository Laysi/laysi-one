//! Storage layer: the PostgreSQL repository.
//!
//! This layer only moves data in and out; it makes no business decisions.
//! Balance is enforced by the database's deferred trigger and by
//! [`laysi_domain::validate_entry`], and regulation values come from
//! `laysi-regulation`. The repository must not become a third opinion.
//!
//! Queries use the runtime `sqlx::query` rather than the `query!` macro:
//! the project does not depend on a compile-time database connection, so
//! `cargo build` must work on a machine without Postgres.

use std::collections::HashSet;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use laysi_domain::{LineInput, Money, TrialBalanceRow};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub mod password;
pub mod storage;

pub use storage::{ObjectStore, StorageError};

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("資料庫錯誤：{0}")]
    Db(#[from] sqlx::Error),

    #[error("migration 失敗：{0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("找不到傳票 {no}")]
    VoucherNotFound { no: String },

    #[error("傳票 {no} 目前是「{status}」，不能過帳")]
    NotPostable { no: String, status: String },

    #[error("傳票 {no} 目前是「{status}」，只有已過帳傳票可以作廢")]
    NotVoidable { no: String, status: String },

    #[error("傳票 {no} 目前是「{status}」，只有已過帳傳票可以沖銷")]
    NotReversible { no: String, status: String },

    #[error("傳票 {no} 已經被 {reversed_by} 沖銷，不能重複沖銷")]
    AlreadyReversed { no: String, reversed_by: String },

    #[error("科目 {code} 不存在，或不是可入帳科目")]
    UnknownAccount { code: String },

    #[error("找不到科目 {code}")]
    AccountNotFound { code: String },

    #[error("找不到父科目 {code}")]
    ParentAccountNotFound { code: String },

    #[error("科目 {code} 不能把自己或自己的子科目設成父科目")]
    AccountParentCycle { code: String },

    #[error("科目 {code} 已有子科目，父科目必須維持不可入帳")]
    AccountHasChildren { code: String },

    #[error("科目 {code} 不是啟用中的可入帳資產科目，不能綁定銀行帳戶")]
    InvalidBankGlAccount { code: String },

    #[error("找不到銀行帳戶 #{id}")]
    BankAccountNotFound { id: i32 },

    #[error("找不到發票 {no}")]
    InvoiceNotFound { no: String },

    #[error("找不到應付帳單 {no}")]
    BillNotFound { no: String },

    /// The subsidiary ledger recognises posted vouchers only. While the
    /// source voucher is unposted the entry has not reached the ledger, and
    /// settling it now would put the subsidiary ledger ahead.
    #[error("{no} 的來源傳票 {voucher} 還沒過帳，先過帳才能登記收付")]
    SourceNotPosted { no: String, voucher: String },

    #[error("{no} 已經沒有未結餘額了")]
    NothingLeft { no: String },

    #[error("{no} 只剩 {left} 元未結，登記 {amount} 元會超收")]
    AmountOutOfRange { no: String, left: Money, amount: Money },

    #[error("一張都沒勾，沒有付款單可以送")]
    NothingSelected,

    #[error("找不到銀行交易 #{id}")]
    BankTxnNotFound { id: i32 },

    #[error("銀行交易 #{id} 已經核銷過了，不能再核銷一次")]
    AlreadyMatched { id: i32 },

    #[error("傳票 {no} 已經被銀行交易 #{txn_id} 認領了")]
    VoucherTaken { no: String, txn_id: i32 },

    #[error("傳票 {no} 已作廢，不能拿來核銷")]
    VoucherVoided { no: String },

    #[error("找不到字軌 {track}")]
    TrackNotFound { track: String },

    #[error("字軌 {track} 的號碼用完了（配號到 {range_to}），要先向國稅局申請配號")]
    TrackExhausted { track: String, range_to: i32 },

    #[error("找不到電子發票 {no}")]
    EInvoiceNotFound { no: String },

    #[error("{no} 目前是「{status}」，只有上傳失敗的才需要重送")]
    NotRetryable { no: String, status: String },

    #[error("{period} 的折舊已經提過了（傳票 {voucher}），同一期不能提兩次")]
    PeriodAlreadyDepreciated { period: String, voucher: String },

    #[error("這一期沒有需要提列折舊的資產")]
    NothingToDepreciate,

    #[error("沒有「{code}」這項設定")]
    SettingNotFound { code: String },

    #[error("沒有「{code}」這個維度；這個操作只新增維度值")]
    DimensionNotFound { code: String },

    #[error("找不到簽核流程第 {step_no} 關")]
    ApprovalRuleNotFound { step_no: i32 },

    #[error("簽核流程至少要留一關，不能刪除最後一關")]
    LastApprovalRule,

    #[error("沒有「{code}」這個角色")]
    RoleNotFound { code: String },

    #[error("「{code}」不能在此處變更，要重跑期初")]
    SettingNotEditable { code: String },

    #[error("沒有這個角色或權限：{role} / {permission}")]
    UnknownRoleOrPermission { role: String, permission: String },

    #[error("找不到 Token #{id}")]
    TokenNotFound { id: i32 },

    #[error("找不到使用者帳號 #{id}")]
    UserNotFound { id: i32 },

    #[error("使用者帳號 #{id} 不存在或目前不能登入")]
    UserNotActive { id: i32 },

    #[error("找不到報支單 {no}")]
    ClaimNotFound { no: String },

    #[error("報支單 {no} 目前是「{status}」，這一關已經走完了")]
    ClaimNotPending { no: String, status: String },

    #[error("報支單 {no} 的第 {step} 關已經簽過了")]
    StepAlreadyDecided { no: String, step: i32 },

    #[error("沒有「{code}」這個部門")]
    DepartmentNotFound { code: String },

    #[error("沒有「{code}」這個預算版本")]
    BudgetVersionNotFound { code: String },

    #[error("找不到文件 {no}")]
    DocumentNotFound { no: String },

    #[error("文件 {no} 沒有第 {version} 版")]
    DocumentVersionNotFound { no: String, version: i32 },

    #[error("沒有「{code}」這個匯入來源")]
    ImportSourceNotFound { code: String },

    #[error("找不到匯入批次 {no}")]
    ImportBatchNotFound { no: String },

    #[error("匯入批次 {no} 目前是「{status}」，只有預覽中的批次可以確認匯入")]
    ImportNotPreview { no: String, status: String },

    #[error("匯入批次 {no} 目前是「{status}」，只有已匯入的批次可以回滾")]
    ImportNotCommitted { no: String, status: String },

    #[error("匯入批次 {no} 有 {count} 張傳票已經過帳（{vouchers}），不能回滾。已過帳的更正只能開反向分錄。")]
    ImportHasPostedVouchers { no: String, count: usize, vouchers: String },

    #[error("「{name}」已經撤銷過了")]
    TokenAlreadyRevoked { name: String },

    #[error("沒有這位員工：{no}")]
    EmployeeNotFound { no: String },

    #[error("找不到異動單 {no}")]
    MovementNotFound { no: String },

    #[error("「{kind}」的流程只有 {total} 關，沒有第 {step_no} 關")]
    NoSuchFlowStep { kind: String, step_no: i32, total: i64 },

    #[error("異動單 {no} 的第 {step} 關已經完成過了")]
    MovementStepAlreadyDone { no: String, step: i32 },

    #[error("找不到休假單 {no}")]
    LeaveNotFound { no: String },

    #[error("{date} 已經打過{kind}卡（{at}），一天只記一次；如需更正請由人資補登")]
    PunchAlreadyRecorded { date: String, kind: String, at: String },

    #[error("{date} 還沒有上班卡，不能只打下班卡")]
    PunchOutWithoutIn { date: String },

    #[error("下班時間 {out} 不能早於上班時間 {in_at}")]
    PunchOutBeforeIn { out: String, in_at: String },

    #[error("休假單 {no} 已經是「{status}」了，簽過的不能再簽一次")]
    LeaveAlreadyDecided { no: String, status: String },

    #[error("{period} 的薪資已經算過了（傳票 {voucher}），同一期不能發兩次")]
    PeriodAlreadyPaid { period: String, voucher: String },

    #[error("{period} 沒有任何一位在職員工需要計薪")]
    NobodyToPay { period: String },

    #[error("{0}")]
    Domain(#[from] laysi_domain::DomainError),
}

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    pub async fn connect(url: &str, max_conns: u32) -> Result<Self> {
        let pool = PgPoolOptions::new().max_connections(max_conns).connect(url).await?;
        Ok(Self { pool })
    }

    /// Build a repository from an existing pool, for composed services and
    /// for route tests that do not connect.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply the migrations. sqlx runs each file in its own transaction, so
    /// no other connection can observe a half-applied schema.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }

    pub async fn migration_version(&self) -> Result<Option<i64>> {
        let row = sqlx::query("SELECT MAX(version) AS v FROM _sqlx_migrations")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.try_get::<Option<i64>, _>("v").ok().flatten()))
    }

    /// Project the compile-time embedded rulesets into `reg_ruleset`.
    ///
    /// The authority is always the TOML under `backend/regulation/`; this
    /// table only makes the rules queryable from SQL and citable from audit
    /// reports. Upserted on (domain, version), so re-running is idempotent.
    pub async fn sync_regulations(&self) -> Result<u64> {
        let mut n = 0;
        for rs in laysi_regulation::all() {
            let r = sqlx::query(
                "INSERT INTO reg_ruleset (domain, version, effective_from, effective_to,
                                          source_note, payload, checksum)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)
                 ON CONFLICT (domain, version) DO UPDATE
                   SET effective_from = EXCLUDED.effective_from,
                       effective_to   = EXCLUDED.effective_to,
                       source_note    = EXCLUDED.source_note,
                       payload        = EXCLUDED.payload,
                       checksum       = EXCLUDED.checksum",
            )
            .bind(&rs.domain)
            .bind(&rs.version)
            .bind(rs.effective_from)
            .bind(rs.effective_to)
            .bind(&rs.source_note)
            .bind(&rs.payload)
            .bind(format!("{:016x}", fnv1a(rs.raw)))
            .execute(&self.pool)
            .await?;
            n += r.rows_affected();
        }
        Ok(n)
    }

    // Accounts

    pub async fn accounts(&self) -> Result<Vec<Account>> {
        Ok(sqlx::query(
            "SELECT code, name, kind, normal_side, is_postable, is_active,
                    parent_code, sort_order, report_group, cash_flow_section
               FROM account ORDER BY sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_account)
        .collect())
    }

    pub async fn account(&self, code: &str) -> Result<Account> {
        sqlx::query(
            "SELECT code, name, kind, normal_side, is_postable, is_active,
                    parent_code, sort_order, report_group, cash_flow_section
               FROM account WHERE code = $1",
        )
        .bind(code)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_account)
        .ok_or_else(|| StoreError::AccountNotFound { code: code.to_string() })
    }

    /// Create an account. The caller decides the report group and cash flow
    /// activity; the database CHECK is the final gate.
    ///
    /// When a parent is given, the same transaction marks the parent as not
    /// postable: a roll-up node that still accepts entries would double count
    /// against its children.
    pub async fn create_account(&self, req: WriteAccount<'_>) -> Result<Account> {
        let mut tx = self.pool.begin().await?;
        prepare_account_parent(&mut tx, req.code, req.parent_code, req.is_postable).await?;

        sqlx::query(
            "INSERT INTO account
                (code, name, kind, normal_side, is_postable, is_active,
                 parent_code, sort_order, report_group, cash_flow_section)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(req.code)
        .bind(req.name)
        .bind(req.kind)
        .bind(req.normal_side)
        .bind(req.is_postable)
        .bind(req.is_active)
        .bind(req.parent_code)
        .bind(req.sort_order)
        .bind(req.report_group)
        .bind(req.cash_flow_section)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.account(req.code).await
    }

    /// Full update of an account. Deletion is deliberately absent;
    /// `is_active = false` only blocks future entries while historical
    /// entries and statements keep the account.
    pub async fn update_account(&self, req: WriteAccount<'_>) -> Result<Account> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query("SELECT 1 FROM account WHERE code = $1 FOR UPDATE")
            .bind(req.code)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
        if !exists {
            return Err(StoreError::AccountNotFound { code: req.code.to_string() });
        }

        prepare_account_parent(&mut tx, req.code, req.parent_code, req.is_postable).await?;

        sqlx::query(
            "UPDATE account
                SET name = $2, kind = $3, normal_side = $4, is_postable = $5,
                    is_active = $6, parent_code = $7, sort_order = $8,
                    report_group = $9, cash_flow_section = $10
              WHERE code = $1",
        )
        .bind(req.code)
        .bind(req.name)
        .bind(req.kind)
        .bind(req.normal_side)
        .bind(req.is_postable)
        .bind(req.is_active)
        .bind(req.parent_code)
        .bind(req.sort_order)
        .bind(req.report_group)
        .bind(req.cash_flow_section)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.account(req.code).await
    }

    /// Create a bank account. `gl_code` must be an active, postable asset
    /// account: a foreign key alone would still allow receipts and payments
    /// to land on a liability or a roll-up node.
    pub async fn create_bank_account(&self, req: CreateBankAccount<'_>) -> Result<BankAccount> {
        let valid = sqlx::query(
            "SELECT 1 FROM account
              WHERE code = $1 AND kind = 'asset' AND is_postable AND is_active",
        )
        .bind(req.gl_code)
        .fetch_optional(&self.pool)
        .await?
        .is_some();
        if !valid {
            return Err(StoreError::InvalidBankGlAccount { code: req.gl_code.to_string() });
        }

        let row = sqlx::query(
            "INSERT INTO bank_account (name, account_no, gl_code, sort_order)
             VALUES ($1,$2,$3,$4)
             RETURNING id, name, account_no, gl_code, sort_order",
        )
        .bind(req.name)
        .bind(req.account_no)
        .bind(req.gl_code)
        .bind(req.sort_order)
        .fetch_one(&self.pool)
        .await?;

        Ok(BankAccount {
            id: row.get("id"),
            name: row.get("name"),
            account_no: row.get("account_no"),
            gl_code: row.get("gl_code"),
            sort_order: row.get("sort_order"),
        })
    }

    // Trial balance and balances

    pub async fn trial_balance(&self, as_of: Option<NaiveDate>) -> Result<Vec<TrialBalanceRow>> {
        Ok(sqlx::query(
            "SELECT a.code, a.name, a.normal_side,
                    COALESCE(SUM(l.debit), 0)  AS debit,
                    COALESCE(SUM(l.credit), 0) AS credit
               FROM account a
               JOIN voucher_line l ON l.account_code = a.code
               JOIN voucher v      ON v.id = l.voucher_id
              WHERE v.status = 'posted'
                AND ($1::date IS NULL OR v.voucher_date <= $1)
              GROUP BY a.code, a.name, a.normal_side, a.sort_order
              ORDER BY a.sort_order, a.code",
        )
        .bind(as_of)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            let debit: Money = r.get("debit");
            let credit: Money = r.get("credit");
            let normal: String = r.get("normal_side");
            TrialBalanceRow {
                code: r.get("code"),
                name: r.get("name"),
                debit,
                credit,
                balance: if normal == "debit" { debit - credit } else { credit - debit },
            }
        })
        .collect())
    }

    /// Posted balance of a set of accounts, signed by their normal side.
    pub async fn balance_of(&self, codes: &[&str], normal_debit: bool) -> Result<Money> {
        let list: Vec<String> = codes.iter().map(|s| s.to_string()).collect();
        let net: Money = sqlx::query(
            "SELECT COALESCE(SUM(l.debit - l.credit), 0) AS net
               FROM voucher_line l JOIN voucher v ON v.id = l.voucher_id
              WHERE v.status = 'posted' AND l.account_code = ANY($1)",
        )
        .bind(&list)
        .fetch_one(&self.pool)
        .await?
        .get("net");
        Ok(if normal_debit { net } else { -net })
    }

    /// Profit over a period (revenue minus expense).
    pub async fn profit_between(&self, from: NaiveDate, to: NaiveDate) -> Result<Money> {
        Ok(sqlx::query(
            "SELECT COALESCE(SUM(CASE WHEN a.kind = 'revenue' THEN l.credit - l.debit
                                      WHEN a.kind = 'expense' THEN l.credit - l.debit
                                      ELSE 0 END), 0) AS net
               FROM voucher_line l
               JOIN voucher v ON v.id = l.voucher_id
               JOIN account a ON a.code = l.account_code
              WHERE v.status = 'posted'
                AND a.kind IN ('revenue','expense')
                AND v.voucher_date BETWEEN $1 AND $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?
        .get("net"))
    }

    /// Posted balances of the bank and petty cash accounts.
    ///
    /// The filter has to live in the subquery: putting `v.status = 'posted'`
    /// in the WHERE of a LEFT JOIN would count unposted entries into the
    /// balance.
    pub async fn cash_position(&self) -> Result<Vec<CashAccount>> {
        Ok(sqlx::query(
            "SELECT b.name, b.account_no,
                    COALESCE(SUM(p.debit - p.credit), 0) AS balance
               FROM bank_account b
               LEFT JOIN (SELECT l.* FROM voucher_line l
                            JOIN voucher v ON v.id = l.voucher_id
                           WHERE v.status = 'posted') p
                      ON p.bank_account_id = b.id
              GROUP BY b.id, b.name, b.account_no, b.sort_order
              ORDER BY b.sort_order",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| CashAccount {
            name: r.get("name"),
            account_no: r.get("account_no"),
            balance: r.get("balance"),
        })
        .collect())
    }

    /// Month-end cash position (cumulative balance) over the last `months`.
    pub async fn cash_history(&self, as_of: NaiveDate, months: i32) -> Result<Vec<(String, Money)>> {
        Ok(sqlx::query(
            "WITH m AS (
                SELECT generate_series(
                    date_trunc('month', $1::timestamp) - make_interval(months => $2 - 1),
                    date_trunc('month', $1::timestamp),
                    interval '1 month') AS month)
             SELECT to_char(m.month, 'YYYY-MM') AS label,
                    COALESCE((SELECT SUM(l.debit - l.credit)
                                FROM voucher_line l JOIN voucher v ON v.id = l.voucher_id
                               WHERE v.status = 'posted'
                                 AND l.account_code IN ('1101','1102')
                                 AND v.voucher_date < (m.month + interval '1 month')::date), 0)
                    AS balance
               FROM m ORDER BY m.month",
        )
        .bind(as_of)
        .bind(months)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| (r.get("label"), r.get("balance")))
        .collect())
    }

    // Vouchers

    pub async fn search_vouchers(
        &self,
        status: Option<&str>,
        q: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Voucher>> {
        let rows = sqlx::query(
            "SELECT v.id, v.no, v.voucher_date, v.memo, v.status, v.source, v.created_by,
                    original.no AS reverses, reversal.no AS reversed_by
               FROM voucher v
               LEFT JOIN voucher original ON original.id = v.reverses
               LEFT JOIN voucher reversal ON reversal.reverses = v.id
              WHERE ($1::text IS NULL OR v.status = $1)
                AND ($2::text IS NULL
                     OR v.memo ILIKE '%' || $2 || '%' OR v.no ILIKE '%' || $2 || '%')
              ORDER BY v.voucher_date DESC, v.no DESC
              LIMIT $3",
        )
        .bind(status)
        .bind(q)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(self.hydrate(r).await?);
        }
        Ok(out)
    }

    pub async fn voucher_by_no(&self, no: &str) -> Result<Voucher> {
        let r = sqlx::query(
            "SELECT v.id, v.no, v.voucher_date, v.memo, v.status, v.source, v.created_by,
                    original.no AS reverses, reversal.no AS reversed_by
               FROM voucher v
               LEFT JOIN voucher original ON original.id = v.reverses
               LEFT JOIN voucher reversal ON reversal.reverses = v.id
              WHERE v.no = $1",
        )
        .bind(no)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::VoucherNotFound { no: no.to_string() })?;
        self.hydrate(&r).await
    }

    async fn hydrate(&self, r: &PgRow) -> Result<Voucher> {
        let id: Uuid = r.get("id");
        let lines: Vec<VoucherLine> = sqlx::query(
            "SELECT l.account_code, a.name AS account_name, l.debit, l.credit, l.memo
               FROM voucher_line l JOIN account a ON a.code = l.account_code
              WHERE l.voucher_id = $1 ORDER BY l.line_no",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|l| VoucherLine {
            account_code: l.get("account_code"),
            account_name: l.get("account_name"),
            debit: l.get("debit"),
            credit: l.get("credit"),
            memo: l.get("memo"),
        })
        .collect();

        let amount = lines.iter().map(|l| l.debit).sum::<Money>();
        // One-line summary in the form "debit account / credit account".
        let label = |l: Option<&VoucherLine>| {
            l.map(|l| format!("{} {}", l.account_code, l.account_name)).unwrap_or_default()
        };
        let accounts_label = format!(
            "{} / {}",
            label(lines.iter().find(|l| l.debit > Money::ZERO)),
            label(lines.iter().find(|l| l.credit > Money::ZERO))
        );

        Ok(Voucher {
            no: r.get("no"),
            date: r.get("voucher_date"),
            memo: r.get("memo"),
            status: r.get("status"),
            source: r.get("source"),
            created_by: r.get("created_by"),
            reverses: r.get("reverses"),
            reversed_by: r.get("reversed_by"),
            amount,
            accounts_label,
            lines,
        })
    }

    /// Create a voucher.
    ///
    /// Lines pass [`laysi_domain::validate_entry`] before the write and the
    /// database's deferred trigger after it. Two gates are deliberate: MCP
    /// tools and future importers can bypass the application layer, so the
    /// database has to be the last line of defence.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_voucher(
        &self,
        date: NaiveDate,
        memo: &str,
        lines: &[LineInput],
        status: &str,
        source: &str,
        created_by: &str,
        audit_id: Option<i64>,
    ) -> Result<String> {
        let mut tx = self.pool.begin().await?;
        let no =
            insert_voucher(&mut tx, date, memo, lines, status, source, created_by, audit_id).await?;
        tx.commit().await?;
        Ok(no)
    }

    pub async fn post_voucher(&self, no: &str) -> Result<Voucher> {
        let status: String = sqlx::query("SELECT status FROM voucher WHERE no = $1")
            .bind(no)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::VoucherNotFound { no: no.to_string() })?
            .get("status");

        if status != "draft" && status != "pending" {
            return Err(StoreError::NotPostable { no: no.to_string(), status });
        }

        sqlx::query("UPDATE voucher SET status = 'posted', posted_at = now() WHERE no = $1")
            .bind(no)
            .execute(&self.pool)
            .await?;
        self.voucher_by_no(no).await
    }

    /// Void a posted voucher. The lines are untouched; the voucher simply
    /// leaves the ledger and the statements.
    pub async fn void_voucher(&self, no: &str) -> Result<Voucher> {
        let mut tx = self.pool.begin().await?;
        let status: String = sqlx::query("SELECT status FROM voucher WHERE no = $1 FOR UPDATE")
            .bind(no)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::VoucherNotFound { no: no.to_string() })?
            .get("status");
        ensure_voidable(no, &status)?;

        sqlx::query("UPDATE voucher SET status = 'void' WHERE no = $1")
            .bind(no)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.voucher_by_no(no).await
    }

    /// Create and immediately post a reversing voucher on a given date.
    ///
    /// The new voucher is written as `pending`, copies the original lines'
    /// bank account and department, and only then becomes `posted`. That
    /// order keeps the existing line validation and deferred trigger in play.
    pub async fn reverse_voucher(
        &self,
        no: &str,
        date: NaiveDate,
        created_by: &str,
    ) -> Result<Voucher> {
        let mut tx = self.pool.begin().await?;

        // Lock the original so two concurrent reversal requests check the
        // uniqueness constraint in sequence.
        let original =
            sqlx::query("SELECT id, status, memo FROM voucher WHERE no = $1 FOR UPDATE")
                .bind(no)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| StoreError::VoucherNotFound { no: no.to_string() })?;
        let original_id: Uuid = original.get("id");
        let status: String = original.get("status");
        ensure_reversible(no, &status)?;

        if let Some(row) = sqlx::query("SELECT no FROM voucher WHERE reverses = $1")
            .bind(original_id)
            .fetch_optional(&mut *tx)
            .await?
        {
            return Err(StoreError::AlreadyReversed {
                no: no.to_string(),
                reversed_by: row.get("no"),
            });
        }

        let rows = sqlx::query(
            "SELECT account_code, debit, credit, memo, bank_account_id, dept_id
               FROM voucher_line
              WHERE voucher_id = $1
              ORDER BY line_no",
        )
        .bind(original_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut lines = Vec::with_capacity(rows.len());
        let mut departments = Vec::with_capacity(rows.len());
        for row in rows {
            let debit: Money = row.get("debit");
            let credit: Money = row.get("credit");
            lines.push(LineInput {
                account_code: row.get("account_code"),
                debit: credit,
                credit: debit,
                memo: row.get("memo"),
                bank_account_id: row.get("bank_account_id"),
            });
            departments.push(row.get::<Option<i32>, _>("dept_id"));
        }

        let original_memo: String = original.get("memo");
        let memo = if original_memo.trim().is_empty() {
            format!("沖銷 {no}")
        } else {
            format!("沖銷 {no} · {original_memo}")
        };
        let reversal_no = insert_voucher(
            &mut tx,
            date,
            &memo,
            &lines,
            "pending",
            "reversal",
            created_by,
            None,
        )
        .await?;

        for (line_no, dept_id) in departments.into_iter().enumerate() {
            set_line_dept(&mut tx, &reversal_no, line_no as i32 + 1, dept_id).await?;
        }

        sqlx::query(
            "UPDATE voucher SET reverses = $2
              WHERE no = $1 AND status = 'pending'",
        )
        .bind(&reversal_no)
        .bind(original_id)
        .execute(&mut *tx)
        .await?;

        // Post only once the lines, bank account, department and reversal link
        // are all in place.
        sqlx::query(
            "UPDATE voucher SET status = 'posted', posted_at = now()
              WHERE no = $1 AND status = 'pending'",
        )
        .bind(&reversal_no)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.voucher_by_no(&reversal_no).await
    }

    // Accounts receivable
    //
    // Every settled amount in the subsidiary ledger points at a voucher, and
    // only posted ones count. This is "the ledger is the only balance of
    // record" expressed in SQL: while the voucher is unposted the ledger has
    // not moved, so the subsidiary ledger may not move either.

    /// Create a plain receivable together with its unposted voucher.
    ///
    /// Not every receivable gets an e-invoice; B2B monthly settlement and
    /// overseas customers use this path directly. E-invoicing calls the same
    /// [`insert_ar_invoice`] so the two creation paths cannot drift.
    pub async fn create_ar_invoice(&self, req: CreateArInvoice<'_>) -> Result<CreatedArInvoice> {
        let mut tx = self.pool.begin().await?;
        let memo = if req.memo.is_empty() {
            format!("應收 {} {}", req.customer, req.invoice_no)
        } else {
            req.memo.to_string()
        };
        let lines = vec![
            LineInput {
                account_code: req.receivable_account_code.to_string(),
                debit: req.amount,
                credit: Money::ZERO,
                memo: Some(req.invoice_no.to_string()),
                bank_account_id: None,
            },
            LineInput {
                account_code: req.revenue_account_code.to_string(),
                debit: Money::ZERO,
                credit: req.amount,
                memo: Some(memo.clone()),
                bank_account_id: None,
            },
        ];
        let voucher_no = insert_ar_invoice(
            &mut tx,
            InsertArInvoice {
                invoice_no: req.invoice_no,
                customer: req.customer,
                issued_on: req.issued_on,
                due_on: req.due_on,
                amount: req.amount,
                voucher_memo: &memo,
                invoice_memo: &memo,
                lines: &lines,
                created_by: req.created_by,
            },
        )
        .await?;
        tx.commit().await?;

        Ok(CreatedArInvoice {
            invoice_no: req.invoice_no.to_string(),
            customer: req.customer.to_string(),
            issued_on: req.issued_on,
            due_on: req.due_on,
            amount: req.amount,
            memo,
            voucher_no,
        })
    }

    /// Receivable detail. `as_of` computes days overdue, `include_settled`
    /// decides whether fully paid invoices are listed.
    pub async fn ar_invoices(
        &self,
        as_of: NaiveDate,
        include_settled: bool,
        q: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ArInvoice>> {
        Ok(sqlx::query(
            "SELECT i.invoice_no, i.customer, i.issued_on, i.due_on, i.amount, i.memo,
                    i.origin_voucher_no,
                    COALESCE(ov.status, 'posted') = 'posted' AS recognised,
                    r.settled, r.in_transit, r.last_voucher_no,
                    ($1::date - i.due_on) AS days_overdue
               FROM ar_invoice i
               LEFT JOIN voucher ov ON ov.no = i.origin_voucher_no
               LEFT JOIN LATERAL (
                   SELECT COALESCE(SUM(rc.amount) FILTER (WHERE v.status = 'posted'), 0)
                              AS settled,
                          COALESCE(SUM(rc.amount) FILTER (WHERE v.status IN ('draft','pending')), 0)
                              AS in_transit,
                          (ARRAY_AGG(rc.voucher_no ORDER BY rc.id DESC))[1] AS last_voucher_no
                     FROM ar_receipt rc JOIN voucher v ON v.no = rc.voucher_no
                    WHERE rc.invoice_id = i.id AND v.status <> 'void') r ON true
              WHERE i.status = 'open'
                AND ($2 OR COALESCE(r.settled, 0) < i.amount)
                AND ($3::text IS NULL
                     OR i.customer ILIKE '%' || $3 || '%'
                     OR i.invoice_no ILIKE '%' || $3 || '%')
              ORDER BY i.due_on, i.invoice_no
              LIMIT $4",
        )
        .bind(as_of)
        .bind(include_settled)
        .bind(q)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            let amount: Money = r.get("amount");
            let settled: Money = r.get("settled");
            let in_transit: Money = r.get("in_transit");
            ArInvoice {
                invoice_no: r.get("invoice_no"),
                customer: r.get("customer"),
                issued_on: r.get("issued_on"),
                due_on: r.get("due_on"),
                amount,
                settled,
                in_transit,
                outstanding: amount - settled,
                state: settle_state(amount, settled, in_transit, "receiving"),
                days_overdue: r.get("days_overdue"),
                memo: r.get("memo"),
                origin_voucher_no: r.get("origin_voucher_no"),
                recognised: r.get("recognised"),
                receipt_voucher_no: r.get("last_voucher_no"),
            }
        })
        .collect())
    }

    /// Total outstanding. Counts only invoices that reached the ledger, so it
    /// must equal the posted balance of 1131.
    pub async fn ar_outstanding_total(&self) -> Result<Money> {
        Ok(sqlx::query(
            "SELECT COALESCE(SUM(i.amount - s.paid), 0) AS total
               FROM ar_invoice i
               LEFT JOIN voucher ov ON ov.no = i.origin_voucher_no
               LEFT JOIN LATERAL (
                   SELECT COALESCE(SUM(rc.amount), 0) AS paid
                     FROM ar_receipt rc JOIN voucher v ON v.no = rc.voucher_no
                    WHERE rc.invoice_id = i.id AND v.status = 'posted') s ON true
              WHERE i.status = 'open' AND COALESCE(ov.status, 'posted') = 'posted'",
        )
        .fetch_one(&self.pool)
        .await?
        .get("total"))
    }

    /// Record a receipt: create an unposted receipt voucher and link it back
    /// to the invoice.
    ///
    /// The voucher and the `ar_receipt` row must share one transaction.
    /// Writing only one side is exactly the divergence between subsidiary
    /// ledger and general ledger this module exists to prevent.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_receipt(
        &self,
        invoice_no: &str,
        amount: Option<Money>,
        received_on: NaiveDate,
        bank_account_id: i32,
        source: &str,
        created_by: &str,
    ) -> Result<Receipt> {
        let mut tx = self.pool.begin().await?;

        // FOR UPDATE: if two people mark the same invoice as received at once,
        // the second waits.
        let row = sqlx::query(
            "SELECT i.id, i.customer, i.amount, i.origin_voucher_no,
                    COALESCE(ov.status, 'posted') AS origin_status,
                    COALESCE((SELECT SUM(rc.amount) FROM ar_receipt rc
                                JOIN voucher v ON v.no = rc.voucher_no
                               WHERE rc.invoice_id = i.id AND v.status <> 'void'), 0) AS claimed
               FROM ar_invoice i
               LEFT JOIN voucher ov ON ov.no = i.origin_voucher_no
              WHERE i.invoice_no = $1 AND i.status = 'open'
              FOR UPDATE OF i",
        )
        .bind(invoice_no)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::InvoiceNotFound { no: invoice_no.to_string() })?;

        let origin_status: String = row.get("origin_status");
        if origin_status != "posted" {
            return Err(StoreError::SourceNotPosted {
                no: invoice_no.to_string(),
                voucher: row.get::<Option<String>, _>("origin_voucher_no").unwrap_or_default(),
            });
        }

        let invoice_id: i32 = row.get("id");
        let customer: String = row.get("customer");
        let total: Money = row.get("amount");
        let claimed: Money = row.get("claimed");
        let left = total - claimed;
        let amount = amount.unwrap_or(left);

        if left <= Money::ZERO {
            return Err(StoreError::NothingLeft { no: invoice_no.to_string() });
        }
        if amount <= Money::ZERO || amount > left {
            return Err(StoreError::AmountOutOfRange {
                no: invoice_no.to_string(),
                left,
                amount,
            });
        }

        let gl = bank_gl_code(&mut tx, bank_account_id).await?;
        let memo = format!("收{customer} {invoice_no} 貨款");
        let lines = vec![
            LineInput {
                account_code: gl,
                debit: amount,
                credit: Money::ZERO,
                memo: Some(memo.clone()),
                bank_account_id: Some(bank_account_id),
            },
            LineInput {
                account_code: "1131".into(),
                debit: Money::ZERO,
                credit: amount,
                memo: Some(invoice_no.to_string()),
                bank_account_id: None,
            },
        ];

        let voucher_no =
            insert_voucher(&mut tx, received_on, &memo, &lines, "pending", source, created_by, None)
                .await?;

        sqlx::query(
            "INSERT INTO ar_receipt (invoice_id, received_on, amount, voucher_no, created_by)
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(invoice_id)
        .bind(received_on)
        .bind(amount)
        .bind(&voucher_no)
        .bind(created_by)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Receipt {
            voucher_no,
            invoice_no: invoice_no.to_string(),
            customer,
            amount,
            still_open: left - amount,
        })
    }

    // Accounts payable

    /// Create a payable bill and its unposted voucher. One transaction, so
    /// neither the subsidiary ledger nor the general ledger is left alone.
    pub async fn create_ap_bill(&self, req: CreateApBill<'_>) -> Result<CreatedApBill> {
        let mut tx = self.pool.begin().await?;
        let memo = if req.memo.is_empty() {
            format!("應付 {} {}", req.vendor, req.bill_no)
        } else {
            req.memo.to_string()
        };
        let lines = vec![
            LineInput {
                account_code: req.debit_account_code.to_string(),
                debit: req.amount,
                credit: Money::ZERO,
                memo: Some(memo.clone()),
                bank_account_id: None,
            },
            LineInput {
                account_code: req.payable_account_code.to_string(),
                debit: Money::ZERO,
                credit: req.amount,
                memo: Some(req.bill_no.to_string()),
                bank_account_id: None,
            },
        ];
        let voucher_no = insert_voucher(
            &mut tx,
            req.billed_on,
            &memo,
            &lines,
            "pending",
            "invoice",
            req.created_by,
            None,
        )
        .await?;

        sqlx::query(
            "INSERT INTO ap_bill
                (bill_no, vendor, memo, billed_on, due_on, amount, origin_voucher_no)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(req.bill_no)
        .bind(req.vendor)
        .bind(&memo)
        .bind(req.billed_on)
        .bind(req.due_on)
        .bind(req.amount)
        .bind(&voucher_no)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(CreatedApBill {
            bill_no: req.bill_no.to_string(),
            vendor: req.vendor.to_string(),
            memo,
            billed_on: req.billed_on,
            due_on: req.due_on,
            amount: req.amount,
            voucher_no,
        })
    }

    pub async fn ap_bills(
        &self,
        as_of: NaiveDate,
        include_settled: bool,
        q: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ApBill>> {
        Ok(sqlx::query(
            "SELECT b.bill_no, b.vendor, b.memo, b.billed_on, b.due_on, b.amount,
                    b.origin_voucher_no,
                    COALESCE(ov.status, 'posted') = 'posted' AS recognised,
                    p.settled, p.in_transit, p.last_batch_no,
                    ($1::date - b.due_on) AS days_overdue
               FROM ap_bill b
               LEFT JOIN voucher ov ON ov.no = b.origin_voucher_no
               LEFT JOIN LATERAL (
                   SELECT COALESCE(SUM(pl.amount) FILTER (WHERE v.status = 'posted'), 0)
                              AS settled,
                          COALESCE(SUM(pl.amount) FILTER (WHERE v.status IN ('draft','pending')), 0)
                              AS in_transit,
                          (ARRAY_AGG(pb.batch_no ORDER BY pb.id DESC))[1] AS last_batch_no
                     FROM ap_payment_line pl
                     JOIN ap_payment_batch pb ON pb.id = pl.batch_id
                     JOIN voucher v ON v.no = pb.voucher_no
                    WHERE pl.bill_id = b.id AND v.status <> 'void') p ON true
              WHERE b.status = 'open'
                AND ($2 OR COALESCE(p.settled, 0) < b.amount)
                AND ($3::text IS NULL
                     OR b.vendor ILIKE '%' || $3 || '%'
                     OR b.bill_no ILIKE '%' || $3 || '%'
                     OR b.memo ILIKE '%' || $3 || '%')
              ORDER BY b.due_on, b.bill_no
              LIMIT $4",
        )
        .bind(as_of)
        .bind(include_settled)
        .bind(q)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            let amount: Money = r.get("amount");
            let settled: Money = r.get("settled");
            let in_transit: Money = r.get("in_transit");
            ApBill {
                bill_no: r.get("bill_no"),
                vendor: r.get("vendor"),
                memo: r.get("memo"),
                billed_on: r.get("billed_on"),
                due_on: r.get("due_on"),
                amount,
                settled,
                in_transit,
                outstanding: amount - settled,
                state: settle_state(amount, settled, in_transit, "paying"),
                days_overdue: r.get("days_overdue"),
                origin_voucher_no: r.get("origin_voucher_no"),
                recognised: r.get("recognised"),
                batch_no: r.get("last_batch_no"),
            }
        })
        .collect())
    }

    /// Total outstanding. Counts only bills that reached the ledger, so it
    /// must equal the posted balance of 2101.
    pub async fn ap_outstanding_total(&self) -> Result<Money> {
        Ok(sqlx::query(
            "SELECT COALESCE(SUM(b.amount - s.paid), 0) AS total
               FROM ap_bill b
               LEFT JOIN voucher ov ON ov.no = b.origin_voucher_no
               LEFT JOIN LATERAL (
                   SELECT COALESCE(SUM(pl.amount), 0) AS paid
                     FROM ap_payment_line pl
                     JOIN ap_payment_batch pb ON pb.id = pl.batch_id
                     JOIN voucher v ON v.no = pb.voucher_no
                    WHERE pl.bill_id = b.id AND v.status = 'posted') s ON true
              WHERE b.status = 'open' AND COALESCE(ov.status, 'posted') = 'posted'",
        )
        .fetch_one(&self.pool)
        .await?
        .get("total"))
    }

    /// Create a payment batch: one voucher covering several bills.
    ///
    /// A debit line per bill keeps it visible who was paid, while the credits
    /// combine into a single bank line, matching the single outgoing
    /// transfer the bank will report.
    pub async fn create_payment_batch(
        &self,
        bill_nos: &[String],
        pay_on: NaiveDate,
        bank_account_id: i32,
        source: &str,
        created_by: &str,
    ) -> Result<PaymentBatch> {
        if bill_nos.is_empty() {
            return Err(StoreError::NothingSelected);
        }

        let mut tx = self.pool.begin().await?;
        let mut lines: Vec<LineInput> = Vec::with_capacity(bill_nos.len() + 1);
        let mut picked: Vec<(i32, Money)> = Vec::with_capacity(bill_nos.len());
        let mut vendors: Vec<String> = Vec::with_capacity(bill_nos.len());
        let mut total = Money::ZERO;

        for bill_no in bill_nos {
            let row = sqlx::query(
                "SELECT b.id, b.vendor, b.memo, b.amount, b.origin_voucher_no,
                        COALESCE(ov.status, 'posted') AS origin_status,
                        COALESCE((SELECT SUM(pl.amount) FROM ap_payment_line pl
                                    JOIN ap_payment_batch pb ON pb.id = pl.batch_id
                                    JOIN voucher v ON v.no = pb.voucher_no
                                   WHERE pl.bill_id = b.id AND v.status <> 'void'), 0) AS claimed
                   FROM ap_bill b
                   LEFT JOIN voucher ov ON ov.no = b.origin_voucher_no
                  WHERE b.bill_no = $1 AND b.status = 'open'
                  FOR UPDATE OF b",
            )
            .bind(bill_no)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::BillNotFound { no: bill_no.clone() })?;

            let origin_status: String = row.get("origin_status");
            if origin_status != "posted" {
                return Err(StoreError::SourceNotPosted {
                    no: bill_no.clone(),
                    voucher: row.get::<Option<String>, _>("origin_voucher_no").unwrap_or_default(),
                });
            }

            let amount: Money = row.get("amount");
            let claimed: Money = row.get("claimed");
            let left = amount - claimed;
            if left <= Money::ZERO {
                return Err(StoreError::NothingLeft { no: bill_no.clone() });
            }

            let vendor: String = row.get("vendor");
            let memo: String = row.get("memo");
            lines.push(LineInput {
                account_code: "2101".into(),
                debit: left,
                credit: Money::ZERO,
                memo: Some(format!("{vendor} {memo}")),
                bank_account_id: None,
            });
            picked.push((row.get("id"), left));
            vendors.push(vendor);
            total += left;
        }

        let gl = bank_gl_code(&mut tx, bank_account_id).await?;
        lines.push(LineInput {
            account_code: gl,
            debit: Money::ZERO,
            credit: total,
            memo: Some(format!("付款單 {} 筆", picked.len())),
            bank_account_id: Some(bank_account_id),
        });

        let memo = format!("付款單：{}", short_list(&vendors));
        let voucher_no =
            insert_voucher(&mut tx, pay_on, &memo, &lines, "pending", source, created_by, None)
                .await?;

        let period = pay_on.format("%y%m").to_string();
        let seq = next_doc_no(&mut tx, "P", &period).await?;
        let batch_no = format!("P{period}-{seq:03}");

        let batch_id: i32 = sqlx::query(
            "INSERT INTO ap_payment_batch (batch_no, pay_on, voucher_no, bank_account_id, created_by)
             VALUES ($1,$2,$3,$4,$5) RETURNING id",
        )
        .bind(&batch_no)
        .bind(pay_on)
        .bind(&voucher_no)
        .bind(bank_account_id)
        .bind(created_by)
        .fetch_one(&mut *tx)
        .await?
        .get("id");

        for (bill_id, amount) in &picked {
            sqlx::query(
                "INSERT INTO ap_payment_line (batch_id, bill_id, amount) VALUES ($1,$2,$3)",
            )
            .bind(batch_id)
            .bind(bill_id)
            .bind(amount)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(PaymentBatch {
            batch_no,
            voucher_no,
            pay_on,
            total,
            bill_count: picked.len() as i64,
            vendors,
        })
    }

    // Bank transactions and reconciliation

    pub async fn bank_transactions(
        &self,
        bank_account_id: Option<i32>,
        only_unmatched: bool,
        limit: i64,
    ) -> Result<Vec<BankTxn>> {
        Ok(sqlx::query(
            "SELECT t.id, t.bank_account_id, a.name AS bank_name, a.account_no,
                    t.txn_date, t.memo, t.amount, t.kind,
                    v.no AS matched_voucher_no, t.matched_at, t.matched_by
               FROM bank_txn t
               JOIN bank_account a ON a.id = t.bank_account_id
               LEFT JOIN voucher v ON v.id = t.matched_voucher_id
              WHERE ($1::int IS NULL OR t.bank_account_id = $1)
                AND (NOT $2 OR t.matched_voucher_id IS NULL)
              ORDER BY t.txn_date DESC, t.id DESC
              LIMIT $3",
        )
        .bind(bank_account_id)
        .bind(only_unmatched)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_bank_txn)
        .collect())
    }

    pub async fn bank_transaction(&self, id: i32) -> Result<BankTxn> {
        sqlx::query(
            "SELECT t.id, t.bank_account_id, a.name AS bank_name, a.account_no,
                    t.txn_date, t.memo, t.amount, t.kind,
                    v.no AS matched_voucher_no, t.matched_at, t.matched_by
               FROM bank_txn t
               JOIN bank_account a ON a.id = t.bank_account_id
               LEFT JOIN voucher v ON v.id = t.matched_voucher_id
              WHERE t.id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_bank_txn)
        .ok_or(StoreError::BankTxnNotFound { id })
    }

    /// Match candidates: vouchers touching the same bank account that no
    /// other transaction has claimed.
    ///
    /// Ranked on two things: how far the voucher's net effect on that account
    /// is from the transaction amount, and how many days apart they are.
    /// Amount first, because value dates drift by a day or two while amounts
    /// do not.
    pub async fn bank_match_candidates(
        &self,
        txn: &BankTxn,
        limit: i64,
    ) -> Result<Vec<MatchCandidate>> {
        let gl: String = sqlx::query("SELECT gl_code FROM bank_account WHERE id = $1")
            .bind(txn.bank_account_id)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| r.get("gl_code"))
            .ok_or(StoreError::BankAccountNotFound { id: txn.bank_account_id })?;

        Ok(sqlx::query(
            "SELECT v.no, v.voucher_date, v.memo, v.status,
                    COALESCE(SUM(l.debit - l.credit), 0) AS net,
                    (v.voucher_date - $3::date) AS day_gap
               FROM voucher v
               JOIN voucher_line l ON l.voucher_id = v.id
                    AND (l.bank_account_id = $1
                         OR (l.bank_account_id IS NULL AND l.account_code = $2))
              WHERE v.status IN ('draft','pending','posted')
                AND NOT EXISTS (SELECT 1 FROM bank_txn t WHERE t.matched_voucher_id = v.id)
              GROUP BY v.id, v.no, v.voucher_date, v.memo, v.status
              ORDER BY abs(COALESCE(SUM(l.debit - l.credit), 0) - $4) ASC,
                       abs(v.voucher_date - $3::date) ASC
              LIMIT $5",
        )
        .bind(txn.bank_account_id)
        .bind(&gl)
        .bind(txn.txn_date)
        .bind(txn.amount)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            let net: Money = r.get("net");
            let day_gap: i32 = r.get("day_gap");
            MatchCandidate {
                voucher_no: r.get("no"),
                date: r.get("voucher_date"),
                memo: r.get("memo"),
                status: r.get("status"),
                net_effect: net,
                amount_gap: (net - txn.amount).abs(),
                day_gap,
            }
        })
        .collect())
    }

    /// Reconcile: link a bank transaction to a voucher.
    ///
    /// This creates no journal entry. Reconciling asserts "what the bank
    /// reported is what my books recorded"; it is not a bookkeeping action.
    /// Duplicate reconciliation is blocked by a SQL UNIQUE constraint, and
    /// this method only produces a readable message first.
    pub async fn match_bank_transaction(
        &self,
        txn_id: i32,
        voucher_no: &str,
        actor: &str,
    ) -> Result<BankTxn> {
        let mut tx = self.pool.begin().await?;

        let existing: Option<Option<Uuid>> =
            sqlx::query("SELECT matched_voucher_id FROM bank_txn WHERE id = $1 FOR UPDATE")
                .bind(txn_id)
                .fetch_optional(&mut *tx)
                .await?
                .map(|r| r.get("matched_voucher_id"));
        match existing {
            None => return Err(StoreError::BankTxnNotFound { id: txn_id }),
            Some(Some(_)) => return Err(StoreError::AlreadyMatched { id: txn_id }),
            Some(None) => {}
        }

        let v = sqlx::query("SELECT id, status FROM voucher WHERE no = $1")
            .bind(voucher_no)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::VoucherNotFound { no: voucher_no.to_string() })?;
        let voucher_id: Uuid = v.get("id");
        let status: String = v.get("status");
        if status == "void" {
            return Err(StoreError::VoucherVoided { no: voucher_no.to_string() });
        }

        let taken: Option<i32> =
            sqlx::query("SELECT id FROM bank_txn WHERE matched_voucher_id = $1")
                .bind(voucher_id)
                .fetch_optional(&mut *tx)
                .await?
                .map(|r| r.get("id"));
        if let Some(other) = taken {
            return Err(StoreError::VoucherTaken { no: voucher_no.to_string(), txn_id: other });
        }

        sqlx::query(
            "UPDATE bank_txn
                SET matched_voucher_id = $1, matched_at = now(), matched_by = $2
              WHERE id = $3",
        )
        .bind(voucher_id)
        .bind(actor)
        .bind(txn_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.bank_transaction(txn_id).await
    }

    // E-invoices
    //
    // `einvoice` is the issuing record: track, buyer, tax, upload status. It
    // is not a second revenue ledger — output tax of record is the balance of
    // 2211, which is where `vat_figures` reads it from.

    pub async fn einvoices(
        &self,
        q: Option<&str>,
        include_void: bool,
        limit: i64,
    ) -> Result<Vec<EInvoice>> {
        Ok(sqlx::query(
            "SELECT e.invoice_no, e.track, e.serial_no, e.buyer_tax_id, e.buyer, e.item,
                    e.issued_on, e.net_amount, e.tax_amount, e.total_amount, e.vat_version,
                    e.upload_status, e.upload_attempts, e.uploaded_at, e.last_error,
                    e.voucher_no, e.ar_invoice_no,
                    COALESCE(ov.status, 'posted') = 'posted' AS recognised,
                    ov.status AS voucher_status
               FROM einvoice e
               LEFT JOIN voucher ov ON ov.no = e.voucher_no
              WHERE ($1 OR e.upload_status <> 'void')
                AND ($2::text IS NULL
                     OR e.buyer ILIKE '%' || $2 || '%'
                     OR e.invoice_no ILIKE '%' || $2 || '%'
                     OR e.item ILIKE '%' || $2 || '%')
              ORDER BY e.issued_on DESC, e.serial_no DESC
              LIMIT $3",
        )
        .bind(include_void)
        .bind(q)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| EInvoice {
            invoice_no: r.get("invoice_no"),
            track: r.get("track"),
            serial_no: r.get("serial_no"),
            buyer_tax_id: r.get("buyer_tax_id"),
            buyer: r.get("buyer"),
            item: r.get("item"),
            issued_on: r.get("issued_on"),
            net_amount: r.get("net_amount"),
            tax_amount: r.get("tax_amount"),
            total_amount: r.get("total_amount"),
            vat_version: r.get("vat_version"),
            upload_status: r.get("upload_status"),
            upload_attempts: r.get("upload_attempts"),
            uploaded_at: r.get("uploaded_at"),
            last_error: r.get("last_error"),
            voucher_no: r.get("voucher_no"),
            voucher_status: r.get("voucher_status"),
            ar_invoice_no: r.get("ar_invoice_no"),
            recognised: r.get("recognised"),
        })
        .collect())
    }

    /// Track status: allocation, next number, numbers remaining.
    pub async fn einvoice_track(&self, track: &str) -> Result<TrackState> {
        let r = sqlx::query(
            "SELECT track, period, range_from, range_to, next_no
               FROM einvoice_track WHERE track = $1",
        )
        .bind(track)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::TrackNotFound { track: track.to_string() })?;
        let range_to: i32 = r.get("range_to");
        let next_no: i32 = r.get("next_no");
        Ok(TrackState {
            track: r.get("track"),
            period: r.get("period"),
            range_from: r.get("range_from"),
            range_to,
            next_no,
            remaining: (range_to - next_no + 1).max(0),
        })
    }

    /// Issue one e-invoice.
    ///
    /// Three things in one transaction: take a number, create the voucher
    /// (debit 1131, credit 4111 and 2211), write the receivable. Done
    /// separately, a track number could be consumed without books, or books
    /// created with nothing to collect.
    ///
    /// The tax is computed by the caller and passed in: the rate is a
    /// regulation value that only `laysi-regulation` may resolve.
    pub async fn issue_einvoice(&self, req: IssueEInvoice<'_>) -> Result<IssuedEInvoice> {
        let mut tx = self.pool.begin().await?;

        // FOR UPDATE: two concurrent issues would otherwise take the same
        // number; the second waits.
        let t = sqlx::query(
            "SELECT range_to, next_no FROM einvoice_track WHERE track = $1 FOR UPDATE",
        )
        .bind(req.track)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::TrackNotFound { track: req.track.to_string() })?;

        let range_to: i32 = t.get("range_to");
        let serial_no: i32 = t.get("next_no");
        if serial_no > range_to {
            return Err(StoreError::TrackExhausted { track: req.track.to_string(), range_to });
        }
        let invoice_no = format!("{}-{serial_no}", req.track);

        let total = req.net_amount + req.tax_amount;
        let memo = format!("開票給{} {}", req.buyer, req.item);
        let mut lines = vec![
            LineInput {
                account_code: "1131".into(),
                debit: total,
                credit: Money::ZERO,
                memo: Some(invoice_no.clone()),
                bank_account_id: None,
            },
            LineInput {
                account_code: "4111".into(),
                debit: Money::ZERO,
                credit: req.net_amount,
                memo: Some(req.item.to_string()),
                bank_account_id: None,
            },
        ];
        // Exempt or zero-rated invoices have no tax, so that line does not
        // exist: a zero-amount journal line is not valid.
        if req.tax_amount > Money::ZERO {
            lines.push(LineInput {
                account_code: "2211".into(),
                debit: Money::ZERO,
                credit: req.tax_amount,
                memo: Some(format!("{invoice_no} 營業稅")),
                bank_account_id: None,
            });
        }

        let voucher_no = insert_ar_invoice(
            &mut tx,
            InsertArInvoice {
                invoice_no: &invoice_no,
                customer: req.buyer,
                issued_on: req.issued_on,
                due_on: req.due_on,
                amount: total,
                voucher_memo: &memo,
                invoice_memo: req.item,
                lines: &lines,
                created_by: req.created_by,
            },
        )
        .await?;

        sqlx::query(
            "INSERT INTO einvoice (track, serial_no, invoice_no, buyer_tax_id, buyer, item,
                                   issued_on, net_amount, tax_amount, total_amount, vat_version,
                                   upload_status, voucher_no, ar_invoice_no, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'queued',$12,$3,$13)",
        )
        .bind(req.track)
        .bind(serial_no)
        .bind(&invoice_no)
        .bind(req.buyer_tax_id)
        .bind(req.buyer)
        .bind(req.item)
        .bind(req.issued_on)
        .bind(req.net_amount)
        .bind(req.tax_amount)
        .bind(total)
        .bind(req.vat_version)
        .bind(&voucher_no)
        .bind(req.created_by)
        .execute(&mut *tx)
        .await?;

        sqlx::query("UPDATE einvoice_track SET next_no = next_no + 1 WHERE track = $1")
            .bind(req.track)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(IssuedEInvoice {
            invoice_no,
            voucher_no,
            buyer: req.buyer.to_string(),
            net_amount: req.net_amount,
            tax_amount: req.tax_amount,
            total_amount: total,
            vat_version: req.vat_version.to_string(),
            remaining: (range_to - serial_no).max(0),
        })
    }

    /// Retry: put a failed invoice back in the upload queue.
    ///
    /// It is never marked as uploaded directly. The tax bureau interface is
    /// not connected, and the status may only change when the platform
    /// actually replies; faking success would make "uploaded" meaningless.
    pub async fn retry_einvoice_upload(&self, invoice_no: &str) -> Result<EInvoice> {
        let status: String = sqlx::query("SELECT upload_status FROM einvoice WHERE invoice_no = $1")
            .bind(invoice_no)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::EInvoiceNotFound { no: invoice_no.to_string() })?
            .get("upload_status");
        if status != "failed" {
            return Err(StoreError::NotRetryable { no: invoice_no.to_string(), status });
        }

        sqlx::query(
            "UPDATE einvoice
                SET upload_status = 'queued', upload_attempts = upload_attempts + 1,
                    last_error = NULL
              WHERE invoice_no = $1",
        )
        .bind(invoice_no)
        .execute(&self.pool)
        .await?;

        self.einvoices(Some(invoice_no), true, 1)
            .await?
            .pop()
            .ok_or_else(|| StoreError::EInvoiceNotFound { no: invoice_no.to_string() })
    }

    // VAT and withholding

    /// The four figures of the VAT return, all derived from posted entries.
    ///
    /// Output sales read 4111, output tax reads 2211, input tax reads 1284,
    /// all GL accounts. Summing einvoice instead would create a second set of
    /// output figures, and the two would eventually disagree.
    ///
    /// `prior_credit` is the credit accumulated before the period start
    /// (input tax in excess of output tax).
    pub async fn vat_figures(&self, from: NaiveDate, to: NaiveDate) -> Result<VatFigures> {
        let r = sqlx::query(
            "SELECT
               COALESCE(SUM(l.credit - l.debit) FILTER (
                 WHERE l.account_code = '4111' AND v.status = 'posted'
                   AND v.voucher_date BETWEEN $1 AND $2), 0) AS sales,
               COALESCE(SUM(l.credit - l.debit) FILTER (
                 WHERE l.account_code = '2211' AND v.status = 'posted'
                   AND v.voucher_date BETWEEN $1 AND $2), 0) AS output_tax,
               COALESCE(SUM(l.debit - l.credit) FILTER (
                 WHERE l.account_code = '1284' AND v.status = 'posted'
                   AND v.voucher_date BETWEEN $1 AND $2), 0) AS input_tax,
               COALESCE(SUM(l.credit - l.debit) FILTER (
                 WHERE l.account_code = '4111' AND v.status IN ('draft','pending')
                   AND v.voucher_date BETWEEN $1 AND $2), 0) AS pending_sales,
               COALESCE(SUM(l.credit - l.debit) FILTER (
                 WHERE l.account_code = '2211' AND v.status IN ('draft','pending')
                   AND v.voucher_date BETWEEN $1 AND $2), 0) AS pending_output_tax,
               COALESCE(SUM(l.debit - l.credit) FILTER (
                 WHERE l.account_code = '1284' AND v.status IN ('draft','pending')
                   AND v.voucher_date BETWEEN $1 AND $2), 0) AS pending_input_tax,
               COALESCE(SUM(l.debit - l.credit) FILTER (
                 WHERE l.account_code = '1284' AND v.status = 'posted'
                   AND v.voucher_date < $1), 0)
             - COALESCE(SUM(l.credit - l.debit) FILTER (
                 WHERE l.account_code = '2211' AND v.status = 'posted'
                   AND v.voucher_date < $1), 0) AS prior_net,
               COUNT(DISTINCT v.no) FILTER (
                 WHERE v.status IN ('draft','pending')
                   AND l.account_code IN ('4111','2211','1284')
                   AND v.voucher_date BETWEEN $1 AND $2) AS pending_vouchers
             FROM voucher_line l JOIN voucher v ON v.id = l.voucher_id",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;

        let prior_net: Money = r.get("prior_net");
        Ok(VatFigures {
            sales: r.get("sales"),
            output_tax: r.get("output_tax"),
            input_tax: r.get("input_tax"),
            pending_sales: r.get("pending_sales"),
            pending_output_tax: r.get("pending_output_tax"),
            pending_input_tax: r.get("pending_input_tax"),
            pending_vouchers: r.get("pending_vouchers"),
            prior_credit: prior_net.max(Money::ZERO),
        })
    }

    /// Payment records over a period. How much to withhold is not computed
    /// here; that belongs to the ruleset.
    pub async fn withholding_records(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<WithholdingRecord>> {
        Ok(sqlx::query(
            "SELECT paid_on, payee, payee_id, income_code, gross_amount, basis,
                    withheld_amount, supplementary_base, memo, voucher_no
               FROM withholding_record
              WHERE paid_on BETWEEN $1 AND $2
              ORDER BY paid_on DESC, id DESC",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| WithholdingRecord {
            paid_on: r.get("paid_on"),
            payee: r.get("payee"),
            payee_id: r.get("payee_id"),
            income_code: r.get("income_code"),
            gross_amount: r.get("gross_amount"),
            basis: r.get("basis"),
            withheld_amount: r.get("withheld_amount"),
            supplementary_base: r.get("supplementary_base"),
            memo: r.get("memo"),
            voucher_no: r.get("voucher_no"),
        })
        .collect())
    }

    // Fixed assets and depreciation

    /// Asset listing. Useful lives are not here: the caller takes `category`
    /// and `acquired_on` to the ruleset, and this layer handles only cost and
    /// accumulated depreciation.
    ///
    /// `accumulated` counts only charges whose source voucher is posted, so
    /// the sum across all assets must equal the posted balance of 1602.
    pub async fn fixed_assets(&self, include_disposed: bool) -> Result<Vec<FixedAsset>> {
        Ok(sqlx::query(
            "SELECT a.id, a.code, a.name, a.category, a.acquired_on, a.cost, a.method,
                    a.gl_code, a.accum_gl_code, a.origin_voucher_no, a.status, a.memo,
                    COALESCE(ov.status, 'posted') = 'posted' AS recognised,
                    COALESCE(d.accumulated, 0) AS accumulated,
                    COALESCE(d.in_transit, 0)  AS in_transit
               FROM fixed_asset a
               LEFT JOIN voucher ov ON ov.no = a.origin_voucher_no
               LEFT JOIN LATERAL (
                   SELECT COALESCE(SUM(e.amount) FILTER (WHERE v.status = 'posted'), 0)
                              AS accumulated,
                          COALESCE(SUM(e.amount) FILTER (WHERE v.status IN ('draft','pending')), 0)
                              AS in_transit
                     FROM depreciation_entry e
                     JOIN depreciation_run r ON r.id = e.run_id
                     JOIN voucher v ON v.no = r.voucher_no
                    WHERE e.asset_id = a.id AND v.status <> 'void') d ON true
              WHERE ($1 OR a.status = 'in_use')
              ORDER BY a.acquired_on, a.code",
        )
        .bind(include_disposed)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| FixedAsset {
            id: r.get("id"),
            code: r.get("code"),
            name: r.get("name"),
            category: r.get("category"),
            acquired_on: r.get("acquired_on"),
            cost: r.get("cost"),
            method: r.get("method"),
            gl_code: r.get("gl_code"),
            accum_gl_code: r.get("accum_gl_code"),
            origin_voucher_no: r.get("origin_voucher_no"),
            status: r.get("status"),
            memo: r.get("memo"),
            recognised: r.get("recognised"),
            accumulated: r.get("accumulated"),
            in_transit: r.get("in_transit"),
        })
        .collect())
    }

    /// Periods already charged. `preview_depreciation` uses it to reject a
    /// duplicate run.
    pub async fn depreciation_runs(&self, limit: i64) -> Result<Vec<DepreciationRun>> {
        Ok(sqlx::query(
            "SELECT r.period, r.kind, r.run_on, r.voucher_no, r.ruleset_version, r.created_by,
                    v.status AS voucher_status,
                    COALESCE((SELECT SUM(e.amount) FROM depreciation_entry e
                               WHERE e.run_id = r.id), 0) AS total
               FROM depreciation_run r JOIN voucher v ON v.no = r.voucher_no
              ORDER BY r.run_on DESC, r.id DESC
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| DepreciationRun {
            period: r.get("period"),
            kind: r.get("kind"),
            run_on: r.get("run_on"),
            voucher_no: r.get("voucher_no"),
            voucher_status: r.get("voucher_status"),
            ruleset_version: r.get("ruleset_version"),
            created_by: r.get("created_by"),
            total: r.get("total"),
        })
        .collect())
    }

    /// Charge one period of depreciation: an unposted voucher (debit 6152,
    /// credit 1602) plus a run row and one entry per asset.
    ///
    /// Every entry stores the useful life and ruleset version resolved at the
    /// time, so a retroactive amendment can identify the rows computed under
    /// the old version. `period` is UNIQUE, so a second run for the same
    /// period is rejected by the database.
    pub async fn record_depreciation_run(
        &self,
        period: &str,
        run_on: NaiveDate,
        lines: &[DepreciationLine],
        ruleset_version: &str,
        created_by: &str,
    ) -> Result<DepreciationRun> {
        if lines.is_empty() {
            return Err(StoreError::NothingToDepreciate);
        }

        let mut tx = self.pool.begin().await?;

        if let Some(row) = sqlx::query("SELECT voucher_no FROM depreciation_run WHERE period = $1")
            .bind(period)
            .fetch_optional(&mut *tx)
            .await?
        {
            return Err(StoreError::PeriodAlreadyDepreciated {
                period: period.to_string(),
                voucher: row.get("voucher_no"),
            });
        }

        let total: Money = lines.iter().map(|l| l.amount).sum();
        let memo = format!("{period} 折舊提列");
        // The credits combine into one accumulated depreciation line; the
        // per-asset debits are what show how much each asset was charged.
        let mut entries: Vec<LineInput> = lines
            .iter()
            .map(|l| LineInput {
                account_code: "6152".into(),
                debit: l.amount,
                credit: Money::ZERO,
                memo: Some(l.asset_name.clone()),
                bank_account_id: None,
            })
            .collect();
        entries.push(LineInput {
            account_code: "1602".into(),
            debit: Money::ZERO,
            credit: total,
            memo: Some(format!("{} 項資產", lines.len())),
            bank_account_id: None,
        });

        let voucher_no = insert_voucher(
            &mut tx,
            run_on,
            &memo,
            &entries,
            "pending",
            "depreciation",
            created_by,
            None,
        )
        .await?;

        let run_id: i32 = sqlx::query(
            "INSERT INTO depreciation_run (period, kind, run_on, voucher_no, ruleset_version,
                                           created_by)
             VALUES ($1,'monthly',$2,$3,$4,$5) RETURNING id",
        )
        .bind(period)
        .bind(run_on)
        .bind(&voucher_no)
        .bind(ruleset_version)
        .bind(created_by)
        .fetch_one(&mut *tx)
        .await?
        .get("id");

        for l in lines {
            sqlx::query(
                "INSERT INTO depreciation_entry (run_id, asset_id, amount, useful_life_years)
                 VALUES ($1,$2,$3,$4)",
            )
            .bind(run_id)
            .bind(l.asset_id)
            .bind(l.amount)
            .bind(l.useful_life_years)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(DepreciationRun {
            period: period.to_string(),
            kind: "monthly".into(),
            run_on,
            voucher_no,
            voucher_status: "pending".into(),
            ruleset_version: Some(ruleset_version.to_string()),
            created_by: created_by.to_string(),
            total,
        })
    }
    // MCP grants and audit

    pub async fn tool_enabled(&self, tool: &str) -> Result<Option<bool>> {
        Ok(sqlx::query("SELECT enabled FROM mcp_tool_grant WHERE tool_name = $1")
            .bind(tool)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| r.get("enabled")))
    }

    pub async fn tool_grants(&self) -> Result<Vec<(String, bool)>> {
        Ok(sqlx::query("SELECT tool_name, enabled FROM mcp_tool_grant ORDER BY tool_name")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| (r.get("tool_name"), r.get("enabled")))
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_audit(
        &self,
        actor: &str,
        actor_role: &str,
        tool: &str,
        risk: &str,
        prompt: Option<&str>,
        channel: &str,
        params: &serde_json::Value,
        summary: &str,
        outcome: &str,
        voucher_no: Option<&str>,
    ) -> Result<i64> {
        Ok(sqlx::query(
            "INSERT INTO ai_audit_log
               (actor, actor_role, tool_name, risk, prompt, channel, params,
                result_summary, outcome, voucher_no)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id",
        )
        .bind(actor)
        .bind(actor_role)
        .bind(tool)
        .bind(risk)
        .bind(prompt)
        .bind(channel)
        .bind(params)
        .bind(summary)
        .bind(outcome)
        .bind(voucher_no)
        .fetch_one(&self.pool)
        .await?
        .get("id"))
    }
    /// Full tool grant listing. `tool_grants` returns only the switches,
    /// while the security screen also needs the risk and the description.
    pub async fn tool_grant_details(&self) -> Result<Vec<ToolGrant>> {
        Ok(sqlx::query(
            "SELECT tool_name, enabled, risk, note, updated_at
               FROM mcp_tool_grant ORDER BY tool_name",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| ToolGrant {
            tool_name: r.get("tool_name"),
            enabled: r.get("enabled"),
            risk: r.get("risk"),
            note: r.get("note"),
            updated_at: r.get("updated_at"),
        })
        .collect())
    }

    pub async fn recent_audit(&self, limit: i64) -> Result<Vec<AuditEntry>> {
        Ok(sqlx::query(
            "SELECT id, at, actor, actor_role, tool_name, risk, prompt, channel,
                    result_summary, outcome, voucher_no, reverted
               FROM ai_audit_log ORDER BY at DESC, id DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| AuditEntry {
            id: r.get("id"),
            at: r.get("at"),
            actor: r.get("actor"),
            actor_role: r.get("actor_role"),
            tool_name: r.get("tool_name"),
            risk: r.get("risk"),
            prompt: r.get("prompt"),
            channel: r.get("channel"),
            result_summary: r.get("result_summary"),
            outcome: r.get("outcome"),
            voucher_no: r.get("voucher_no"),
            reverted: r.get("reverted"),
        })
        .collect())
    }
    // Statements: journal lines are the only source
    //
    // All three financial statements share this one query. It returns the
    // debit and credit movement of each account over a period, not a finished
    // statement: the shape of each statement is laid out by pure functions in
    // `laysi-ops`, and this layer only sums the entries.
    //
    // The reason is practical. If the balance sheet, income statement and
    // cash flow each ran their own SQL, there would be three definitions of
    // "posted" and of how dates are compared. They have to agree, or the
    // balance sheet balances while the cash flow statement does not.
    //
    /// Debit and credit movement per account between `[from, to]`. A `from`
    /// of `None` means since inception.
    ///
    /// A LEFT JOIN keeps accounts with no entries: a statement has to show
    /// that an account was zero this period rather than dropping it. Only
    /// `posted` counts; unposted and voided vouchers stay off the statements.
    pub async fn ledger_movements(
        &self,
        from: Option<NaiveDate>,
        to: NaiveDate,
    ) -> Result<Vec<AccountMovement>> {
        Ok(sqlx::query(
            "SELECT a.code, a.name, a.kind, a.normal_side, a.is_postable, a.is_active,
                    a.report_group, a.cash_flow_section,
                    COALESCE(SUM(l.debit), 0)::numeric  AS debit,
                    COALESCE(SUM(l.credit), 0)::numeric AS credit,
                    COUNT(l.id)::bigint AS line_count
               FROM account a
               LEFT JOIN (
                    SELECT vl.id, vl.account_code, vl.debit, vl.credit
                      FROM voucher_line vl
                      JOIN voucher v ON v.id = vl.voucher_id
                     WHERE v.status = 'posted'
                       AND ($1::date IS NULL OR v.voucher_date >= $1)
                       AND v.voucher_date <= $2
               ) l ON l.account_code = a.code
              GROUP BY a.code, a.name, a.kind, a.normal_side, a.is_postable, a.is_active,
                       a.report_group, a.cash_flow_section, a.sort_order
              ORDER BY a.sort_order, a.code",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_movement)
        .collect())
    }
    /// Balance of each cash account on a given date.
    ///
    /// Same calculation as [`Store::cash_position`] with a cut-off date: the
    /// closing cash on the cash flow statement must match the cash position on
    /// that day, not today's.
    pub async fn cash_position_as_of(&self, as_of: NaiveDate) -> Result<Vec<CashAccount>> {
        Ok(sqlx::query(
            "SELECT b.name, b.account_no,
                    COALESCE(SUM(p.debit - p.credit), 0) AS balance
               FROM bank_account b
               LEFT JOIN (SELECT l.* FROM voucher_line l
                            JOIN voucher v ON v.id = l.voucher_id
                           WHERE v.status = 'posted' AND v.voucher_date <= $1) p
                      ON p.bank_account_id = b.id
              GROUP BY b.id, b.name, b.account_no, b.sort_order
              ORDER BY b.sort_order",
        )
        .bind(as_of)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| CashAccount {
            name: r.get("name"),
            account_no: r.get("account_no"),
            balance: r.get("balance"),
        })
        .collect())
    }
    // Company settings and dimensions

    pub async fn company_settings(&self) -> Result<Vec<CompanySetting>> {
        Ok(sqlx::query(
            "SELECT code, label, value, note, editable, updated_at, updated_by
               FROM company_setting ORDER BY sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| CompanySetting {
            code: r.get("code"),
            label: r.get("label"),
            value: r.get("value"),
            note: r.get("note"),
            editable: r.get("editable"),
            updated_at: r.get("updated_at"),
            updated_by: r.get("updated_by"),
        })
        .collect())
    }
    /// Update one setting. Non-editable entries are refused here: changing
    /// the fiscal year requires reopening the period, not editing a string.
    pub async fn update_company_setting(
        &self,
        code: &str,
        value: &str,
        actor: &str,
    ) -> Result<CompanySetting> {
        let editable: bool = sqlx::query("SELECT editable FROM company_setting WHERE code = $1")
            .bind(code)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::SettingNotFound { code: code.to_string() })?
            .get("editable");
        if !editable {
            return Err(StoreError::SettingNotEditable { code: code.to_string() });
        }

        sqlx::query(
            "UPDATE company_setting
                SET value = $2, updated_at = now(), updated_by = $3
              WHERE code = $1",
        )
        .bind(code)
        .bind(value)
        .bind(actor)
        .execute(&self.pool)
        .await?;

        self.company_settings()
            .await?
            .into_iter()
            .find(|s| s.code == code)
            .ok_or_else(|| StoreError::SettingNotFound { code: code.to_string() })
    }

    pub async fn dimensions(&self) -> Result<Vec<Dimension>> {
        let heads = sqlx::query(
            "SELECT code, label, note FROM dimension ORDER BY sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?;

        let values = sqlx::query(
            "SELECT dimension_code, code, label, active
               FROM dimension_value ORDER BY dimension_code, sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(heads
            .iter()
            .map(|h| {
                let code: String = h.get("code");
                Dimension {
                    label: h.get("label"),
                    note: h.get("note"),
                    values: values
                        .iter()
                        .filter(|v| v.get::<String, _>("dimension_code") == code)
                        .map(|v| DimensionValue {
                            code: v.get("code"),
                            label: v.get("label"),
                            active: v.get("active"),
                        })
                        .collect(),
                    code,
                }
            })
            .collect())
    }
    /// Add a value under an existing dimension; this does not create new
    /// dimension definitions.
    ///
    /// A new `department` value is immediately usable by
    /// `voucher_line.dept_id` and `employee.dept_id`, and the composite
    /// foreign keys on both tables keep those columns from pointing at any
    /// other dimension.
    pub async fn create_dimension_value(
        &self,
        req: CreateDimensionValue<'_>,
    ) -> Result<DimensionValueRecord> {
        let dimension_exists = sqlx::query("SELECT 1 FROM dimension WHERE code = $1")
            .bind(req.dimension_code)
            .fetch_optional(&self.pool)
            .await?
            .is_some();
        if !dimension_exists {
            return Err(StoreError::DimensionNotFound {
                code: req.dimension_code.to_string(),
            });
        }

        let row = sqlx::query(
            "INSERT INTO dimension_value
                (dimension_code, code, label, active, sort_order)
             VALUES ($1,$2,$3,$4,$5)
             RETURNING id, dimension_code, code, label, active, sort_order",
        )
        .bind(req.dimension_code)
        .bind(req.code)
        .bind(req.label)
        .bind(req.active)
        .bind(req.sort_order)
        .fetch_one(&self.pool)
        .await?;

        Ok(DimensionValueRecord {
            id: row.get("id"),
            dimension_code: row.get("dimension_code"),
            code: row.get("code"),
            label: row.get("label"),
            active: row.get("active"),
            sort_order: row.get("sort_order"),
        })
    }
    // Roles and permissions
    //
    // The matrix serves both the settings screen and `Registry::dispatch`.
    // dispatch reads `permission.op_names` on every call, so revoking a cell
    // takes effect without waiting for a cache to expire.
    //

    pub async fn roles(&self) -> Result<Vec<Role>> {
        Ok(sqlx::query("SELECT code, name, note FROM role ORDER BY sort_order, code")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| Role { code: r.get("code"), name: r.get("name"), note: r.get("note") })
            .collect())
    }

    pub async fn permissions(&self) -> Result<Vec<Permission>> {
        Ok(sqlx::query(
            "SELECT code, label, note, op_names FROM permission ORDER BY sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| Permission {
            code: r.get("code"),
            label: r.get("label"),
            note: r.get("note"),
            op_names: r.get("op_names"),
        })
        .collect())
    }
    /// Operation names granted to this role. Deny by default: not in the set
    /// means refused.
    ///
    /// Both `full` and `limited` mean the role may enter that operation group;
    /// a limited scope (own department, own record) is narrowed further by
    /// the operation's own query. An unknown role matches no rows and so
    /// yields an empty set.
    pub async fn permitted_ops(&self, role_code: &str) -> Result<HashSet<String>> {
        Ok(sqlx::query(
            "SELECT DISTINCT unnest(p.op_names) AS op_name
               FROM role_permission rp
               JOIN permission p ON p.code = rp.permission_code
              WHERE rp.role_code = $1",
        )
        .bind(role_code)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| r.get::<String, _>("op_name"))
        .collect())
    }
    /// Display name for a role code.
    ///
    /// The screen shows "Accounting" rather than `acct`, but the full role
    /// list lives in `list_roles`, which ordinary staff may not call. A
    /// person's own role name is not confidential, so it gets its own query
    /// rather than a code-to-name table in the frontend, which would
    /// duplicate the permission model and drift.
    pub async fn role_name(&self, code: &str) -> Result<Option<String>> {
        Ok(sqlx::query("SELECT name FROM role WHERE code = $1")
            .bind(code)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| r.get("name")))
    }

    pub async fn role_permissions(&self) -> Result<Vec<RolePermission>> {
        Ok(sqlx::query(
            "SELECT role_code, permission_code, level, note, updated_at, updated_by
               FROM role_permission",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| RolePermission {
            role_code: r.get("role_code"),
            permission_code: r.get("permission_code"),
            level: r.get("level"),
            note: r.get("note"),
            updated_at: r.get("updated_at"),
            updated_by: r.get("updated_by"),
        })
        .collect())
    }
    /// Grant or revoke one cell. `level = None` revokes by deleting the row.
    ///
    /// Revocation deletes rather than storing a row marked "none": a dash in
    /// the matrix means the grant does not exist, not that it is zero. An
    /// empty grant row would become an exception every future query has to
    /// remember.
    pub async fn set_role_permission(
        &self,
        role_code: &str,
        permission_code: &str,
        level: Option<&str>,
        actor: &str,
    ) -> Result<Option<RolePermission>> {
        let known: bool = sqlx::query(
            "SELECT EXISTS (SELECT 1 FROM role WHERE code = $1)
                AND EXISTS (SELECT 1 FROM permission WHERE code = $2) AS ok",
        )
        .bind(role_code)
        .bind(permission_code)
        .fetch_one(&self.pool)
        .await?
        .get("ok");
        if !known {
            return Err(StoreError::UnknownRoleOrPermission {
                role: role_code.to_string(),
                permission: permission_code.to_string(),
            });
        }

        match level {
            None => {
                sqlx::query(
                    "DELETE FROM role_permission
                      WHERE role_code = $1 AND permission_code = $2",
                )
                .bind(role_code)
                .bind(permission_code)
                .execute(&self.pool)
                .await?;
                Ok(None)
            }
            Some(level) => {
                sqlx::query(
                    "INSERT INTO role_permission
                       (role_code, permission_code, level, updated_at, updated_by)
                     VALUES ($1,$2,$3,now(),$4)
                     ON CONFLICT (role_code, permission_code) DO UPDATE
                       SET level = EXCLUDED.level,
                           updated_at = EXCLUDED.updated_at,
                           updated_by = EXCLUDED.updated_by",
                )
                .bind(role_code)
                .bind(permission_code)
                .bind(level)
                .bind(actor)
                .execute(&self.pool)
                .await?;

                Ok(self
                    .role_permissions()
                    .await?
                    .into_iter()
                    .find(|p| p.role_code == role_code && p.permission_code == permission_code))
            }
        }
    }
    // User accounts, external identities and sessions
    /// Number of accounts. `COUNT()` keeps PostgreSQL's BIGINT type explicit.
    pub async fn user_count(&self) -> Result<i64> {
        Ok(sqlx::query("SELECT COUNT(*)::bigint AS count FROM user_account")
            .fetch_one(&self.pool)
            .await?
            .get("count"))
    }
    /// List account metadata. The return type carries the password hash, and
    /// admin tools must not print that column.
    pub async fn users(&self) -> Result<Vec<UserAccount>> {
        Ok(sqlx::query(
            "SELECT id, username, display_name, email, password_hash, role_code,
                    employee_id, status, must_change_password, created_at, created_by,
                    last_login_at
               FROM user_account
              ORDER BY username",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_user_account)
        .collect())
    }
    /// Look up an account by username. The return value carries the password
    /// hash for verification and deliberately does not implement Serialize.
    pub async fn user_by_username(&self, username: &str) -> Result<Option<UserAccount>> {
        Ok(sqlx::query(
            "SELECT id, username, display_name, email, password_hash, role_code,
                    employee_id, status, must_change_password, created_at, created_by,
                    last_login_at
               FROM user_account
              WHERE username = $1",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_user_account))
    }
    /// Look up an account by the IdP's stable subject; email is never used to
    /// match a login.
    pub async fn user_by_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<UserAccount>> {
        Ok(sqlx::query(
            "SELECT u.id, u.username, u.display_name, u.email, u.password_hash, u.role_code,
                    u.employee_id, u.status, u.must_change_password, u.created_at,
                    u.created_by, u.last_login_at
               FROM user_identity i
               JOIN user_account u ON u.id = i.user_id
              WHERE i.provider = $1 AND i.subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_user_account))
    }
    /// Create an account. With a password hash of `None` the account can only
    /// sign in through an external identity.
    pub async fn create_user(&self, req: CreateUser<'_>) -> Result<UserAccount> {
        let mut conn = self.pool.acquire().await?;
        insert_user(&mut conn, req).await
    }
    /// Whether the web bootstrap endpoint may still create the first account.
    ///
    /// Both halves matter. An empty `install_bootstrap` with accounts present
    /// is an installation set up from the CLI; a filled `install_bootstrap`
    /// with no accounts is one whose accounts were deleted afterwards, and
    /// that must not reopen an unauthenticated account-creating endpoint.
    pub async fn bootstrap_open(&self) -> Result<bool> {
        Ok(sqlx::query(
            "SELECT NOT EXISTS (SELECT 1 FROM user_account)
                AND NOT EXISTS (SELECT 1 FROM install_bootstrap) AS can_bootstrap",
        )
        .fetch_one(&self.pool)
        .await?
        .get("can_bootstrap"))
    }
    /// Claim the installation and create its first account in one transaction.
    /// `None` means the window was already closed and nothing was written.
    ///
    /// The claim and the account are inserted together so that no path can
    /// produce one without the other. `WHERE NOT EXISTS` closes the window
    /// against an account created meanwhile by the CLI; the primary key closes
    /// it against a second web caller, which blocks on the conflicting row
    /// until this transaction commits and then affects no rows.
    pub async fn bootstrap_first_user(
        &self,
        req: CreateUser<'_>,
        user_agent: &str,
        token_required: bool,
    ) -> Result<Option<UserAccount>> {
        let mut tx = self.pool.begin().await?;
        let claimed = sqlx::query(
            "INSERT INTO install_bootstrap (id, username, user_agent, token_required)
             SELECT 1, $1::text, $2::text, $3::boolean
              WHERE NOT EXISTS (SELECT 1 FROM user_account)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(req.username)
        .bind(user_agent)
        .bind(token_required)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            return Ok(None);
        }
        let user = insert_user(&mut tx, req).await?;
        tx.commit().await?;
        Ok(Some(user))
    }
    /// Set or clear the password hash. Once cleared the account accepts only
    /// its linked external identity.
    pub async fn set_password_hash(
        &self,
        user_id: i32,
        password_hash: Option<&str>,
        must_change_password: bool,
    ) -> Result<UserAccount> {
        let row = sqlx::query(
            "UPDATE user_account
                SET password_hash = $2, must_change_password = $3
              WHERE id = $1
             RETURNING id, username, display_name, email, password_hash, role_code,
                       employee_id, status, must_change_password, created_at, created_by,
                       last_login_at",
        )
        .bind(user_id)
        .bind(password_hash)
        .bind(must_change_password)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UserNotFound { id: user_id })?;
        Ok(row_to_user_account(&row))
    }
    /// Link an IdP subject to an existing account. A unique constraint stops
    /// the same external identity being claimed twice.
    pub async fn link_identity(
        &self,
        user_id: i32,
        provider: &str,
        subject: &str,
        email_at_link: &str,
    ) -> Result<UserIdentity> {
        let row = sqlx::query(
            "INSERT INTO user_identity (user_id, provider, subject, email_at_link)
             VALUES ($1,$2,$3,$4)
             RETURNING id, user_id, provider, subject, email_at_link, linked_at",
        )
        .bind(user_id)
        .bind(provider)
        .bind(subject)
        .bind(email_at_link)
        .fetch_one(&self.pool)
        .await?;
        Ok(UserIdentity {
            id: row.get("id"),
            user_id: row.get("user_id"),
            provider: row.get("provider"),
            subject: row.get("subject"),
            email_at_link: row.get("email_at_link"),
            linked_at: row.get("linked_at"),
        })
    }
    /// Point a login at a roster row, or clear the link with `None`.
    ///
    /// Needed for accounts created before the person had an employee record,
    /// and for the owner, who is an employee too: without a link the four
    /// self-service screens have no subject to be about.
    pub async fn set_employee_link(
        &self,
        user_id: i32,
        employee_id: Option<i32>,
    ) -> Result<UserAccount> {
        let row = sqlx::query(
            "UPDATE user_account
                SET employee_id = $2
              WHERE id = $1
             RETURNING id, username, display_name, email, password_hash, role_code,
                       employee_id, status, must_change_password, created_at, created_by,
                       last_login_at",
        )
        .bind(user_id)
        .bind(employee_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UserNotFound { id: user_id })?;
        Ok(row_to_user_account(&row))
    }

    /// Create a session and update the last login time, in one transaction so
    /// neither happens alone.
    pub async fn create_session(&self, req: CreateSession<'_>) -> Result<UserSession> {
        let mut tx = self.pool.begin().await?;
        let user_row = sqlx::query(
            "UPDATE user_account
                SET last_login_at = now()
              WHERE id = $1 AND status = 'active'
             RETURNING id, username, display_name, email, password_hash, role_code,
                       employee_id, status, must_change_password, created_at, created_by,
                       last_login_at",
        )
        .bind(req.user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::UserNotActive { id: req.user_id })?;

        let session_row = sqlx::query(
            "INSERT INTO user_session
                    (token_hash, user_id, expires_at, user_agent, ip)
             VALUES ($1,$2,$3,$4,$5)
             RETURNING id, created_at, last_seen_at, expires_at, revoked_at,
                       revoked_by, user_agent, ip",
        )
        .bind(req.token_hash)
        .bind(req.user_id)
        .bind(req.expires_at)
        .bind(req.user_agent)
        .bind(req.ip)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(UserSession {
            id: session_row.get("id"),
            user: row_to_user_account(&user_row),
            created_at: session_row.get("created_at"),
            last_seen_at: session_row.get("last_seen_at"),
            expires_at: session_row.get("expires_at"),
            revoked_at: session_row.get("revoked_at"),
            revoked_by: session_row.get("revoked_by"),
            user_agent: session_row.get("user_agent"),
            ip: session_row.get("ip"),
        })
    }
    /// Fetch a still-usable session by hash.
    ///
    /// Returns `None` when the session is revoked, expired, or its account is
    /// disabled, so the caller cannot forget one of those conditions. The
    /// plaintext token stays with the caller and must already be hashed
    /// before reaching this method.
    pub async fn session_by_hash(
        &self,
        token_hash: &str,
        idle_cutoff: DateTime<Utc>,
    ) -> Result<Option<UserSession>> {
        Ok(sqlx::query(
            "SELECT s.id AS session_id, s.created_at AS session_created_at,
                    s.last_seen_at, s.expires_at, s.revoked_at, s.revoked_by,
                    s.user_agent, s.ip,
                    u.id AS account_id, u.username, u.display_name, u.email,
                    u.password_hash, u.role_code, u.employee_id, u.status,
                    u.must_change_password, u.created_at AS account_created_at,
                    u.created_by, u.last_login_at
               FROM user_session s
               JOIN user_account u ON u.id = s.user_id
              WHERE s.token_hash = $1
                AND s.revoked_at IS NULL
                AND s.expires_at > now()
                AND s.last_seen_at > $2
                AND u.status = 'active'",
        )
        .bind(token_hash)
        .bind(idle_cutoff)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_user_session))
    }
    /// Revoke a session. `false` means it did not exist or was already
    /// revoked.
    pub async fn revoke_session(&self, token_hash: &str, actor: &str) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE user_session
                SET revoked_at = now(), revoked_by = $2
              WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(token_hash)
        .bind(actor)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0)
    }
    /// Touch the last-activity time of a live session. Expired, revoked or
    /// disabled sessions are not extended.
    pub async fn touch_session(
        &self,
        token_hash: &str,
        idle_cutoff: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE user_session s
                SET last_seen_at = now()
              WHERE s.token_hash = $1
                AND s.revoked_at IS NULL
                AND s.expires_at > now()
                AND s.last_seen_at > $2
                AND EXISTS (
                    SELECT 1 FROM user_account u
                     WHERE u.id = s.user_id AND u.status = 'active'
                )",
        )
        .bind(token_hash)
        .bind(idle_cutoff)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0)
    }

    pub async fn approval_rules(&self) -> Result<Vec<ApprovalRule>> {
        Ok(sqlx::query(
            "SELECT a.step_no, a.role_code, r.name AS role_name, a.label,
                    a.condition_note, a.min_amount
               FROM approval_rule a LEFT JOIN role r ON r.code = a.role_code
              ORDER BY a.step_no",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| ApprovalRule {
            step_no: r.get("step_no"),
            role_code: r.get("role_code"),
            role_name: r.get("role_name"),
            label: r.get("label"),
            condition_note: r.get("condition_note"),
            min_amount: r.get("min_amount"),
        })
        .collect())
    }
    /// Insert or replace one approval step. The same `step_no` is the same
    /// step, hence an upsert.
    ///
    /// `min_amount` is the company's own approval policy, not regulation. It
    /// stays in the table, is never derived from a ruleset, and must not move
    /// into the regulation TOML.
    pub async fn set_approval_rule(&self, req: SetApprovalRule<'_>) -> Result<ApprovalRule> {
        if let Some(role_code) = req.role_code {
            let role_exists = sqlx::query("SELECT 1 FROM role WHERE code = $1")
                .bind(role_code)
                .fetch_optional(&self.pool)
                .await?
                .is_some();
            if !role_exists {
                return Err(StoreError::RoleNotFound { code: role_code.to_string() });
            }
        }

        sqlx::query(
            "INSERT INTO approval_rule
                (step_no, role_code, label, condition_note, min_amount)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (step_no) DO UPDATE
               SET role_code = EXCLUDED.role_code,
                   label = EXCLUDED.label,
                   condition_note = EXCLUDED.condition_note,
                   min_amount = EXCLUDED.min_amount",
        )
        .bind(req.step_no)
        .bind(req.role_code)
        .bind(req.label)
        .bind(req.condition_note)
        .bind(req.min_amount)
        .execute(&self.pool)
        .await?;

        self.approval_rules()
            .await?
            .into_iter()
            .find(|rule| rule.step_no == req.step_no)
            .ok_or(StoreError::ApprovalRuleNotFound { step_no: req.step_no })
    }
    /// Delete a step, keeping at least one in the flow.
    ///
    /// Locking the whole small table is deliberate: if two people delete the
    /// last two steps at once, each transaction's opening COUNT would still
    /// show another step remaining. The table lock makes the second delete
    /// observe the first.
    pub async fn delete_approval_rule(&self, step_no: i32) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("LOCK TABLE approval_rule IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await?;

        let row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS total,
                    COALESCE(BOOL_OR(step_no = $1), false) AS found
               FROM approval_rule",
        )
        .bind(step_no)
        .fetch_one(&mut *tx)
        .await?;
        let total: i64 = row.get("total");
        let found: bool = row.get("found");
        if !found {
            return Err(StoreError::ApprovalRuleNotFound { step_no });
        }
        ensure_approval_rule_can_delete(total)?;

        sqlx::query("DELETE FROM approval_rule WHERE step_no = $1")
            .bind(step_no)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    // Security policies and tokens

    pub async fn security_policies(&self) -> Result<Vec<SecurityPolicy>> {
        Ok(sqlx::query(
            "SELECT code, label, value, detail, enforced, sort_order
               FROM security_policy ORDER BY sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| SecurityPolicy {
            code: r.get("code"),
            label: r.get("label"),
            value: r.get("value"),
            detail: r.get("detail"),
            enforced: r.get("enforced"),
            sort_order: r.get("sort_order"),
        })
        .collect())
    }

    pub async fn api_tokens(&self, include_revoked: bool) -> Result<Vec<ApiToken>> {
        self.api_tokens_filtered(include_revoked, None).await
    }

    /// Tokens one employee issued for themselves.
    pub async fn api_tokens_owned_by(
        &self,
        employee_id: i32,
        include_revoked: bool,
    ) -> Result<Vec<ApiToken>> {
        self.api_tokens_filtered(include_revoked, Some(employee_id)).await
    }

    async fn api_tokens_filtered(
        &self,
        include_revoked: bool,
        owner_employee_id: Option<i32>,
    ) -> Result<Vec<ApiToken>> {
        Ok(sqlx::query(
            "SELECT t.id, t.name, t.kind, t.scope, t.prefix, t.actor, t.actor_role,
                    r.name AS actor_role_name, t.created_at, t.created_by,
                    t.last_used_at, t.rotate_days, t.expiry_note,
                    t.revoked_at, t.revoked_by, t.owner_employee_id
               FROM api_token t LEFT JOIN role r ON r.code = t.actor_role
              WHERE ($1 OR t.revoked_at IS NULL)
                AND ($2::int IS NULL OR t.owner_employee_id = $2)
              ORDER BY t.revoked_at NULLS FIRST, t.last_used_at DESC NULLS LAST, t.id",
        )
        .bind(include_revoked)
        .bind(owner_employee_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_api_token)
        .collect())
    }
    /// Fetch a token row by hash, whether or not it is revoked or expired.
    ///
    /// Revoked and expired rows are deliberately not filtered out in SQL: the
    /// caller (`/mcp` auth) needs to tell revocation and expiry apart in the
    /// audit summary. Usability is decided by calling
    /// [`ApiToken::is_expired`] and checking `revoked_at`; finding a row must
    /// never be treated as authentication having succeeded.
    pub async fn token_by_hash(&self, token_hash: &str) -> Result<Option<ApiToken>> {
        Ok(sqlx::query(
            "SELECT t.id, t.name, t.kind, t.scope, t.prefix, t.actor, t.actor_role,
                    r.name AS actor_role_name, t.created_at, t.created_by,
                    t.last_used_at, t.rotate_days, t.expiry_note,
                    t.revoked_at, t.revoked_by, t.owner_employee_id
               FROM api_token t LEFT JOIN role r ON r.code = t.actor_role
              WHERE t.token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_api_token))
    }
    /// Record the last successful use of a token. Revoked tokens are not
    /// updated.
    pub async fn touch_api_token_used(&self, id: i32) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE api_token SET last_used_at = now()
              WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0)
    }
    /// Issue a new token.
    ///
    /// The plaintext appears only in this return value; the system cannot
    /// read it back afterwards, because the table stores a BLAKE3 hash and
    /// the first eight characters. A forgotten token can only be reissued.
    pub async fn issue_api_token(&self, req: IssueApiToken<'_>) -> Result<IssuedApiToken> {
        // Two v4 UUIDs concatenated as hex: 256 bits of randomness from the
        // operating system CSPRNG.
        let secret = format!(
            "lys_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let prefix: String = secret.chars().take(8).collect();
        let hash = token_hash(&secret);

        let id: i32 = sqlx::query(
            "INSERT INTO api_token (name, kind, scope, prefix, token_hash, actor, actor_role,
                                    created_by, rotate_days, expiry_note, owner_employee_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING id",
        )
        .bind(req.name)
        .bind(req.kind)
        .bind(req.scope)
        .bind(&prefix)
        .bind(&hash)
        .bind(req.actor)
        .bind(req.actor_role)
        .bind(req.created_by)
        .bind(req.rotate_days)
        .bind(req.expiry_note)
        .bind(req.owner_employee_id)
        .fetch_one(&self.pool)
        .await?
        .get("id");

        let token = self
            .api_tokens(true)
            .await?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| StoreError::TokenNotFound { id })?;
        Ok(IssuedApiToken { token, secret })
    }
    /// Revoke a token. An already revoked token keeps its original timestamp;
    /// the moment of revocation is an audit fact.
    pub async fn revoke_api_token(&self, id: i32, actor: &str) -> Result<ApiToken> {
        self.revoke_token_where(id, None, actor).await
    }

    /// Revoke a token the caller issued for themselves.
    ///
    /// The employee is part of the WHERE clause and of the lookup that
    /// follows, so someone else's id finds no row at all rather than being
    /// found and then refused.
    pub async fn revoke_own_api_token(
        &self,
        id: i32,
        employee_id: i32,
        actor: &str,
    ) -> Result<ApiToken> {
        self.revoke_token_where(id, Some(employee_id), actor).await
    }

    async fn revoke_token_where(
        &self,
        id: i32,
        owner_employee_id: Option<i32>,
        actor: &str,
    ) -> Result<ApiToken> {
        let done = sqlx::query(
            "UPDATE api_token SET revoked_at = now(), revoked_by = $2
              WHERE id = $1 AND revoked_at IS NULL
                AND ($3::int IS NULL OR owner_employee_id = $3)",
        )
        .bind(id)
        .bind(actor)
        .bind(owner_employee_id)
        .execute(&self.pool)
        .await?;

        let token = self
            .api_tokens_filtered(true, owner_employee_id)
            .await?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or(StoreError::TokenNotFound { id })?;
        if done.rows_affected() == 0 {
            return Err(StoreError::TokenAlreadyRevoked { name: token.name });
        }
        Ok(token)
    }
    // Department dimension
    /// Department list: the `dimension_value` rows with
    /// `dimension_code = 'department'`.
    pub async fn departments(&self) -> Result<Vec<Department>> {
        Ok(sqlx::query(
            "SELECT id, code, label, active FROM dimension_value
              WHERE dimension_code = 'department' ORDER BY sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| Department {
            id: r.get("id"),
            code: r.get("code"),
            label: r.get("label"),
            active: r.get("active"),
        })
        .collect())
    }
    /// Period movements split by department and account. Both budget
    /// utilisation and departmental P&L are derived from this one query.
    ///
    /// Same definitions as [`Store::ledger_movements`] (posted only, closed
    /// date interval) with one extra `dept_id` dimension. The totals of the
    /// two queries must agree: departmental P&L summing to the income
    /// statement is verified this way, not assumed.
    ///
    /// Entries with no department come back as `dept_id = None` and are never
    /// dropped: that money was still spent, it just has no owner.
    pub async fn dept_movements(
        &self,
        from: Option<NaiveDate>,
        to: NaiveDate,
    ) -> Result<Vec<DeptMovement>> {
        Ok(sqlx::query(
            "SELECT l.dept_id, d.code AS dept_code, d.label AS dept_label,
                    a.code, a.name, a.kind, a.report_group,
                    SUM(l.debit) AS debit, SUM(l.credit) AS credit, COUNT(*) AS line_count
               FROM voucher_line l
               JOIN voucher v ON v.id = l.voucher_id
               JOIN account a ON a.code = l.account_code
               LEFT JOIN dimension_value d ON d.id = l.dept_id
              WHERE v.status = 'posted'
                AND ($1::date IS NULL OR v.voucher_date >= $1)
                AND v.voucher_date <= $2
              GROUP BY l.dept_id, d.code, d.label, d.sort_order,
                       a.code, a.name, a.kind, a.report_group, a.sort_order
              ORDER BY d.sort_order NULLS LAST, a.sort_order, a.code",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| DeptMovement {
            dept_id: r.get("dept_id"),
            dept_code: r.get("dept_code"),
            dept_label: r.get("dept_label"),
            code: r.get("code"),
            name: r.get("name"),
            kind: r.get("kind"),
            report_group: r.get("report_group"),
            debit: r.get("debit"),
            credit: r.get("credit"),
            line_count: r.get("line_count"),
        })
        .collect())
    }
    // Budgets
    //
    // These two methods read and write budgeted amounts only. Actuals are not
    // here and not in any table; for actuals use `dept_movements`, which is
    // the ledger.
    //

    pub async fn budget_versions(&self) -> Result<Vec<BudgetVersion>> {
        Ok(sqlx::query(
            "SELECT v.code, v.label, v.fiscal_year, v.is_current, v.note, v.created_by,
                    COALESCE(n.rows, 0) AS row_count,
                    COALESCE(n.total, 0) AS total
               FROM budget_version v
               LEFT JOIN LATERAL (SELECT COUNT(*) AS rows, SUM(b.amount) AS total
                                    FROM budget b WHERE b.version_code = v.code) n ON true
              ORDER BY v.fiscal_year DESC, v.code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| BudgetVersion {
            code: r.get("code"),
            label: r.get("label"),
            fiscal_year: r.get("fiscal_year"),
            is_current: r.get("is_current"),
            note: r.get("note"),
            created_by: r.get("created_by"),
            row_count: r.get("row_count"),
            total: r.get("total"),
        })
        .collect())
    }

    pub async fn budget_rows(&self, version_code: &str) -> Result<Vec<BudgetRow>> {
        Ok(sqlx::query(
            "SELECT b.account_code, a.name AS account_name, b.dept_id,
                    d.code AS dept_code, d.label AS dept_label,
                    b.amount, b.note, b.updated_at, b.updated_by
               FROM budget b
               JOIN account a ON a.code = b.account_code
               LEFT JOIN dimension_value d ON d.id = b.dept_id
              WHERE b.version_code = $1
              ORDER BY a.sort_order, a.code, d.sort_order NULLS FIRST",
        )
        .bind(version_code)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| BudgetRow {
            account_code: r.get("account_code"),
            account_name: r.get("account_name"),
            dept_id: r.get("dept_id"),
            dept_code: r.get("dept_code"),
            dept_label: r.get("dept_label"),
            amount: r.get("amount"),
            note: r.get("note"),
            updated_at: r.get("updated_at"),
            updated_by: r.get("updated_by"),
        })
        .collect())
    }

    /// Set one budget row. A `dept_code` of `None` is the company-wide row.
    pub async fn set_budget(
        &self,
        version_code: &str,
        account_code: &str,
        dept_code: Option<&str>,
        amount: Money,
        note: &str,
        actor: &str,
    ) -> Result<BudgetRow> {
        let known: bool =
            sqlx::query("SELECT EXISTS (SELECT 1 FROM budget_version WHERE code = $1) AS ok")
                .bind(version_code)
                .fetch_one(&self.pool)
                .await?
                .get("ok");
        if !known {
            return Err(StoreError::BudgetVersionNotFound { code: version_code.to_string() });
        }

        let dept_id = match dept_code {
            None => None,
            Some(code) => Some(self.department_id(code).await?),
        };

        sqlx::query(
            "INSERT INTO budget (version_code, account_code, dept_id, amount, note,
                                 updated_at, updated_by)
             VALUES ($1,$2,$3,$4,$5,now(),$6)
             ON CONFLICT (version_code, account_code, dept_id) DO UPDATE
               SET amount = EXCLUDED.amount, note = EXCLUDED.note,
                   updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by",
        )
        .bind(version_code)
        .bind(account_code)
        .bind(dept_id)
        .bind(amount)
        .bind(note)
        .bind(actor)
        .execute(&self.pool)
        .await?;

        self.budget_rows(version_code)
            .await?
            .into_iter()
            .find(|b| b.account_code == account_code && b.dept_id == dept_id)
            .ok_or_else(|| StoreError::UnknownAccount { code: account_code.to_string() })
    }

    async fn department_id(&self, code: &str) -> Result<i32> {
        sqlx::query(
            "SELECT id FROM dimension_value WHERE dimension_code = 'department' AND code = $1",
        )
        .bind(code)
        .fetch_optional(&self.pool)
        .await?
        .map(|r| r.get("id"))
        .ok_or_else(|| StoreError::DepartmentNotFound { code: code.to_string() })
    }

    // Expense claims
    //
    // Amount thresholds are not in this layer. They live in `approval_rule`,
    // and ops reads them to decide which steps a claim must pass. This layer
    // only records a step's decision and creates the voucher the final step
    // produces. Those two must share one transaction: an approval without a
    // voucher, or a voucher without an approval record, is a broken trail.

    pub async fn expense_claims(
        &self,
        include_done: bool,
        limit: i64,
    ) -> Result<Vec<ExpenseClaim>> {
        self.expense_claims_filtered(include_done, limit, None).await
    }

    /// Claims submitted by one claimant. Self-service operations pass only the
    /// employee name resolved from `ctx.actor`; the query itself accepts no
    /// other employee identifier.
    pub async fn expense_claims_by_claimant(
        &self,
        claimant: &str,
        limit: i64,
    ) -> Result<Vec<ExpenseClaim>> {
        self.expense_claims_filtered(true, limit, Some(claimant)).await
    }

    async fn expense_claims_filtered(
        &self,
        include_done: bool,
        limit: i64,
        claimant: Option<&str>,
    ) -> Result<Vec<ExpenseClaim>> {
        let rows = sqlx::query(
            "SELECT c.id, c.claim_no, c.title, c.claimant, c.claimant_role,
                    c.dept_id, d.code AS dept_code, d.label AS dept_label,
                    c.spent_on, c.amount, c.suggested_account_code, c.account_code,
                    sa.name AS suggested_account_name, aa.name AS account_name,
                    c.pay_method, c.status, c.voucher_no, v.status AS voucher_status,
                    c.document_id, doc.doc_no, doc.name AS document_name,
                    c.memo, c.created_at, c.created_by
               FROM expense_claim c
               LEFT JOIN dimension_value d ON d.id = c.dept_id
               LEFT JOIN account sa ON sa.code = c.suggested_account_code
               LEFT JOIN account aa ON aa.code = c.account_code
               LEFT JOIN voucher v ON v.no = c.voucher_no
               LEFT JOIN document doc ON doc.id = c.document_id
              WHERE ($1 OR c.status IN ('submitted','approving'))
                AND ($3::text IS NULL OR c.claimant = $3)
              ORDER BY c.spent_on DESC, c.id DESC
              LIMIT $2",
        )
        .bind(include_done)
        .bind(limit)
        .bind(claimant)
        .fetch_all(&self.pool)
        .await?;

        let steps = sqlx::query(
            "SELECT claim_id, step_no, decision, actor, actor_role, note, decided_at
               FROM expense_claim_step ORDER BY claim_id, step_no",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                let id: i32 = r.get("id");
                ExpenseClaim {
                    id,
                    claim_no: r.get("claim_no"),
                    title: r.get("title"),
                    claimant: r.get("claimant"),
                    claimant_role: r.get("claimant_role"),
                    dept_code: r.get("dept_code"),
                    dept_label: r.get("dept_label"),
                    spent_on: r.get("spent_on"),
                    amount: r.get("amount"),
                    suggested_account_code: r.get("suggested_account_code"),
                    suggested_account_name: r.get("suggested_account_name"),
                    account_code: r.get("account_code"),
                    account_name: r.get("account_name"),
                    pay_method: r.get("pay_method"),
                    status: r.get("status"),
                    voucher_no: r.get("voucher_no"),
                    voucher_status: r.get("voucher_status"),
                    document_no: r.get("doc_no"),
                    document_name: r.get("document_name"),
                    memo: r.get("memo"),
                    created_at: r.get("created_at"),
                    created_by: r.get("created_by"),
                    steps: steps
                        .iter()
                        .filter(|s| s.get::<i32, _>("claim_id") == id)
                        .map(|s| ExpenseStep {
                            step_no: s.get("step_no"),
                            decision: s.get("decision"),
                            actor: s.get("actor"),
                            actor_role: s.get("actor_role"),
                            note: s.get("note"),
                            decided_at: s.get("decided_at"),
                        })
                        .collect(),
                }
            })
            .collect())
    }

    pub async fn expense_claim(&self, claim_no: &str) -> Result<ExpenseClaim> {
        self.expense_claims(true, 1000)
            .await?
            .into_iter()
            .find(|c| c.claim_no == claim_no)
            .ok_or_else(|| StoreError::ClaimNotFound { no: claim_no.to_string() })
    }

    /// One claim belonging to one employee. The employee is part of the WHERE
    /// clause rather than a check afterwards, so another person's claim number
    /// is a missing row and not a rejected match. Claims filed by accounting
    /// carry no employee id and are therefore invisible here.
    pub async fn expense_claim_of_employee(
        &self,
        claim_no: &str,
        employee_id: i32,
    ) -> Result<ExpenseClaim> {
        let owned: Option<String> =
            sqlx::query("SELECT claim_no FROM expense_claim WHERE claim_no = $1 AND employee_id = $2")
                .bind(claim_no)
                .bind(employee_id)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| r.get("claim_no"));
        match owned {
            Some(no) => self.expense_claim(&no).await,
            None => Err(StoreError::ClaimNotFound { no: claim_no.to_string() }),
        }
    }

    /// Documents attached to one record, oldest first.
    pub async fn documents_linked_to(
        &self,
        link_type: &str,
        link_ref: &str,
    ) -> Result<Vec<DocumentRow>> {
        Ok(sqlx::query(
            "SELECT d.doc_no, d.name, d.kind, d.link_type, d.link_ref, d.archived,
                    d.note, d.created_at, d.created_by,
                    v.version_no, v.object_key, v.file_name, v.byte_size, v.content_type,
                    v.checksum, v.ocr_status, v.uploaded_at, v.uploaded_by,
                    n.versions, n.total_bytes
               FROM document d
               JOIN LATERAL (SELECT * FROM document_version dv
                              WHERE dv.document_id = d.id
                              ORDER BY dv.version_no DESC LIMIT 1) v ON true
               JOIN LATERAL (SELECT COUNT(*) AS versions, COALESCE(SUM(dv.byte_size),0)::bigint AS total_bytes
                               FROM document_version dv WHERE dv.document_id = d.id) n ON true
              WHERE d.link_type = $1 AND d.link_ref = $2 AND NOT d.archived
              ORDER BY d.id",
        )
        .bind(link_type)
        .bind(link_ref)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_document)
        .collect())
    }

    /// Point a claim at its receipt, but never replace one it already has.
    /// Later attachments stay reachable through the document's own link.
    pub async fn set_claim_document_if_absent(
        &self,
        claim_no: &str,
        document_id: i32,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE expense_claim SET document_id = $2
              WHERE claim_no = $1 AND document_id IS NULL",
        )
        .bind(claim_no)
        .bind(document_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Submit a claim. Step 1 (the claimant uploading) completes on submission.
    pub async fn submit_expense_claim(&self, req: SubmitExpenseClaim<'_>) -> Result<ExpenseClaim> {
        let dept_id = match req.dept_code {
            None => None,
            Some(code) => Some(self.department_id(code).await?),
        };

        let mut tx = self.pool.begin().await?;
        let period = req.spent_on.format("%y%m").to_string();
        let seq = next_doc_no(&mut tx, "EX", &period).await?;
        let claim_no = format!("EX{period}-{seq:03}");

        let id: i32 = sqlx::query(
            "INSERT INTO expense_claim
               (claim_no, title, claimant, claimant_role, dept_id, spent_on, amount,
                suggested_account_code, pay_method, status, document_id, memo, created_by,
                employee_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'submitted',$10,$11,$3,$12) RETURNING id",
        )
        .bind(&claim_no)
        .bind(req.title)
        .bind(req.claimant)
        .bind(req.claimant_role)
        .bind(dept_id)
        .bind(req.spent_on)
        .bind(req.amount)
        .bind(req.suggested_account_code)
        .bind(req.pay_method)
        .bind(req.document_id)
        .bind(req.memo)
        .bind(req.employee_id)
        .fetch_one(&mut *tx)
        .await?
        .get("id");

        sqlx::query(
            "INSERT INTO expense_claim_step
               (claim_id, step_no, decision, actor, actor_role, note)
             VALUES ($1, 1, 'approved', $2, $3, '上傳收據')",
        )
        .bind(id)
        .bind(req.claimant)
        .bind(req.claimant_role)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.expense_claim(&claim_no).await
    }

    /// Approve one step. A `voucher` value means this was the final step, and
    /// the voucher and approval record share the transaction.
    pub async fn approve_expense_step(
        &self,
        req: ApproveExpenseStep<'_>,
    ) -> Result<ExpenseClaim> {
        let mut tx = self.pool.begin().await?;

        // FOR UPDATE: two simultaneous approvals would otherwise create two
        // vouchers; the second waits.
        let row = sqlx::query(
            "SELECT c.id, c.status, c.amount, c.title, c.claimant, c.pay_method, c.dept_id,
                    c.spent_on, c.account_code
               FROM expense_claim c WHERE c.claim_no = $1 FOR UPDATE",
        )
        .bind(req.claim_no)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::ClaimNotFound { no: req.claim_no.to_string() })?;

        let claim_id: i32 = row.get("id");
        let status: String = row.get("status");
        if status != "submitted" && status != "approving" {
            return Err(StoreError::ClaimNotPending { no: req.claim_no.to_string(), status });
        }

        let done: bool = sqlx::query(
            "SELECT EXISTS (SELECT 1 FROM expense_claim_step
                             WHERE claim_id = $1 AND step_no = $2) AS ok",
        )
        .bind(claim_id)
        .bind(req.step_no)
        .fetch_one(&mut *tx)
        .await?
        .get("ok");
        if done {
            return Err(StoreError::StepAlreadyDecided {
                no: req.claim_no.to_string(),
                step: req.step_no,
            });
        }

        if let Some(code) = req.account_code {
            sqlx::query("UPDATE expense_claim SET account_code = $2 WHERE id = $1")
                .bind(claim_id)
                .bind(code)
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query(
            "INSERT INTO expense_claim_step (claim_id, step_no, decision, actor, actor_role, note)
             VALUES ($1,$2,'approved',$3,$4,$5)",
        )
        .bind(claim_id)
        .bind(req.step_no)
        .bind(req.actor)
        .bind(req.actor_role)
        .bind(req.note)
        .execute(&mut *tx)
        .await?;

        let mut voucher_no = None;
        if req.finalise {
            let amount: Money = row.get("amount");
            let title: String = row.get("title");
            let claimant: String = row.get("claimant");
            let pay_method: String = row.get("pay_method");
            let dept_id: Option<i32> = row.get("dept_id");
            let spent_on: NaiveDate = row.get("spent_on");
            let stored: Option<String> = row.get("account_code");
            let expense_code = req
                .account_code
                .map(str::to_string)
                .or(stored)
                .ok_or_else(|| StoreError::UnknownAccount { code: "（未指定費用科目）".into() })?;

            // Credit side: month-end reimbursement goes to payables, immediate
            // payment moves petty cash.
            let (credit_code, bank_id) = if pay_method == "petty_cash" {
                let id: Option<i32> =
                    sqlx::query("SELECT id FROM bank_account WHERE gl_code = '1101' ORDER BY id")
                        .fetch_optional(&mut *tx)
                        .await?
                        .map(|r| r.get("id"));
                ("1101", id)
            } else {
                ("2101", None)
            };

            let memo = format!("{claimant} {title}");
            let lines = vec![
                LineInput {
                    account_code: expense_code,
                    debit: amount,
                    credit: Money::ZERO,
                    memo: Some(req.claim_no.to_string()),
                    bank_account_id: None,
                },
                LineInput {
                    account_code: credit_code.into(),
                    debit: Money::ZERO,
                    credit: amount,
                    memo: Some(format!("{claimant} 報支")),
                    bank_account_id: bank_id,
                },
            ];

            let no = insert_voucher(
                &mut tx,
                req.posted_on.unwrap_or(spent_on),
                &memo,
                &lines,
                "pending",
                "expense",
                req.actor,
                None,
            )
            .await?;

            // The expense line carries the department so departmental P&L sees
            // the cost.
            set_line_dept(&mut tx, &no, 1, dept_id).await?;

            sqlx::query(
                "UPDATE expense_claim SET status = 'approved', voucher_no = $2 WHERE id = $1",
            )
            .bind(claim_id)
            .bind(&no)
            .execute(&mut *tx)
            .await?;
            voucher_no = Some(no);
        } else {
            sqlx::query("UPDATE expense_claim SET status = 'approving' WHERE id = $1")
                .bind(claim_id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        let _ = voucher_no;
        self.expense_claim(req.claim_no).await
    }

    /// Reject. The whole claim is marked rejected and the claimant submits a
    /// new one.
    ///
    /// There is no "send back to step N": a rejected claim usually has wrong
    /// data, and once corrected every earlier approver saw something
    /// different. Keeping their signatures would endorse content they never
    /// reviewed.
    pub async fn reject_expense_claim(
        &self,
        claim_no: &str,
        step_no: i32,
        actor: &str,
        actor_role: &str,
        note: &str,
    ) -> Result<ExpenseClaim> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT id, status FROM expense_claim WHERE claim_no = $1 FOR UPDATE")
            .bind(claim_no)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::ClaimNotFound { no: claim_no.to_string() })?;

        let id: i32 = row.get("id");
        let status: String = row.get("status");
        if status != "submitted" && status != "approving" {
            return Err(StoreError::ClaimNotPending { no: claim_no.to_string(), status });
        }

        sqlx::query(
            "INSERT INTO expense_claim_step (claim_id, step_no, decision, actor, actor_role, note)
             VALUES ($1,$2,'rejected',$3,$4,$5)
             ON CONFLICT (claim_id, step_no) DO UPDATE
               SET decision = 'rejected', actor = EXCLUDED.actor,
                   actor_role = EXCLUDED.actor_role, note = EXCLUDED.note,
                   decided_at = now()",
        )
        .bind(id)
        .bind(step_no)
        .bind(actor)
        .bind(actor_role)
        .bind(note)
        .execute(&mut *tx)
        .await?;

        sqlx::query("UPDATE expense_claim SET status = 'rejected' WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        self.expense_claim(claim_no).await
    }

    // Documents

    /// Document list, each with its latest revision.
    pub async fn documents(&self, q: Option<&str>, limit: i64) -> Result<Vec<DocumentRow>> {
        Ok(sqlx::query(
            "SELECT d.doc_no, d.name, d.kind, d.link_type, d.link_ref, d.archived,
                    d.note, d.created_at, d.created_by,
                    v.version_no, v.object_key, v.file_name, v.byte_size, v.content_type,
                    v.checksum, v.ocr_status, v.uploaded_at, v.uploaded_by,
                    n.versions, n.total_bytes
               FROM document d
               JOIN LATERAL (SELECT * FROM document_version dv
                              WHERE dv.document_id = d.id
                              ORDER BY dv.version_no DESC LIMIT 1) v ON true
               JOIN LATERAL (SELECT COUNT(*) AS versions, COALESCE(SUM(dv.byte_size),0)::bigint AS total_bytes
                               FROM document_version dv WHERE dv.document_id = d.id) n ON true
              WHERE ($1::text IS NULL
                     OR d.name ILIKE '%' || $1 || '%'
                     OR d.doc_no ILIKE '%' || $1 || '%'
                     OR d.link_ref ILIKE '%' || $1 || '%'
                     OR v.file_name ILIKE '%' || $1 || '%')
              ORDER BY v.uploaded_at DESC, d.id DESC
              LIMIT $2",
        )
        .bind(q)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_document)
        .collect())
    }

    /// Database id for a document number. Not found is an error.
    pub async fn document_id(&self, doc_no: &str) -> Result<i32> {
        sqlx::query("SELECT id FROM document WHERE doc_no = $1")
            .bind(doc_no)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| r.get("id"))
            .ok_or_else(|| StoreError::DocumentNotFound { no: doc_no.to_string() })
    }

    /// One revision of a document. Omitting `version` gives the latest.
    pub async fn document_version(
        &self,
        doc_no: &str,
        version: Option<i32>,
    ) -> Result<DocumentRow> {
        let row = sqlx::query(
            "SELECT d.doc_no, d.name, d.kind, d.link_type, d.link_ref, d.archived,
                    d.note, d.created_at, d.created_by,
                    v.version_no, v.object_key, v.file_name, v.byte_size, v.content_type,
                    v.checksum, v.ocr_status, v.uploaded_at, v.uploaded_by,
                    n.versions, n.total_bytes
               FROM document d
               JOIN document_version v ON v.document_id = d.id
               JOIN LATERAL (SELECT COUNT(*) AS versions, COALESCE(SUM(dv.byte_size),0)::bigint AS total_bytes
                               FROM document_version dv WHERE dv.document_id = d.id) n ON true
              WHERE d.doc_no = $1 AND ($2::int IS NULL OR v.version_no = $2)
              ORDER BY v.version_no DESC LIMIT 1",
        )
        .bind(doc_no)
        .bind(version)
        .fetch_optional(&self.pool)
        .await?;

        match (row, version) {
            (Some(r), _) => Ok(row_to_document(&r)),
            (None, Some(v)) => {
                Err(StoreError::DocumentVersionNotFound { no: doc_no.to_string(), version: v })
            }
            (None, None) => Err(StoreError::DocumentNotFound { no: doc_no.to_string() }),
        }
    }

    /// Allocate a number and revision for a document, with no content yet.
    ///
    /// Two steps are deliberate: the number and revision are needed to build
    /// the object key, and `document_version` is written only after the
    /// upload succeeds. The other order leaves either an object the database
    /// does not know about, or a row pointing at a file that does not exist.
    /// The latter is worse, because it looks like evidence.
    pub async fn reserve_document_slot(
        &self,
        req: ReserveDocument<'_>,
    ) -> Result<DocumentSlot> {
        let mut tx = self.pool.begin().await?;

        let existing: Option<(i32, String, i32)> = match req.doc_no {
            Some(no) => sqlx::query(
                "SELECT d.id, d.doc_no,
                        COALESCE((SELECT MAX(version_no) FROM document_version
                                   WHERE document_id = d.id), 0) AS last
                   FROM document d WHERE d.doc_no = $1 FOR UPDATE",
            )
            .bind(no)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| (r.get("id"), r.get("doc_no"), r.get("last"))),
            None => None,
        };

        let (doc_id, doc_no, version_no) = match existing {
            Some((id, no, last)) => (id, no, last + 1),
            None => {
                if let Some(no) = req.doc_no {
                    return Err(StoreError::DocumentNotFound { no: no.to_string() });
                }
                let period = req.today.format("%y%m").to_string();
                let seq = next_doc_no(&mut tx, "D", &period).await?;
                let no = format!("D{period}-{seq:03}");
                let id: i32 = sqlx::query(
                    "INSERT INTO document (doc_no, name, kind, link_type, link_ref, note, created_by)
                     VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
                )
                .bind(&no)
                .bind(req.name)
                .bind(req.kind)
                .bind(req.link_type)
                .bind(req.link_ref)
                .bind(req.note)
                .bind(req.actor)
                .fetch_one(&mut *tx)
                .await?
                .get("id");
                (id, no, 1)
            }
        };

        tx.commit().await?;
        Ok(DocumentSlot { document_id: doc_id, doc_no, version_no })
    }

    /// The file is in the bucket; register it.
    pub async fn record_document_version(
        &self,
        slot: &DocumentSlot,
        object: &storage::StoredObject,
        file_name: &str,
        ocr_status: &str,
        actor: &str,
    ) -> Result<DocumentRow> {
        sqlx::query(
            "INSERT INTO document_version
               (document_id, version_no, object_key, file_name, byte_size, content_type,
                checksum, ocr_status, uploaded_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(slot.document_id)
        .bind(slot.version_no)
        .bind(&object.key)
        .bind(file_name)
        .bind(object.byte_size)
        .bind(&object.content_type)
        .bind(&object.checksum)
        .bind(ocr_status)
        .bind(actor)
        .execute(&self.pool)
        .await?;
        self.document_version(&slot.doc_no, Some(slot.version_no)).await
    }

    /// Document statistics: documents, revisions, bytes, and how many await
    /// OCR.
    ///
    /// The byte count is the database side's sum; the bucket side is measured
    /// separately by `ObjectStore::usage`. Reporting both makes a mismatch
    /// visible.
    pub async fn document_stats(&self) -> Result<DocumentStats> {
        let r = sqlx::query(
            "SELECT
               (SELECT COUNT(*) FROM document)                                    AS documents,
               (SELECT COUNT(*) FROM document_version)                            AS versions,
               (SELECT COALESCE(SUM(byte_size),0)::bigint FROM document_version)   AS bytes,
               (SELECT COUNT(*) FROM document_version WHERE ocr_status = 'pending') AS ocr_pending,
               (SELECT COUNT(*) FROM document WHERE archived)                      AS archived,
               (SELECT COUNT(*) FROM voucher v
                 WHERE v.status IN ('pending','posted')
                   AND NOT EXISTS (SELECT 1 FROM document d
                                    WHERE d.link_type = 'voucher' AND d.link_ref = v.no))
                                                                                   AS vouchers_without_doc",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(DocumentStats {
            documents: r.get("documents"),
            versions: r.get("versions"),
            bytes: r.get("bytes"),
            ocr_pending: r.get("ocr_pending"),
            archived: r.get("archived"),
            vouchers_without_doc: r.get("vouchers_without_doc"),
        })
    }

    // Imports

    pub async fn import_sources(&self) -> Result<Vec<ImportSource>> {
        Ok(sqlx::query(
            "SELECT s.code, s.label, s.format, s.cadence, s.supported, s.note,
                    b.last_at, b.last_status, b.last_batch_no
               FROM import_source s
               LEFT JOIN LATERAL (
                   SELECT ib.created_at AS last_at, ib.status AS last_status,
                          ib.batch_no AS last_batch_no
                     FROM import_batch ib
                    WHERE ib.source_code = s.code AND ib.status <> 'discarded'
                    ORDER BY ib.id DESC LIMIT 1) b ON true
              ORDER BY s.sort_order, s.code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| ImportSource {
            code: r.get("code"),
            label: r.get("label"),
            format: r.get("format"),
            cadence: r.get("cadence"),
            supported: r.get("supported"),
            note: r.get("note"),
            last_at: r.get("last_at"),
            last_status: r.get("last_status"),
            last_batch_no: r.get("last_batch_no"),
        })
        .collect())
    }

    pub async fn integrations(&self) -> Result<Vec<Integration>> {
        Ok(sqlx::query(
            "SELECT code, label, detail, live, note FROM integration ORDER BY sort_order, code",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| Integration {
            code: r.get("code"),
            label: r.get("label"),
            detail: r.get("detail"),
            live: r.get("live"),
            note: r.get("note"),
        })
        .collect())
    }

    pub async fn import_batches(&self, limit: i64) -> Result<Vec<ImportBatch>> {
        let rows = sqlx::query(
            "SELECT b.id, b.batch_no, b.source_code, s.label AS source_label, b.file_name,
                    b.row_count, b.ok_count, b.error_count, b.warn_count, b.status,
                    b.voucher_nos, b.created_at, b.created_by, b.settled_at, b.settled_by
               FROM import_batch b JOIN import_source s ON s.code = b.source_code
              ORDER BY b.id DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let issues = sqlx::query(
            "SELECT batch_id, row_no, severity, issue, fix FROM import_issue
              ORDER BY batch_id, row_no, id",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(|r| row_to_import_batch(r, &issues)).collect())
    }

    pub async fn import_batch(&self, batch_no: &str) -> Result<ImportBatch> {
        self.import_batches(200)
            .await?
            .into_iter()
            .find(|b| b.batch_no == batch_no)
            .ok_or_else(|| StoreError::ImportBatchNotFound { no: batch_no.to_string() })
    }

    /// Store the result of one dry run. Writes no vouchers; this step only
    /// records what was seen.
    pub async fn create_import_batch(&self, req: CreateImportBatch<'_>) -> Result<ImportBatch> {
        let known: bool =
            sqlx::query("SELECT EXISTS (SELECT 1 FROM import_source WHERE code = $1) AS ok")
                .bind(req.source_code)
                .fetch_one(&self.pool)
                .await?
                .get("ok");
        if !known {
            return Err(StoreError::ImportSourceNotFound { code: req.source_code.to_string() });
        }

        let mut tx = self.pool.begin().await?;
        let period = req.today.format("%y%m").to_string();
        let seq = next_doc_no(&mut tx, "IM", &period).await?;
        let batch_no = format!("IM{period}-{seq:03}");

        let id: i32 = sqlx::query(
            "INSERT INTO import_batch
               (batch_no, source_code, file_name, row_count, ok_count, error_count, warn_count,
                status, payload, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'preview',$8,$9) RETURNING id",
        )
        .bind(&batch_no)
        .bind(req.source_code)
        .bind(req.file_name)
        .bind(req.row_count)
        .bind(req.ok_count)
        .bind(req.error_count)
        .bind(req.warn_count)
        .bind(req.payload)
        .bind(req.actor)
        .fetch_one(&mut *tx)
        .await?
        .get("id");

        for issue in req.issues {
            sqlx::query(
                "INSERT INTO import_issue (batch_id, row_no, severity, issue, fix)
                 VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(id)
            .bind(issue.row_no)
            .bind(&issue.severity)
            .bind(&issue.issue)
            .bind(&issue.fix)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        self.import_batch(&batch_no).await
    }

    /// A batch's payload: the voucher drafts parsed by the dry run.
    pub async fn import_payload(&self, batch_no: &str) -> Result<(String, serde_json::Value)> {
        let r = sqlx::query("SELECT status, payload FROM import_batch WHERE batch_no = $1")
            .bind(batch_no)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::ImportBatchNotFound { no: batch_no.to_string() })?;
        Ok((r.get("status"), r.get("payload")))
    }

    /// Perform the write. Vouchers are always `pending`: an import does not
    /// post, a person does.
    pub async fn commit_import_batch(
        &self,
        batch_no: &str,
        drafts: &[ImportVoucherDraft],
        actor: &str,
    ) -> Result<ImportBatch> {
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query("SELECT id, status FROM import_batch WHERE batch_no = $1 FOR UPDATE")
            .bind(batch_no)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::ImportBatchNotFound { no: batch_no.to_string() })?;
        let id: i32 = row.get("id");
        let status: String = row.get("status");
        if status != "preview" {
            return Err(StoreError::ImportNotPreview { no: batch_no.to_string(), status });
        }

        let mut nos: Vec<String> = Vec::with_capacity(drafts.len());
        for d in drafts {
            let lines: Vec<LineInput> = d
                .lines
                .iter()
                .map(|l| LineInput {
                    account_code: l.account_code.clone(),
                    debit: l.debit,
                    credit: l.credit,
                    memo: l.memo.clone(),
                    bank_account_id: None,
                })
                .collect();
            let no =
                insert_voucher(&mut tx, d.date, &d.memo, &lines, "pending", "import", actor, None)
                    .await?;
            for (i, l) in d.lines.iter().enumerate() {
                if l.dept_id.is_some() {
                    set_line_dept(&mut tx, &no, i as i32 + 1, l.dept_id).await?;
                }
            }
            nos.push(no);
        }

        sqlx::query(
            "UPDATE import_batch
                SET status = 'committed', voucher_nos = $2, settled_at = now(), settled_by = $3
              WHERE id = $1",
        )
        .bind(id)
        .bind(&nos)
        .bind(actor)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.import_batch(batch_no).await
    }

    /// Roll back a batch. Only unposted vouchers can be deleted; posted ones
    /// can be corrected only by a reversing entry.
    pub async fn rollback_import_batch(
        &self,
        batch_no: &str,
        actor: &str,
    ) -> Result<ImportBatch> {
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query(
            "SELECT id, status, voucher_nos FROM import_batch WHERE batch_no = $1 FOR UPDATE",
        )
        .bind(batch_no)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::ImportBatchNotFound { no: batch_no.to_string() })?;

        let id: i32 = row.get("id");
        let status: String = row.get("status");
        if status != "committed" {
            return Err(StoreError::ImportNotCommitted { no: batch_no.to_string(), status });
        }
        let nos: Vec<String> = row.get("voucher_nos");

        let posted: Vec<String> = sqlx::query(
            "SELECT no FROM voucher WHERE no = ANY($1) AND status = 'posted' ORDER BY no",
        )
        .bind(&nos)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(|r| r.get("no"))
        .collect();
        if !posted.is_empty() {
            return Err(StoreError::ImportHasPostedVouchers {
                no: batch_no.to_string(),
                count: posted.len(),
                vouchers: short_list(&posted),
            });
        }

        sqlx::query("DELETE FROM voucher WHERE no = ANY($1)")
            .bind(&nos)
            .execute(&mut *tx)
            .await?;

        sqlx::query(
            "UPDATE import_batch
                SET status = 'rolled_back', settled_at = now(), settled_by = $2
              WHERE id = $1",
        )
        .bind(id)
        .bind(actor)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.import_batch(batch_no).await
    }

    /// System status as far as the database can see it.
    ///
    /// The brand fingerprint and RustFS connectivity are not here; `/healthz`
    /// answers those, because only the server process knows what it verified
    /// at startup. The screen reads both and labels each source.
    pub async fn system_stats(&self) -> Result<SystemStats> {
        let r = sqlx::query(
            "SELECT
               (SELECT COUNT(*) FROM voucher WHERE status = 'posted')            AS posted,
               (SELECT COUNT(*) FROM voucher WHERE status IN ('draft','pending')) AS unposted,
               (SELECT COUNT(*) FROM account)                                     AS accounts,
               (SELECT COUNT(*) FROM ai_audit_log)                                AS audit_entries,
               (SELECT COUNT(*) FROM ai_audit_log WHERE outcome = 'blocked')      AS audit_blocked,
               (SELECT MIN(at) FROM ai_audit_log)                                 AS audit_since,
               (SELECT COUNT(*) FROM api_token WHERE revoked_at IS NULL)          AS tokens_live,
               (SELECT COUNT(*) FROM api_token WHERE revoked_at IS NOT NULL)      AS tokens_revoked,
               (SELECT COUNT(*) FROM reg_ruleset)                                 AS rulesets,
               (SELECT COUNT(*) FROM mcp_tool_grant WHERE enabled)                AS tools_on,
               (SELECT COUNT(*) FROM mcp_tool_grant)                              AS tools_total",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(SystemStats {
            posted_vouchers: r.get("posted"),
            unposted_vouchers: r.get("unposted"),
            accounts: r.get("accounts"),
            audit_entries: r.get("audit_entries"),
            audit_blocked: r.get("audit_blocked"),
            audit_since: r.get("audit_since"),
            tokens_live: r.get("tokens_live"),
            tokens_revoked: r.get("tokens_revoked"),
            rulesets: r.get("rulesets"),
            tools_enabled: r.get("tools_on"),
            tools_total: r.get("tools_total"),
        })
    }

    /// The first bank account under a given GL account.
    ///
    /// "Which account payroll is paid from" is a setting and must not be a
    /// hard-coded id, but neither should the user have to specify it every
    /// time. Finding none is a configuration error and returns an error
    /// rather than picking one arbitrarily.
    pub async fn primary_bank_account(&self, gl_code: &str) -> Result<i32> {
        sqlx::query(
            "SELECT id FROM bank_account WHERE gl_code = $1 ORDER BY sort_order, id LIMIT 1",
        )
        .bind(gl_code)
        .fetch_optional(&self.pool)
        .await?
        .map(|r| r.get("id"))
        .ok_or_else(|| StoreError::UnknownAccount { code: gl_code.to_string() })
    }

    // HR: employees and movements
    //
    // This section only moves data. Premiums, leave entitlement and the
    // minimum wage check all happen in laysi-ops via `laysi-regulation`; the
    // repository must not become a second opinion.

    /// Create an employee record. The three insured brackets are kept exactly
    /// as filed and are never validated, corrected or truncated against a
    /// ruleset here; rates and ceilings are the regulated part.
    pub async fn create_employee(&self, req: WriteEmployee<'_>) -> Result<Employee> {
        let mut tx = self.pool.begin().await?;
        let dept_id = resolve_department_id(&mut tx, req.dept_code).await?;
        ensure_role_exists(&mut tx, req.role_code).await?;

        sqlx::query(
            "INSERT INTO employee
                (emp_no, name, dept_id, title, role_code, employment_type,
                 hired_on, left_on, status, base_salary, labor_insured_salary,
                 health_insured_salary, pension_wage, id_no, bank_account,
                 note, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(req.emp_no)
        .bind(req.name)
        .bind(dept_id)
        .bind(req.title)
        .bind(req.role_code)
        .bind(req.employment_type)
        .bind(req.hired_on)
        .bind(req.left_on)
        .bind(req.status)
        .bind(req.base_salary)
        .bind(req.labor_insured_salary)
        .bind(req.health_insured_salary)
        .bind(req.pension_wage)
        .bind(req.id_no)
        .bind(req.bank_account)
        .bind(req.note)
        .bind(req.actor)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.employee(req.emp_no).await
    }

    /// Full update of an employee record. Neither creating nor updating an
    /// employee creates a login; `user_account` stays with `laysi-admin`, and
    /// the two tables are deliberately separate.
    pub async fn update_employee(&self, req: WriteEmployee<'_>) -> Result<Employee> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query("SELECT 1 FROM employee WHERE emp_no = $1 FOR UPDATE")
            .bind(req.emp_no)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
        if !exists {
            return Err(StoreError::EmployeeNotFound { no: req.emp_no.to_string() });
        }

        let dept_id = resolve_department_id(&mut tx, req.dept_code).await?;
        ensure_role_exists(&mut tx, req.role_code).await?;
        sqlx::query(
            "UPDATE employee
                SET name = $2, dept_id = $3, title = $4, role_code = $5,
                    employment_type = $6, hired_on = $7, left_on = $8,
                    status = $9, base_salary = $10,
                    labor_insured_salary = $11, health_insured_salary = $12,
                    pension_wage = $13, id_no = $14, bank_account = $15,
                    note = $16
              WHERE emp_no = $1",
        )
        .bind(req.emp_no)
        .bind(req.name)
        .bind(dept_id)
        .bind(req.title)
        .bind(req.role_code)
        .bind(req.employment_type)
        .bind(req.hired_on)
        .bind(req.left_on)
        .bind(req.status)
        .bind(req.base_salary)
        .bind(req.labor_insured_salary)
        .bind(req.health_insured_salary)
        .bind(req.pension_wage)
        .bind(req.id_no)
        .bind(req.bank_account)
        .bind(req.note)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.employee(req.emp_no).await
    }

    pub async fn employees(&self, include_left: bool, q: Option<&str>) -> Result<Vec<Employee>> {
        Ok(sqlx::query(
            "SELECT e.id, e.emp_no, e.name, e.dept_id, d.code AS dept_code, d.label AS dept_label,
                    e.title, e.role_code, r.name AS role_name, e.employment_type,
                    e.hired_on, e.left_on, e.status, e.base_salary,
                    e.labor_insured_salary, e.health_insured_salary, e.pension_wage,
                    e.id_no, e.bank_account, e.note
               FROM employee e
               LEFT JOIN dimension_value d ON d.id = e.dept_id
               LEFT JOIN role r           ON r.code = e.role_code
              WHERE ($1 OR e.status <> 'left')
                AND ($2::text IS NULL OR e.name ILIKE '%' || $2 || '%'
                                      OR e.emp_no ILIKE '%' || $2 || '%'
                                      OR e.title ILIKE '%' || $2 || '%'
                                      OR COALESCE(d.label,'') ILIKE '%' || $2 || '%')
              ORDER BY e.emp_no",
        )
        .bind(include_left)
        .bind(q)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_employee)
        .collect())
    }

    pub async fn employee(&self, emp_no: &str) -> Result<Employee> {
        sqlx::query(
            "SELECT e.id, e.emp_no, e.name, e.dept_id, d.code AS dept_code, d.label AS dept_label,
                    e.title, e.role_code, r.name AS role_name, e.employment_type,
                    e.hired_on, e.left_on, e.status, e.base_salary,
                    e.labor_insured_salary, e.health_insured_salary, e.pension_wage,
                    e.id_no, e.bank_account, e.note
               FROM employee e
               LEFT JOIN dimension_value d ON d.id = e.dept_id
               LEFT JOIN role r           ON r.code = e.role_code
              WHERE e.emp_no = $1",
        )
        .bind(emp_no)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_employee)
        .ok_or_else(|| StoreError::EmployeeNotFound { no: emp_no.to_string() })
    }

    /// Find an employee by the primary key linked to the account. Self-service
    /// must never guess an identity from a name, which may repeat.
    pub async fn employee_by_id(&self, id: i32) -> Result<Option<Employee>> {
        Ok(sqlx::query(
            "SELECT e.id, e.emp_no, e.name, e.dept_id, d.code AS dept_code, d.label AS dept_label,
                    e.title, e.role_code, r.name AS role_name, e.employment_type,
                    e.hired_on, e.left_on, e.status, e.base_salary,
                    e.labor_insured_salary, e.health_insured_salary, e.pension_wage,
                    e.id_no, e.bank_account, e.note
               FROM employee e
               LEFT JOIN dimension_value d ON d.id = e.dept_id
               LEFT JOIN role r           ON r.code = e.role_code
              WHERE e.id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(row_to_employee))
    }

    /// The steps required by hire, leave, pay change and transfer. The flow is
    /// data, not code.
    pub async fn hr_flow_steps(&self) -> Result<Vec<HrFlowStep>> {
        Ok(sqlx::query(
            "SELECT kind, step_no, label, note FROM hr_flow_step ORDER BY kind, step_no",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| HrFlowStep {
            kind: r.get("kind"),
            step_no: r.get("step_no"),
            label: r.get("label"),
            note: r.get("note"),
        })
        .collect())
    }

    pub async fn employee_movements(&self, limit: i64) -> Result<Vec<EmployeeMovement>> {
        let rows = sqlx::query(
            "SELECT m.id, m.movement_no, m.employee_id, e.emp_no, e.name,
                    d.label AS dept_label, m.kind, m.effective_on,
                    m.from_value, m.to_value, m.reason, m.status, m.created_at, m.created_by
               FROM employee_movement m
               JOIN employee e            ON e.id = m.employee_id
               LEFT JOIN dimension_value d ON d.id = e.dept_id
              ORDER BY m.effective_on DESC, m.id DESC
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let steps = sqlx::query(
            "SELECT movement_id, step_no, done_at, done_by, note
               FROM employee_movement_step ORDER BY movement_id, step_no",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                let id: i32 = r.get("id");
                EmployeeMovement {
                    id,
                    movement_no: r.get("movement_no"),
                    emp_no: r.get("emp_no"),
                    name: r.get("name"),
                    dept_label: r.get("dept_label"),
                    kind: r.get("kind"),
                    effective_on: r.get("effective_on"),
                    from_value: r.get("from_value"),
                    to_value: r.get("to_value"),
                    reason: r.get("reason"),
                    status: r.get("status"),
                    created_at: r.get("created_at"),
                    created_by: r.get("created_by"),
                    done_steps: steps
                        .iter()
                        .filter(|s| s.get::<i32, _>("movement_id") == id)
                        .map(|s| MovementStep {
                            step_no: s.get("step_no"),
                            done_at: s.get("done_at"),
                            done_by: s.get("done_by"),
                            note: s.get("note"),
                        })
                        .collect(),
                }
            })
            .collect())
    }

    pub async fn employee_movement(&self, movement_no: &str) -> Result<EmployeeMovement> {
        self.employee_movements(1000)
            .await?
            .into_iter()
            .find(|m| m.movement_no == movement_no)
            .ok_or_else(|| StoreError::MovementNotFound { no: movement_no.to_string() })
    }

    /// Open a movement request. Numbered `MV{yymm}-{nnn}`, sharing
    /// `doc_sequence` with vouchers.
    pub async fn record_employee_movement(
        &self,
        req: RecordMovement<'_>,
    ) -> Result<EmployeeMovement> {
        let mut tx = self.pool.begin().await?;

        let employee_id: i32 = sqlx::query("SELECT id FROM employee WHERE emp_no = $1")
            .bind(req.emp_no)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::EmployeeNotFound { no: req.emp_no.to_string() })?
            .get("id");

        let period = req.effective_on.format("%y%m").to_string();
        let seq = next_doc_no(&mut tx, "MV", &period).await?;
        let movement_no = format!("MV{period}-{seq:03}");

        sqlx::query(
            "INSERT INTO employee_movement
               (movement_no, employee_id, kind, effective_on, from_value, to_value,
                reason, status, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'open',$8)",
        )
        .bind(&movement_no)
        .bind(employee_id)
        .bind(req.kind)
        .bind(req.effective_on)
        .bind(req.from_value)
        .bind(req.to_value)
        .bind(req.reason)
        .bind(req.actor)
        .execute(&mut *tx)
        .await?;

        // Raising a leaving request immediately marks the employee as leaving
        // and records the planned date: the roster and payroll must see it now,
        // not only when the final step completes.
        if req.kind == "leave" {
            sqlx::query(
                "UPDATE employee SET status = 'leaving', left_on = $2 WHERE id = $1",
            )
            .bind(employee_id)
            .bind(req.effective_on)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        self.employee_movement(&movement_no).await
    }

    /// Mark one step complete. The movement closes automatically once every
    /// step is done.
    pub async fn complete_movement_step(
        &self,
        movement_no: &str,
        step_no: i32,
        actor: &str,
        note: &str,
    ) -> Result<EmployeeMovement> {
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query(
            "SELECT m.id, m.kind, m.status, m.employee_id, m.effective_on
               FROM employee_movement m WHERE m.movement_no = $1 FOR UPDATE",
        )
        .bind(movement_no)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::MovementNotFound { no: movement_no.to_string() })?;

        let id: i32 = row.get("id");
        let kind: String = row.get("kind");
        let employee_id: i32 = row.get("employee_id");
        let effective_on: NaiveDate = row.get("effective_on");

        let total: i64 = sqlx::query("SELECT COUNT(*)::bigint AS n FROM hr_flow_step WHERE kind = $1")
            .bind(&kind)
            .fetch_one(&mut *tx)
            .await?
            .get("n");
        if step_no < 1 || i64::from(step_no) > total {
            return Err(StoreError::NoSuchFlowStep { kind, step_no, total });
        }

        let inserted = sqlx::query(
            "INSERT INTO employee_movement_step (movement_id, step_no, done_by, note)
             VALUES ($1,$2,$3,$4) ON CONFLICT (movement_id, step_no) DO NOTHING",
        )
        .bind(id)
        .bind(step_no)
        .bind(actor)
        .bind(note)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 0 {
            return Err(StoreError::MovementStepAlreadyDone {
                no: movement_no.to_string(),
                step: step_no,
            });
        }

        let done: i64 =
            sqlx::query("SELECT COUNT(*)::bigint AS n FROM employee_movement_step WHERE movement_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?
                .get("n");

        if done >= total {
            sqlx::query("UPDATE employee_movement SET status = 'done' WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            // The employee becomes 'left' only when the leaving flow completes.
            if kind == "leave" {
                sqlx::query("UPDATE employee SET status = 'left', left_on = $2 WHERE id = $1")
                    .bind(employee_id)
                    .bind(effective_on)
                    .execute(&mut *tx)
                    .await?;
            }
        }

        tx.commit().await?;
        self.employee_movement(movement_no).await
    }

    // HR: attendance and leave

    pub async fn attendance(&self, month: &str) -> Result<Vec<AttendanceRow>> {
        Ok(sqlx::query(
            "SELECT a.employee_id, e.emp_no, e.name, d.label AS dept_label, e.title,
                    a.month, a.required_days, a.worked_days, a.leave_days,
                    a.overtime_hours, a.overtime_pay, a.late_count, a.note
               FROM attendance a
               JOIN employee e            ON e.id = a.employee_id
               LEFT JOIN dimension_value d ON d.id = e.dept_id
              WHERE a.month = $1
              ORDER BY e.emp_no",
        )
        .bind(month)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| AttendanceRow {
            employee_id: r.get("employee_id"),
            emp_no: r.get("emp_no"),
            name: r.get("name"),
            dept_label: r.get("dept_label"),
            title: r.get("title"),
            month: r.get("month"),
            required_days: r.get("required_days"),
            worked_days: r.get("worked_days"),
            leave_days: r.get("leave_days"),
            overtime_hours: r.get("overtime_hours"),
            overtime_pay: r.get("overtime_pay"),
            late_count: r.get("late_count"),
            note: r.get("note"),
        })
        .collect())
    }

    /// Which months have attendance data, so the month picker does not guess.
    pub async fn attendance_months(&self) -> Result<Vec<String>> {
        Ok(sqlx::query("SELECT DISTINCT month FROM attendance ORDER BY month DESC")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| r.get("month"))
            .collect())
    }

    pub async fn leave_requests(
        &self,
        include_done: bool,
        limit: i64,
    ) -> Result<Vec<LeaveRequest>> {
        self.leave_requests_filtered(include_done, limit, None).await
    }

    /// All leave requests for one employee, including decided history.
    pub async fn leave_requests_by_employee(
        &self,
        employee_id: i32,
    ) -> Result<Vec<LeaveRequest>> {
        self.leave_requests_filtered(true, i64::MAX, Some(employee_id))
            .await
    }

    async fn leave_requests_filtered(
        &self,
        include_done: bool,
        limit: i64,
        employee_id: Option<i32>,
    ) -> Result<Vec<LeaveRequest>> {
        Ok(sqlx::query(
            "SELECT l.id, l.leave_no, l.employee_id, e.emp_no, e.name,
                    d.label AS dept_label, l.kind, l.from_on, l.to_on, l.days, l.reason,
                    l.status, l.decided_by, l.decided_role, l.decided_at, l.decision_note,
                    l.created_at, l.created_by
               FROM leave_request l
               JOIN employee e            ON e.id = l.employee_id
               LEFT JOIN dimension_value d ON d.id = e.dept_id
              WHERE ($1 OR l.status = 'pending')
                AND ($3::int IS NULL OR l.employee_id = $3)
              ORDER BY l.from_on DESC, l.id DESC
              LIMIT $2",
        )
        .bind(include_done)
        .bind(limit)
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_leave)
        .collect())
    }

    /// Annual leave days already taken by each employee in a year.
    ///
    /// The entitlement is not in the database: it is derived from service
    /// length and the `annual_leave` rules, changes every year, and storing it
    /// would leave a copy that drifts from the law. Days taken are the fact.
    pub async fn annual_leave_taken(&self, year: i32) -> Result<Vec<(i32, Money)>> {
        Ok(sqlx::query(
            "SELECT employee_id, COALESCE(SUM(days), 0)::numeric AS taken
               FROM leave_request
              WHERE kind = 'annual' AND status = 'approved'
                AND EXTRACT(YEAR FROM from_on) = $1
              GROUP BY employee_id",
        )
        .bind(year)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| (r.get("employee_id"), r.get("taken")))
        .collect())
    }

    /// Raise a pending leave request on the employee's own behalf.
    ///
    /// The number and the record are written in one transaction; `status` is
    /// fixed at `pending` and every `decided_*` column keeps the table's
    /// default null. Requester and creator both come from the single employee
    /// resolved by the operation layer.
    pub async fn create_leave_request(
        &self,
        req: NewLeaveRequest<'_>,
    ) -> Result<LeaveRequest> {
        let mut tx = self.pool.begin().await?;
        let period = roc_month(req.from_on);
        let seq = next_doc_no(&mut tx, "LV", &period).await?;
        let leave_no = format!("LV{period}-{seq:03}");

        sqlx::query(
            "INSERT INTO leave_request
               (leave_no, employee_id, kind, from_on, to_on, days, reason, status, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'pending',$8)",
        )
        .bind(&leave_no)
        .bind(req.employee_id)
        .bind(req.kind)
        .bind(req.from_on)
        .bind(req.to_on)
        .bind(req.days)
        .bind(req.reason)
        .bind(req.actor)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.leave_requests_by_employee(req.employee_id)
            .await?
            .into_iter()
            .find(|request| request.leave_no == leave_no)
            .ok_or(StoreError::LeaveNotFound { no: leave_no })
    }

    /// Every punch of one month, optionally narrowed to one employee.
    ///
    /// Ordered so that a day's arrival always precedes its departure, which
    /// is what the pairing in `ops/attendance.rs` walks over.
    pub async fn punches(
        &self,
        month: &str,
        employee_id: Option<i32>,
    ) -> Result<Vec<AttendancePunch>> {
        Ok(sqlx::query(
            "SELECT p.id, p.employee_id, e.emp_no, e.name, d.label AS dept_label,
                    p.punched_on, p.punched_at, p.kind, p.source, p.note, p.created_by
               FROM attendance_punch p
               JOIN employee e             ON e.id = p.employee_id
               LEFT JOIN dimension_value d ON d.id = e.dept_id
              WHERE to_char(p.punched_on, 'YYYY-MM') = $1
                AND ($2::int IS NULL OR p.employee_id = $2)
              ORDER BY e.emp_no, p.punched_on, p.kind",
        )
        .bind(month)
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(punch_row)
        .collect())
    }

    /// Record one punch.
    ///
    /// The two orderings a UNIQUE constraint cannot express are checked here
    /// inside the transaction: a day may not have two arrivals, and a
    /// departure may not precede the arrival it belongs to. `replace` is what
    /// separates an employee pressing the button, which must refuse to
    /// overwrite, from HR correcting a missed punch, which exists to.
    pub async fn record_punch(&self, p: NewPunch<'_>, replace: bool) -> Result<AttendancePunch> {
        let mut tx = self.pool.begin().await?;

        let existing = sqlx::query(
            "SELECT kind, punched_at FROM attendance_punch
              WHERE employee_id = $1 AND punched_on = $2
              FOR UPDATE",
        )
        .bind(p.employee_id)
        .bind(p.punched_on)
        .fetch_all(&mut *tx)
        .await?;
        let at_of = |kind: &str| -> Option<DateTime<Utc>> {
            existing
                .iter()
                .find(|row| row.get::<String, _>("kind") == kind)
                .map(|row| row.get("punched_at"))
        };
        let local = |at: DateTime<Utc>| laysi_domain::business_time(at).format("%H:%M:%S").to_string();

        if !replace {
            if let Some(at) = at_of(p.kind) {
                return Err(StoreError::PunchAlreadyRecorded {
                    date: p.punched_on.to_string(),
                    kind: if p.kind == "in" { "上班".into() } else { "下班".into() },
                    at: local(at),
                });
            }
        }

        match p.kind {
            "out" => {
                let Some(in_at) = at_of("in") else {
                    return Err(StoreError::PunchOutWithoutIn {
                        date: p.punched_on.to_string(),
                    });
                };
                if p.punched_at < in_at {
                    return Err(StoreError::PunchOutBeforeIn {
                        out: local(p.punched_at),
                        in_at: local(in_at),
                    });
                }
            }
            // Correcting an arrival to after the departure already recorded
            // would leave the day with negative hours.
            _ => {
                if let Some(out_at) = at_of("out") {
                    if p.punched_at > out_at {
                        return Err(StoreError::PunchOutBeforeIn {
                            out: local(out_at),
                            in_at: local(p.punched_at),
                        });
                    }
                }
            }
        }

        sqlx::query(
            "INSERT INTO attendance_punch
               (employee_id, punched_on, punched_at, kind, source, note, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (employee_id, punched_on, kind) DO UPDATE
                SET punched_at = EXCLUDED.punched_at,
                    source     = EXCLUDED.source,
                    note       = EXCLUDED.note,
                    created_by = EXCLUDED.created_by,
                    created_at = now()",
        )
        .bind(p.employee_id)
        .bind(p.punched_on)
        .bind(p.punched_at)
        .bind(p.kind)
        .bind(p.source)
        .bind(p.note)
        .bind(p.actor)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let month = p.punched_on.format("%Y-%m").to_string();
        self.punches(&month, Some(p.employee_id))
            .await?
            .into_iter()
            .find(|row| row.punched_on == p.punched_on && row.kind == p.kind)
            .ok_or(StoreError::PunchOutWithoutIn { date: p.punched_on.to_string() })
    }

    pub async fn decide_leave_request(
        &self,
        leave_no: &str,
        approve: bool,
        actor: &str,
        actor_role: &str,
        note: &str,
    ) -> Result<LeaveRequest> {
        let status: String = sqlx::query("SELECT status FROM leave_request WHERE leave_no = $1")
            .bind(leave_no)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::LeaveNotFound { no: leave_no.to_string() })?
            .get("status");
        if status != "pending" {
            return Err(StoreError::LeaveAlreadyDecided { no: leave_no.to_string(), status });
        }

        sqlx::query(
            "UPDATE leave_request
                SET status = CASE WHEN $2 THEN 'approved' ELSE 'rejected' END,
                    decided_by = $3, decided_role = $4, decided_at = now(), decision_note = $5
              WHERE leave_no = $1",
        )
        .bind(leave_no)
        .bind(approve)
        .bind(actor)
        .bind(actor_role)
        .bind(note)
        .execute(&self.pool)
        .await?;

        self.leave_requests(true, 1000)
            .await?
            .into_iter()
            .find(|l| l.leave_no == leave_no)
            .ok_or_else(|| StoreError::LeaveNotFound { no: leave_no.to_string() })
    }

    // HR: payroll

    pub async fn payroll_runs(&self, limit: i64) -> Result<Vec<PayrollRun>> {
        Ok(sqlx::query(
            "SELECT p.period, p.pay_on, p.headcount, p.gross_total, p.deduction_total,
                    p.net_total, p.employer_total, p.voucher_no, v.status AS voucher_status,
                    p.labor_version, p.health_version, p.pension_version, p.withholding_version,
                    p.note, p.created_at, p.created_by
               FROM payroll_run p
               LEFT JOIN voucher v ON v.no = p.voucher_no
              ORDER BY p.period DESC
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_payroll_run)
        .collect())
    }

    pub async fn payroll_run(&self, period: &str) -> Result<Option<PayrollRun>> {
        Ok(self.payroll_runs(500).await?.into_iter().find(|r| r.period == period))
    }

    pub async fn payslips(&self, period: &str) -> Result<Vec<Payslip>> {
        self.payslips_filtered(Some(period), None).await
    }

    /// Payslip history for one employee. The employee id is resolved by the
    /// self-service operation from `ctx.actor` and is not an API input, so a
    /// caller cannot redirect this query at someone else.
    pub async fn payslips_by_employee(&self, employee_id: i32) -> Result<Vec<Payslip>> {
        self.payslips_filtered(None, Some(employee_id)).await
    }

    async fn payslips_filtered(
        &self,
        period: Option<&str>,
        employee_id: Option<i32>,
    ) -> Result<Vec<Payslip>> {
        Ok(sqlx::query(
            "SELECT p.period, p.pay_on,
                    p.labor_version, p.health_version, p.pension_version,
                    p.withholding_version,
                    s.employee_id, e.emp_no, e.name, e.title, d.label AS dept_label,
                    s.base_salary, s.overtime, s.allowance, s.gross,
                    s.labor_insured_salary, s.health_insured_salary, s.pension_wage,
                    s.labor_employee, s.health_employee, s.income_tax,
                    s.labor_employer, s.health_employer, s.pension_employer,
                    s.deduction, s.net, s.paid_days, s.month_days, s.note
               FROM payslip s
               JOIN payroll_run p         ON p.id = s.run_id
               JOIN employee e            ON e.id = s.employee_id
               LEFT JOIN dimension_value d ON d.id = e.dept_id
              WHERE ($1::text IS NULL OR p.period = $1)
                AND ($2::int IS NULL OR s.employee_id = $2)
              ORDER BY p.period DESC, e.emp_no",
        )
        .bind(period)
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_payslip)
        .collect())
    }

    /// Total paid and tax withheld per employee for a year, used by the
    /// annual withholding statements.
    pub async fn payroll_year_totals(&self, year: i32) -> Result<Vec<YearPayTotal>> {
        Ok(sqlx::query(
            "SELECT e.emp_no, e.name, e.id_no,
                    COALESCE(SUM(s.gross), 0)::numeric      AS gross,
                    COALESCE(SUM(s.income_tax), 0)::numeric AS tax,
                    COUNT(*)::bigint                        AS months
               FROM payslip s
               JOIN payroll_run p ON p.id = s.run_id
               JOIN employee e    ON e.id = s.employee_id
              WHERE LEFT(p.period, 4) = $1::text
              GROUP BY e.emp_no, e.name, e.id_no
              ORDER BY e.emp_no",
        )
        .bind(year.to_string())
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| YearPayTotal {
            emp_no: r.get("emp_no"),
            name: r.get("name"),
            id_no: r.get("id_no"),
            gross: r.get("gross"),
            income_tax: r.get("tax"),
            months: r.get("months"),
        })
        .collect())
    }

    /// Salary expense already in the ledger (periods whose voucher is posted)
    /// and the amount still in transit.
    ///
    /// Returns (posted total, unposted total). Together with
    /// `balance_of(["6111"])` these are two independently computed paths, and
    /// agreement is a conclusion rather than an assumption.
    pub async fn payroll_totals(&self) -> Result<(Money, Money)> {
        let r = sqlx::query(
            "SELECT
               COALESCE(SUM(p.gross_total) FILTER (WHERE v.status = 'posted'), 0)::numeric
                 AS posted,
               COALESCE(SUM(p.gross_total) FILTER (WHERE v.status IN ('draft','pending')), 0)::numeric
                 AS pending
               FROM payroll_run p
               LEFT JOIN voucher v ON v.no = p.voucher_no",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((r.get("posted"), r.get("pending")))
    }

    /// Record one payroll period: the run, the per-person payslips and the
    /// unposted voucher, all in one transaction.
    ///
    /// The voucher is created as `pending`: computing payroll answers "what is
    /// owed this month" and posting answers "put it in the ledger". Two
    /// decisions, matching receipts, payments, depreciation and expenses.
    ///
    /// Debits carry one line per department so departmental P&L sees salary;
    /// credits are two lines, withheld items and net pay.
    pub async fn record_payroll_run(&self, req: RecordPayroll<'_>) -> Result<PayrollRun> {
        if req.slips.is_empty() {
            return Err(StoreError::NobodyToPay { period: req.period.to_string() });
        }

        let mut tx = self.pool.begin().await?;

        if let Some(row) = sqlx::query("SELECT voucher_no FROM payroll_run WHERE period = $1")
            .bind(req.period)
            .fetch_optional(&mut *tx)
            .await?
        {
            return Err(StoreError::PeriodAlreadyPaid {
                period: req.period.to_string(),
                voucher: row.get::<Option<String>, _>("voucher_no").unwrap_or_default(),
            });
        }

        let gross: Money = req.slips.iter().map(|s| s.gross).sum();
        let deduction: Money = req.slips.iter().map(|s| s.deduction).sum();
        let net: Money = req.slips.iter().map(|s| s.net).sum();
        let employer: Money = req
            .slips
            .iter()
            .map(|s| s.labor_employer + s.health_employer + s.pension_employer)
            .sum();

        // One salary expense line per department. Employees with no department
        // collect into a single `None` line rather than being apportioned onto
        // others, the same rule as the "unassigned" row in departmental P&L.
        let mut buckets: Vec<(Option<i32>, String, Money, usize)> = Vec::new();
        for s in req.slips {
            match buckets.iter_mut().find(|b| b.0 == s.dept_id) {
                Some(b) => {
                    b.2 += s.gross;
                    b.3 += 1;
                }
                None => buckets.push((
                    s.dept_id,
                    s.dept_label.clone().unwrap_or_else(|| "未指定部門".into()),
                    s.gross,
                    1,
                )),
            }
        }

        let mut lines: Vec<LineInput> = buckets
            .iter()
            .map(|(_, label, amount, n)| LineInput {
                account_code: "6111".into(),
                debit: *amount,
                credit: Money::ZERO,
                memo: Some(format!("{label} {n} 人")),
                bank_account_id: None,
            })
            .collect();

        if deduction > Money::ZERO {
            lines.push(LineInput {
                account_code: "2117".into(),
                debit: Money::ZERO,
                credit: deduction,
                memo: Some(req.deduction_memo.to_string()),
                bank_account_id: None,
            });
        }
        lines.push(LineInput {
            account_code: "1102".into(),
            debit: Money::ZERO,
            credit: net,
            memo: Some(format!("實發 {} 人", req.slips.len())),
            bank_account_id: Some(req.bank_account_id),
        });

        let voucher_no = insert_voucher(
            &mut tx,
            req.pay_on,
            &format!("{} 薪資發放", req.period),
            &lines,
            "pending",
            "payroll",
            req.actor,
            None,
        )
        .await?;

        for (i, (dept_id, _, _, _)) in buckets.iter().enumerate() {
            set_line_dept(&mut tx, &voucher_no, i as i32 + 1, *dept_id).await?;
        }

        let run_id: i32 = sqlx::query(
            "INSERT INTO payroll_run
               (period, pay_on, headcount, gross_total, deduction_total, net_total,
                employer_total, voucher_no, labor_version, health_version,
                pension_version, withholding_version, note, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING id",
        )
        .bind(req.period)
        .bind(req.pay_on)
        .bind(req.slips.len() as i32)
        .bind(gross)
        .bind(deduction)
        .bind(net)
        .bind(employer)
        .bind(&voucher_no)
        .bind(req.labor_version)
        .bind(req.health_version)
        .bind(req.pension_version)
        .bind(req.withholding_version)
        .bind(req.note)
        .bind(req.actor)
        .fetch_one(&mut *tx)
        .await?
        .get("id");

        for s in req.slips {
            sqlx::query(
                "INSERT INTO payslip
                   (run_id, employee_id, base_salary, overtime, allowance, gross,
                    labor_insured_salary, health_insured_salary, pension_wage,
                    labor_employee, health_employee, income_tax,
                    labor_employer, health_employer, pension_employer,
                    deduction, net, paid_days, month_days, note)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
            )
            .bind(run_id)
            .bind(s.employee_id)
            .bind(s.base_salary)
            .bind(s.overtime)
            .bind(s.allowance)
            .bind(s.gross)
            .bind(s.labor_insured_salary)
            .bind(s.health_insured_salary)
            .bind(s.pension_wage)
            .bind(s.labor_employee)
            .bind(s.health_employee)
            .bind(s.income_tax)
            .bind(s.labor_employer)
            .bind(s.health_employer)
            .bind(s.pension_employer)
            .bind(s.deduction)
            .bind(s.net)
            .bind(s.paid_days)
            .bind(s.month_days)
            .bind(&s.note)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        self.payroll_run(req.period)
            .await?
            .ok_or_else(|| StoreError::NobodyToPay { period: req.period.to_string() })
    }
}

// Return types

#[derive(Debug, Clone, serde::Serialize)]
pub struct Account {
    pub code: String,
    pub name: String,
    pub kind: String,
    pub normal_side: String,
    pub is_postable: bool,
    pub is_active: bool,
    pub parent_code: Option<String>,
    pub sort_order: i32,
    pub report_group: String,
    pub cash_flow_section: Option<String>,
}

/// Fields needed to create or fully update an account.
pub struct WriteAccount<'a> {
    pub code: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub normal_side: &'a str,
    pub is_postable: bool,
    pub is_active: bool,
    pub parent_code: Option<&'a str>,
    pub sort_order: i32,
    pub report_group: &'a str,
    pub cash_flow_section: Option<&'a str>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BankAccount {
    pub id: i32,
    pub name: String,
    pub account_no: String,
    pub gl_code: String,
    pub sort_order: i32,
}

pub struct CreateBankAccount<'a> {
    pub name: &'a str,
    pub account_no: &'a str,
    pub gl_code: &'a str,
    pub sort_order: i32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CashAccount {
    pub name: String,
    pub account_no: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub balance: Money,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VoucherLine {
    pub account_code: String,
    pub account_name: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub debit: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub credit: Money,
    pub memo: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Voucher {
    pub no: String,
    pub date: NaiveDate,
    pub memo: String,
    pub status: String,
    pub source: String,
    pub created_by: String,
    /// Which original voucher this one reverses.
    pub reverses: Option<String>,
    /// Which reversing voucher has reversed this original.
    pub reversed_by: Option<String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    pub accounts_label: String,
    pub lines: Vec<VoucherLine>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: String,
    pub actor_role: String,
    pub tool_name: String,
    pub risk: String,
    pub prompt: Option<String>,
    /// `ui` or `mcp`: whether a person or an AI did this.
    pub channel: String,
    pub result_summary: String,
    pub outcome: String,
    pub voucher_no: Option<String>,
    pub reverted: bool,
}

/// One row of the receivables subsidiary ledger.
///
/// `settled` counts posted receipts only, while `in_transit` is the part
/// whose receipt voucher exists but is unposted. They are separate because
/// only the former reaches the ledger: `outstanding` must always equal the
/// 1131 balance and cannot be reduced early by an unposted voucher.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArInvoice {
    pub invoice_no: String,
    pub customer: String,
    pub issued_on: NaiveDate,
    pub due_on: NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub settled: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub in_transit: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub outstanding: Money,
    /// `open`, `receiving` (receipt voucher unposted), or `settled`.
    pub state: String,
    /// Days overdue. Negative means not yet due.
    pub days_overdue: i32,
    pub memo: String,
    pub origin_voucher_no: Option<String>,
    /// Whether the source voucher is posted. False means it has not reached
    /// the ledger and is excluded from the outstanding total.
    pub recognised: bool,
    pub receipt_voucher_no: Option<String>,
}

pub struct CreateArInvoice<'a> {
    pub invoice_no: &'a str,
    pub customer: &'a str,
    pub issued_on: NaiveDate,
    pub due_on: NaiveDate,
    pub amount: Money,
    pub memo: &'a str,
    pub receivable_account_code: &'a str,
    pub revenue_account_code: &'a str,
    pub created_by: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CreatedArInvoice {
    pub invoice_no: String,
    pub customer: String,
    pub issued_on: NaiveDate,
    pub due_on: NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    pub memo: String,
    pub voucher_no: String,
}

/// One row of the payables subsidiary ledger, mirroring [`ArInvoice`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ApBill {
    pub bill_no: String,
    pub vendor: String,
    pub memo: String,
    pub billed_on: NaiveDate,
    pub due_on: NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub settled: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub in_transit: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub outstanding: Money,
    /// `open`, `paying` (payment voucher unposted), or `settled`.
    pub state: String,
    pub days_overdue: i32,
    pub origin_voucher_no: Option<String>,
    pub recognised: bool,
    pub batch_no: Option<String>,
}

pub struct CreateApBill<'a> {
    pub bill_no: &'a str,
    pub vendor: &'a str,
    pub memo: &'a str,
    pub billed_on: NaiveDate,
    pub due_on: NaiveDate,
    pub amount: Money,
    pub debit_account_code: &'a str,
    pub payable_account_code: &'a str,
    pub created_by: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CreatedApBill {
    pub bill_no: String,
    pub vendor: String,
    pub memo: String,
    pub billed_on: NaiveDate,
    pub due_on: NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    pub voucher_no: String,
}

/// Result of recording one receipt.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Receipt {
    pub voucher_no: String,
    pub invoice_no: String,
    pub customer: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    /// How much of this invoice remains outstanding.
    #[serde(with = "rust_decimal::serde::str")]
    pub still_open: Money,
}

/// One payment batch.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PaymentBatch {
    pub batch_no: String,
    pub voucher_no: String,
    pub pay_on: NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Money,
    pub bill_count: i64,
    pub vendors: Vec<String>,
}

/// One bank transaction. `amount` is positive for money in, negative out.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BankTxn {
    pub id: i32,
    pub bank_account_id: i32,
    pub bank_name: String,
    pub account_no: String,
    pub txn_date: NaiveDate,
    pub memo: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    pub kind: String,
    pub matched_voucher_no: Option<String>,
    pub matched_at: Option<DateTime<Utc>>,
    pub matched_by: Option<String>,
}

impl BankTxn {
    pub fn is_matched(&self) -> bool {
        self.matched_voucher_no.is_some()
    }
}

/// A match candidate for one transaction. `amount_gap` and `day_gap` drive
/// the ranking and let a person judge for themselves.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MatchCandidate {
    pub voucher_no: String,
    pub date: NaiveDate,
    pub memo: String,
    pub status: String,
    /// The voucher's net effect on that bank account (debit minus credit); it
    /// must share the transaction's sign to match.
    #[serde(with = "rust_decimal::serde::str")]
    pub net_effect: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount_gap: Money,
    pub day_gap: i32,
}

/// The issuing record of one e-invoice.
///
/// `net_amount` and `tax_amount` were computed at issue time and are never
/// recomputed on read: once an invoice is issued, the tax printed on it is
/// a fact. `vat_version` records which ruleset produced it, so a
/// retroactive amendment can identify the invoices to reissue.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EInvoice {
    pub invoice_no: String,
    pub track: String,
    pub serial_no: i32,
    pub buyer_tax_id: String,
    pub buyer: String,
    pub item: String,
    pub issued_on: NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub net_amount: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub tax_amount: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_amount: Money,
    pub vat_version: String,
    /// `queued`, `uploaded`, `failed` or `void`.
    pub upload_status: String,
    pub upload_attempts: i32,
    pub uploaded_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub voucher_no: Option<String>,
    pub voucher_status: Option<String>,
    pub ar_invoice_no: Option<String>,
    /// The issuing voucher is posted, so this invoice's output tax is in the
    /// ledger.
    pub recognised: bool,
}

/// Track status. `remaining` is how many invoices can still be issued.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackState {
    pub track: String,
    pub period: String,
    pub range_from: i32,
    pub range_to: i32,
    pub next_no: i32,
    pub remaining: i32,
}

/// Everything needed to issue one e-invoice. The tax is computed by the
/// caller from the ruleset.
pub struct IssueEInvoice<'a> {
    pub track: &'a str,
    pub buyer_tax_id: &'a str,
    pub buyer: &'a str,
    pub item: &'a str,
    pub issued_on: NaiveDate,
    pub due_on: NaiveDate,
    pub net_amount: Money,
    pub tax_amount: Money,
    /// VAT ruleset version used for this tax amount.
    pub vat_version: &'a str,
    pub created_by: &'a str,
}

/// Result of issuing one invoice.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IssuedEInvoice {
    pub invoice_no: String,
    pub voucher_no: String,
    pub buyer: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub net_amount: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub tax_amount: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_amount: Money,
    pub vat_version: String,
    /// Numbers remaining in this track.
    pub remaining: i32,
}

/// Ledger figures for the VAT return. Posted entries only.
#[derive(Debug, Clone)]
pub struct VatFigures {
    /// Net credit movement of 4111 for the period.
    pub sales: Money,
    /// Net credit movement of 2211: the output tax on the return.
    pub output_tax: Money,
    /// Net debit movement of 1284 for the period.
    pub input_tax: Money,
    pub pending_sales: Money,
    pub pending_output_tax: Money,
    pub pending_input_tax: Money,
    /// Vouchers touching these three accounts that are not yet posted.
    pub pending_vouchers: i64,
    /// Credit carried forward: input tax in excess of output tax before the
    /// period start.
    pub prior_credit: Money,
}

/// One payment of withholdable income. The tax due is not stored here; that
/// requires asking the ruleset as of the payment date.
#[derive(Debug, Clone)]
pub struct WithholdingRecord {
    pub paid_on: NaiveDate,
    pub payee: String,
    pub payee_id: String,
    pub income_code: String,
    pub gross_amount: Money,
    /// `rate`: computed from the withholding standard. `table`: from the
    /// salary withholding table, with the tax supplied by the source system.
    pub basis: String,
    pub withheld_amount: Option<Money>,
    /// Whether this payment falls in a chargeable category for the
    /// supplementary health insurance premium.
    pub supplementary_base: bool,
    pub memo: String,
    pub voucher_no: Option<String>,
}

/// One fixed asset. The useful life is not here: ask the depreciation
/// ruleset with `category` and `acquired_on`, which is its only source.
#[derive(Debug, Clone)]
pub struct FixedAsset {
    pub id: i32,
    pub code: String,
    pub name: String,
    pub category: String,
    pub acquired_on: NaiveDate,
    pub cost: Money,
    pub method: String,
    pub gl_code: String,
    pub accum_gl_code: String,
    pub origin_voucher_no: Option<String>,
    pub status: String,
    pub memo: String,
    /// The acquisition voucher is posted. False means the asset has not
    /// reached the ledger and is excluded from the cost total.
    pub recognised: bool,
    /// Posted depreciation to date. Summed across all assets this must equal
    /// the ledger balance of 1602.
    pub accumulated: Money,
    /// Depreciation vouchered but not yet posted.
    pub in_transit: Money,
}

/// One depreciation run. `kind = 'opening'` is the entry that restates net
/// book value into cost and accumulated depreciation at opening.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DepreciationRun {
    pub period: String,
    pub kind: String,
    pub run_on: NaiveDate,
    pub voucher_no: String,
    pub voucher_status: String,
    /// An opening restatement has no ruleset version; it was not derived from
    /// the rules.
    pub ruleset_version: Option<String>,
    pub created_by: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Money,
}

/// Debit and credit movement of one account over a period. All three
/// financial statements are built from this type.
///
/// It deliberately stores movement rather than a balance: a balance is the
/// movement from inception to a date, and a period figure is the movement
/// between two dates. One type, one query, differing only in whether `from`
/// is given. One concept fewer is one way fewer for figures to disagree.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountMovement {
    pub code: String,
    pub name: String,
    pub kind: String,
    pub normal_side: String,
    pub is_postable: bool,
    /// False only blocks new entries; historical entries still appear on the
    /// statements.
    pub is_active: bool,
    /// Which section of the balance sheet or income statement it belongs to.
    pub report_group: String,
    /// Cash flow activity. `None` for P&L accounts, represented by profit.
    pub cash_flow_section: Option<String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub debit: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub credit: Money,
    /// How many journal lines touched it this period. Zero means unused.
    pub line_count: i64,
}

impl AccountMovement {
    /// Net debit. The normal direction for assets and expenses.
    pub fn net_debit(&self) -> Money {
        self.debit - self.credit
    }

    /// Net credit. The normal direction for liabilities, equity and revenue,
    /// and the positive direction on the cash flow statement.
    pub fn net_credit(&self) -> Money {
        self.credit - self.debit
    }

    /// Signed by the account's normal balance side. 1602 accumulated
    /// depreciation is an asset with a credit balance, so this returns a
    /// positive number; presentation decides about parentheses.
    pub fn balance(&self) -> Money {
        if self.normal_side == "debit" {
            self.net_debit()
        } else {
            self.net_credit()
        }
    }

    pub fn is_cash(&self) -> bool {
        self.cash_flow_section.as_deref() == Some("cash")
    }
}

/// One company setting. Regulation values never appear here (see
/// migrations/0006).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompanySetting {
    pub code: String,
    pub label: String,
    pub value: String,
    pub note: String,
    pub editable: bool,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DimensionValue {
    pub code: String,
    pub label: String,
    pub active: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Dimension {
    pub code: String,
    pub label: String,
    pub note: String,
    pub values: Vec<DimensionValue>,
}

pub struct CreateDimensionValue<'a> {
    pub dimension_code: &'a str,
    pub code: &'a str,
    pub label: &'a str,
    pub active: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DimensionValueRecord {
    pub id: i32,
    pub dimension_code: String,
    pub code: String,
    pub label: String,
    pub active: bool,
    pub sort_order: i32,
}

/// A login account. It carries a password hash, so it deliberately does not
/// implement `Serialize`.
#[derive(Debug, Clone)]
pub struct UserAccount {
    pub id: i32,
    pub username: String,
    pub display_name: String,
    pub email: String,
    /// `None` means the account can only sign in via a linked external
    /// identity.
    pub password_hash: Option<String>,
    pub role_code: String,
    pub employee_id: Option<i32>,
    /// `active` or `suspended`.
    pub status: String,
    pub must_change_password: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub last_login_at: Option<DateTime<Utc>>,
}

/// Data needed to create an account. The password must already be hashed by
/// the caller with a proper password hashing algorithm.
pub struct CreateUser<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub email: &'a str,
    pub password_hash: Option<&'a str>,
    pub role_code: &'a str,
    pub employee_id: Option<i32>,
    /// `active` or `suspended`; the database CHECK is still the final gate.
    pub status: &'a str,
    pub must_change_password: bool,
    pub created_by: &'a str,
}

/// A link between an external IdP subject and a local account.
#[derive(Debug, Clone)]
pub struct UserIdentity {
    pub id: i32,
    pub user_id: i32,
    pub provider: String,
    pub subject: String,
    /// Kept only as an audit trace of the link; never used as a login key.
    pub email_at_link: String,
    pub linked_at: DateTime<Utc>,
}

/// A session that can still be looked up. The token hash is not exposed in
/// the return type either.
#[derive(Debug, Clone)]
pub struct UserSession {
    pub id: i32,
    pub user: UserAccount,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: String,
    pub user_agent: String,
    pub ip: String,
}

/// Data needed to create a session; `token_hash` must be computed before
/// calling the store.
pub struct CreateSession<'a> {
    pub token_hash: &'a str,
    pub user_id: i32,
    pub expires_at: DateTime<Utc>,
    pub user_agent: &'a str,
    pub ip: &'a str,
}

/// `code` shares its values with `ai_audit_log.actor_role` and
/// `OpCtx.actor_role`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Role {
    pub code: String,
    pub name: String,
    pub note: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Permission {
    pub code: String,
    pub label: String,
    pub note: String,
    /// Which operations this permission governs. dispatch reads this column
    /// on every call.
    pub op_names: Vec<String>,
}

/// One cell of the matrix. No row means the grant does not exist, which is
/// not the same as a grant of zero.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RolePermission {
    pub role_code: String,
    pub permission_code: String,
    /// `full` or `limited`.
    pub level: String,
    pub note: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

/// One step of the expense approval flow. `min_amount` is company policy,
/// not regulation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ApprovalRule {
    pub step_no: i32,
    pub role_code: Option<String>,
    pub role_name: Option<String>,
    pub label: String,
    pub condition_note: String,
    #[serde(with = "rust_decimal::serde::str_option")]
    pub min_amount: Option<Money>,
}

pub struct SetApprovalRule<'a> {
    pub step_no: i32,
    pub role_code: Option<&'a str>,
    pub label: &'a str,
    pub condition_note: &'a str,
    pub min_amount: Option<Money>,
}

/// One control rule. `enforced` is the reason this type exists: being able
/// to state a rule is not the same as applying it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SecurityPolicy {
    pub code: String,
    pub label: String,
    pub value: String,
    pub detail: String,
    pub enforced: bool,
    pub sort_order: i32,
}

/// An API or MCP token. There is no plaintext field, and no method to
/// recover one.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiToken {
    pub id: i32,
    pub name: String,
    /// `api` or `mcp`.
    pub kind: String,
    pub scope: String,
    /// First eight characters, only so a person can recognise which token a
    /// row is.
    pub prefix: String,
    pub actor: String,
    pub actor_role: String,
    pub actor_role_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub last_used_at: Option<DateTime<Utc>>,
    pub rotate_days: Option<i32>,
    pub expiry_note: String,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: String,
    /// Employee this token belongs to when it was issued through
    /// self-service. `None` for tokens the owner issued on the
    /// installation's behalf.
    pub owner_employee_id: Option<i32>,
}

impl ApiToken {
    /// Expiry when `rotate_days` is set: `created_at + rotate_days`.
    ///
    /// This is enforced expiry, not a rotation reminder. It differs from
    /// `rotate_due_on` on the listing screen (estimated from last use): one
    /// decides whether the token still works, the other only suggests
    /// changing it. A token with no rotation period never expires, and
    /// revocation stays the only way to stop it.
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.rotate_days
            .map(|days| self.created_at + chrono::Duration::days(i64::from(days)))
    }

    /// Whether it has expired as of `at`.
    pub fn is_expired(&self, at: DateTime<Utc>) -> bool {
        self.expires_at().is_some_and(|expires_at| at >= expires_at)
    }
}

/// Data needed to issue a token.
pub struct IssueApiToken<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub scope: &'a str,
    /// Who this token acts as. AI connecting with it is never more privileged
    /// than that person.
    pub actor: &'a str,
    pub actor_role: &'a str,
    pub created_by: &'a str,
    pub rotate_days: Option<i32>,
    pub expiry_note: &'a str,
    /// Set only by self-service issuing. It is the key ownership is checked
    /// against, because `actor` is a display name and two people can share
    /// one.
    pub owner_employee_id: Option<i32>,
}

/// A freshly issued token. `secret` is the plaintext and is visible only
/// this once.
pub struct IssuedApiToken {
    pub token: ApiToken,
    pub secret: String,
}

/// Current grant status of one MCP tool.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolGrant {
    pub tool_name: String,
    pub enabled: bool,
    pub risk: String,
    pub note: String,
    pub updated_at: DateTime<Utc>,
}

/// System status as far as the database can see it. The brand fingerprint
/// and RustFS are answered by `/healthz`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SystemStats {
    pub posted_vouchers: i64,
    pub unposted_vouchers: i64,
    pub accounts: i64,
    pub audit_entries: i64,
    pub audit_blocked: i64,
    pub audit_since: Option<DateTime<Utc>>,
    pub tokens_live: i64,
    pub tokens_revoked: i64,
    pub rulesets: i64,
    pub tools_enabled: i64,
    pub tools_total: i64,
}

// Departments and budgets

/// One department. `id` is the `dimension_value.id` that
/// `voucher_line.dept_id` points at.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Department {
    pub id: i32,
    pub code: String,
    pub label: String,
    pub active: bool,
}

/// Period movement for one department and account.
///
/// A `dept_id` of `None` means those entries have no department. They are
/// still returned, because the money was still spent; dropping them would
/// stop departmental P&L summing to the income statement.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeptMovement {
    pub dept_id: Option<i32>,
    pub dept_code: Option<String>,
    pub dept_label: Option<String>,
    pub code: String,
    pub name: String,
    pub kind: String,
    pub report_group: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub debit: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub credit: Money,
    pub line_count: i64,
}

impl DeptMovement {
    pub fn net_debit(&self) -> Money {
        self.debit - self.credit
    }
    pub fn net_credit(&self) -> Money {
        self.credit - self.debit
    }
}

/// One budget version. A partial unique index guarantees only one
/// `is_current` per year.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BudgetVersion {
    pub code: String,
    pub label: String,
    pub fiscal_year: i32,
    pub is_current: bool,
    pub note: String,
    pub created_by: String,
    pub row_count: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Money,
}

/// One budget row. Budgeted amounts only; actuals come from the ledger.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BudgetRow {
    pub account_code: String,
    pub account_name: String,
    pub dept_id: Option<i32>,
    pub dept_code: Option<String>,
    pub dept_label: Option<String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    pub note: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

// Expense claims

/// One approval step that has actually happened.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExpenseStep {
    pub step_no: i32,
    /// `approved` or `rejected`.
    pub decision: String,
    pub actor: String,
    pub actor_role: String,
    pub note: String,
    pub decided_at: DateTime<Utc>,
}

/// One expense claim.
///
/// `suggested_account_code` is what AI proposed from the receipt and
/// `account_code` is what a person decided. Both are kept so an audit can
/// see the difference.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExpenseClaim {
    pub id: i32,
    pub claim_no: String,
    pub title: String,
    pub claimant: String,
    pub claimant_role: String,
    pub dept_code: Option<String>,
    pub dept_label: Option<String>,
    pub spent_on: NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Money,
    pub suggested_account_code: Option<String>,
    pub suggested_account_name: Option<String>,
    pub account_code: Option<String>,
    pub account_name: Option<String>,
    /// `reimburse` (paid with the month-end run, credit 2101) or
    /// `petty_cash` (paid immediately, credit 1101).
    pub pay_method: String,
    /// `submitted`, `approving`, `approved` or `rejected`.
    pub status: String,
    pub voucher_no: Option<String>,
    pub voucher_status: Option<String>,
    pub document_no: Option<String>,
    pub document_name: Option<String>,
    pub memo: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub steps: Vec<ExpenseStep>,
}

impl ExpenseClaim {
    /// Whether this step has been approved.
    pub fn step_done(&self, step_no: i32) -> bool {
        self.steps.iter().any(|s| s.step_no == step_no && s.decision == "approved")
    }
}

/// Data needed to submit a claim.
pub struct SubmitExpenseClaim<'a> {
    pub title: &'a str,
    pub claimant: &'a str,
    pub claimant_role: &'a str,
    /// Set only by self-service, where the claimant is the caller. `None`
    /// means accounting filed it for somebody, leaving a name and no key.
    pub employee_id: Option<i32>,
    pub dept_code: Option<&'a str>,
    pub spent_on: NaiveDate,
    pub amount: Money,
    /// The account AI suggested. A person decides the final one at step 2.
    pub suggested_account_code: Option<&'a str>,
    pub pay_method: &'a str,
    pub document_id: Option<i32>,
    pub memo: &'a str,
}

/// Data needed to approve one step.
pub struct ApproveExpenseStep<'a> {
    pub claim_no: &'a str,
    pub step_no: i32,
    pub actor: &'a str,
    pub actor_role: &'a str,
    pub note: &'a str,
    /// Carried by the accounting step that confirms the account.
    pub account_code: Option<&'a str>,
    /// Whether this is the final step. If so, the expense voucher is created
    /// in the same transaction.
    pub finalise: bool,
    /// Voucher date. Defaults to the spend date.
    pub posted_on: Option<NaiveDate>,
}

// Documents

/// One revision of a document. The content is not here; `object_key` points
/// into RustFS.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentRow {
    pub doc_no: String,
    pub name: String,
    pub kind: String,
    pub link_type: String,
    pub link_ref: String,
    pub archived: bool,
    pub note: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub version_no: i32,
    pub versions: i64,
    pub object_key: String,
    pub file_name: String,
    pub byte_size: i64,
    pub total_bytes: i64,
    pub content_type: String,
    /// BLAKE3 computed at upload time, recomputed and compared on retrieval.
    pub checksum: String,
    /// `pending` (not recognised; the system has no OCR engine) or
    /// `not_applicable`.
    pub ocr_status: String,
    pub uploaded_at: DateTime<Utc>,
    pub uploaded_by: String,
}

/// Allocate a document number and revision, with no content yet.
pub struct ReserveDocument<'a> {
    /// Set to add a revision to an existing document; `None` starts a new one.
    pub doc_no: Option<&'a str>,
    pub name: &'a str,
    pub kind: &'a str,
    pub link_type: &'a str,
    pub link_ref: &'a str,
    pub note: &'a str,
    pub today: NaiveDate,
    pub actor: &'a str,
}

/// The allocated position, from which the object key is built.
#[derive(Debug, Clone)]
pub struct DocumentSlot {
    pub document_id: i32,
    pub doc_no: String,
    pub version_no: i32,
}

/// Document statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentStats {
    pub documents: i64,
    pub versions: i64,
    /// Bytes registered on the database side. The bucket side is measured
    /// separately by `ObjectStore::usage`.
    pub bytes: i64,
    pub ocr_pending: i64,
    pub archived: i64,
    /// Saved or posted vouchers with no document attached.
    pub vouchers_without_doc: i64,
}

// Imports

/// One import source. `supported = false` means the parser is not written.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportSource {
    pub code: String,
    pub label: String,
    pub format: String,
    pub cadence: String,
    pub supported: bool,
    pub note: String,
    pub last_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_batch_no: Option<String>,
}

/// One external integration. `live` is the reason this table exists: being
/// able to describe an integration is not the same as having one.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Integration {
    pub code: String,
    pub label: String,
    pub detail: String,
    pub live: bool,
    pub note: String,
}

/// One problem found by a dry run. `error` blocks the batch, `warning` only
/// reports.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportIssue {
    pub row_no: i32,
    pub severity: String,
    pub issue: String,
    pub fix: String,
}

/// One import batch.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportBatch {
    pub batch_no: String,
    pub source_code: String,
    pub source_label: String,
    pub file_name: String,
    pub row_count: i32,
    pub ok_count: i32,
    pub error_count: i32,
    pub warn_count: i32,
    /// `preview`, `committed`, `rolled_back` or `discarded`.
    pub status: String,
    pub voucher_nos: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub settled_at: Option<DateTime<Utc>>,
    pub settled_by: String,
    pub issues: Vec<ImportIssue>,
}

/// Data needed to store one dry run result.
pub struct CreateImportBatch<'a> {
    pub source_code: &'a str,
    pub file_name: &'a str,
    pub row_count: i32,
    pub ok_count: i32,
    pub error_count: i32,
    pub warn_count: i32,
    /// The parsed voucher drafts. Committing writes exactly these.
    pub payload: serde_json::Value,
    pub issues: &'a [ImportIssue],
    pub today: NaiveDate,
    pub actor: &'a str,
}

/// One voucher draft produced by an import, stored in
/// `import_batch.payload`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportVoucherDraft {
    pub date: NaiveDate,
    pub memo: String,
    pub lines: Vec<ImportLineDraft>,
}

/// One journal line of a draft.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportLineDraft {
    pub account_code: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub debit: Money,
    #[serde(with = "rust_decimal::serde::str")]
    pub credit: Money,
    #[serde(default)]
    pub memo: Option<String>,
    #[serde(default)]
    pub dept_id: Option<i32>,
}

/// One line of a depreciation voucher: which asset, how much, and the
/// useful life used.
pub struct DepreciationLine {
    pub asset_id: i32,
    pub asset_name: String,
    pub amount: Money,
    pub useful_life_years: i32,
}

// HR types
//
// `Employee` carries the salary and the three insured brackets because the
// repository only moves data. Whether to hand them out is decided in
// laysi-ops: `list_employees` returns no salary and `list_payslips` is
// `Risk::BossOnly`. Filtering belongs to the operation layer so that "who
// sees what" has exactly one home: the risk check in dispatch.

#[derive(Debug, Clone, serde::Serialize)]
pub struct Employee {
    pub id: i32,
    pub emp_no: String,
    pub name: String,
    pub dept_code: Option<String>,
    pub dept_label: Option<String>,
    pub dept_id: Option<i32>,
    pub title: String,
    pub role_code: Option<String>,
    pub role_name: Option<String>,
    pub employment_type: String,
    pub hired_on: NaiveDate,
    pub left_on: Option<NaiveDate>,
    pub status: String,
    pub base_salary: Money,
    /// Insured salary filed with the labour insurance bureau. A fact, not a
    /// rule; the ceiling comes from the ruleset.
    pub labor_insured_salary: Money,
    pub health_insured_salary: Money,
    pub pension_wage: Money,
    /// Masked. The full national ID does not exist in this system.
    pub id_no: String,
    pub bank_account: String,
    pub note: String,
}

/// Create or fully update an employee record. The three insured brackets
/// are filed facts and are never corrected against a ruleset by the store.
pub struct WriteEmployee<'a> {
    pub emp_no: &'a str,
    pub name: &'a str,
    pub dept_code: Option<&'a str>,
    pub title: &'a str,
    pub role_code: &'a str,
    pub employment_type: &'a str,
    pub hired_on: NaiveDate,
    pub left_on: Option<NaiveDate>,
    pub status: &'a str,
    pub base_salary: Money,
    pub labor_insured_salary: Money,
    pub health_insured_salary: Money,
    pub pension_wage: Money,
    pub id_no: &'a str,
    pub bank_account: &'a str,
    pub note: &'a str,
    pub actor: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HrFlowStep {
    pub kind: String,
    pub step_no: i32,
    pub label: String,
    pub note: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MovementStep {
    pub step_no: i32,
    pub done_at: DateTime<Utc>,
    pub done_by: String,
    pub note: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EmployeeMovement {
    pub id: i32,
    pub movement_no: String,
    pub emp_no: String,
    pub name: String,
    pub dept_label: Option<String>,
    pub kind: String,
    pub effective_on: NaiveDate,
    pub from_value: String,
    pub to_value: String,
    pub reason: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub done_steps: Vec<MovementStep>,
}

impl EmployeeMovement {
    pub fn step_done(&self, step_no: i32) -> bool {
        self.done_steps.iter().any(|s| s.step_no == step_no)
    }
}

pub struct RecordMovement<'a> {
    pub emp_no: &'a str,
    pub kind: &'a str,
    pub effective_on: NaiveDate,
    pub from_value: &'a str,
    pub to_value: &'a str,
    pub reason: &'a str,
    pub actor: &'a str,
}

/// One clock-in or clock-out.
///
/// `punched_at` is the instant; `punched_on` is the working day it belongs to
/// in the business time zone. A CHECK keeps the two consistent, so nothing
/// downstream has to guess which of them to trust.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AttendancePunch {
    pub id: i32,
    pub employee_id: i32,
    pub emp_no: String,
    pub name: String,
    pub dept_label: Option<String>,
    pub punched_on: NaiveDate,
    pub punched_at: DateTime<Utc>,
    pub kind: String,
    /// `self` when the employee pressed the button, `manual` when HR entered
    /// it afterwards.
    pub source: String,
    pub note: String,
    pub created_by: String,
}

pub struct NewPunch<'a> {
    pub employee_id: i32,
    pub punched_on: NaiveDate,
    pub punched_at: DateTime<Utc>,
    pub kind: &'a str,
    pub source: &'a str,
    pub note: &'a str,
    pub actor: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttendanceRow {
    pub employee_id: i32,
    pub emp_no: String,
    pub name: String,
    pub dept_label: Option<String>,
    pub title: String,
    pub month: String,
    pub required_days: Money,
    pub worked_days: Money,
    pub leave_days: Money,
    pub overtime_hours: Money,
    /// Overtime pay. The multipliers are regulation and are not yet in the
    /// rule library, so this comes from the timekeeping system rather than
    /// being computed here.
    pub overtime_pay: Money,
    pub late_count: i32,
    pub note: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LeaveRequest {
    pub leave_no: String,
    pub employee_id: i32,
    pub emp_no: String,
    pub name: String,
    pub dept_label: Option<String>,
    pub kind: String,
    pub from_on: NaiveDate,
    pub to_on: NaiveDate,
    pub days: Money,
    pub reason: String,
    pub status: String,
    pub decided_by: String,
    pub decided_role: String,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_note: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

/// A leave request to be written. Both `employee_id` and `actor` are
/// resolved from `ctx.actor` by the self-service operation and never appear
/// as API input.
pub struct NewLeaveRequest<'a> {
    pub employee_id: i32,
    pub kind: &'a str,
    pub from_on: NaiveDate,
    pub to_on: NaiveDate,
    pub days: Money,
    pub reason: &'a str,
    pub actor: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PayrollRun {
    pub period: String,
    pub pay_on: NaiveDate,
    pub headcount: i32,
    pub gross_total: Money,
    pub deduction_total: Money,
    pub net_total: Money,
    pub employer_total: Money,
    pub voucher_no: Option<String>,
    pub voucher_status: Option<String>,
    pub labor_version: String,
    pub health_version: String,
    pub pension_version: String,
    pub withholding_version: String,
    pub note: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Payslip {
    pub period: String,
    pub pay_on: NaiveDate,
    pub labor_version: String,
    pub health_version: String,
    pub pension_version: String,
    pub withholding_version: String,
    pub employee_id: i32,
    pub emp_no: String,
    pub name: String,
    pub title: String,
    pub dept_label: Option<String>,
    pub base_salary: Money,
    pub overtime: Money,
    pub allowance: Money,
    pub gross: Money,
    pub labor_insured_salary: Money,
    pub health_insured_salary: Money,
    pub pension_wage: Money,
    pub labor_employee: Money,
    pub health_employee: Money,
    pub income_tax: Money,
    pub labor_employer: Money,
    pub health_employer: Money,
    pub pension_employer: Money,
    pub deduction: Money,
    pub net: Money,
    pub paid_days: Money,
    pub month_days: Money,
    pub note: String,
}

/// A payslip to be written. Every amount is computed by `laysi-ops` from
/// the ruleset and passed in.
pub struct NewPayslip {
    pub employee_id: i32,
    pub dept_id: Option<i32>,
    pub dept_label: Option<String>,
    pub base_salary: Money,
    pub overtime: Money,
    pub allowance: Money,
    pub gross: Money,
    pub labor_insured_salary: Money,
    pub health_insured_salary: Money,
    pub pension_wage: Money,
    pub labor_employee: Money,
    pub health_employee: Money,
    pub income_tax: Money,
    pub labor_employer: Money,
    pub health_employer: Money,
    pub pension_employer: Money,
    pub deduction: Money,
    pub net: Money,
    pub paid_days: Money,
    pub month_days: Money,
    pub note: String,
}

pub struct RecordPayroll<'a> {
    pub period: &'a str,
    pub pay_on: NaiveDate,
    pub slips: &'a [NewPayslip],
    pub bank_account_id: i32,
    pub deduction_memo: &'a str,
    pub labor_version: &'a str,
    pub health_version: &'a str,
    pub pension_version: &'a str,
    pub withholding_version: &'a str,
    pub note: &'a str,
    pub actor: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct YearPayTotal {
    pub emp_no: String,
    pub name: String,
    pub id_no: String,
    pub gross: Money,
    pub income_tax: Money,
    pub months: i64,
}

// Internal helpers

type Tx<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

fn ensure_voidable(no: &str, status: &str) -> Result<()> {
    if status == "posted" {
        Ok(())
    } else {
        Err(StoreError::NotVoidable { no: no.to_string(), status: status.to_string() })
    }
}

fn ensure_reversible(no: &str, status: &str) -> Result<()> {
    if status == "posted" {
        Ok(())
    } else {
        Err(StoreError::NotReversible { no: no.to_string(), status: status.to_string() })
    }
}

async fn prepare_account_parent(
    tx: &mut Tx<'_>,
    code: &str,
    parent_code: Option<&str>,
    is_postable: bool,
) -> Result<()> {
    if is_postable {
        let has_children: bool =
            sqlx::query("SELECT EXISTS (SELECT 1 FROM account WHERE parent_code = $1) AS found")
                .bind(code)
                .fetch_one(&mut **tx)
                .await?
                .get("found");
        if has_children {
            return Err(StoreError::AccountHasChildren { code: code.to_string() });
        }
    }

    let Some(parent_code) = parent_code else {
        return Ok(());
    };
    if parent_code == code {
        return Err(StoreError::AccountParentCycle { code: code.to_string() });
    }

    let row = sqlx::query(
        "WITH RECURSIVE ancestors(code, parent_code) AS (
             SELECT code, parent_code FROM account WHERE code = $1
             UNION
             SELECT a.code, a.parent_code
               FROM account a JOIN ancestors p ON a.code = p.parent_code
         )
         SELECT COUNT(*)::BIGINT AS total,
                COALESCE(BOOL_OR(code = $2), false) AS cycle
           FROM ancestors",
    )
    .bind(parent_code)
    .bind(code)
    .fetch_one(&mut **tx)
    .await?;
    let total: i64 = row.get("total");
    if total == 0 {
        return Err(StoreError::ParentAccountNotFound { code: parent_code.to_string() });
    }
    if row.get::<bool, _>("cycle") {
        return Err(StoreError::AccountParentCycle { code: code.to_string() });
    }

    sqlx::query("UPDATE account SET is_postable = false WHERE code = $1")
        .bind(parent_code)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn ensure_approval_rule_can_delete(total: i64) -> Result<()> {
    if total <= 1 {
        Err(StoreError::LastApprovalRule)
    } else {
        Ok(())
    }
}

async fn resolve_department_id(
    tx: &mut Tx<'_>,
    dept_code: Option<&str>,
) -> Result<Option<i32>> {
    let Some(dept_code) = dept_code else {
        return Ok(None);
    };
    sqlx::query(
        "SELECT id FROM dimension_value
          WHERE dimension_code = 'department' AND code = $1",
    )
    .bind(dept_code)
    .fetch_optional(&mut **tx)
    .await?
    .map(|row| row.get("id"))
    .map(Some)
    .ok_or_else(|| StoreError::DepartmentNotFound { code: dept_code.to_string() })
}

async fn ensure_role_exists(tx: &mut Tx<'_>, role_code: &str) -> Result<()> {
    if sqlx::query("SELECT 1 FROM role WHERE code = $1")
        .bind(role_code)
        .fetch_optional(&mut **tx)
        .await?
        .is_none()
    {
        return Err(StoreError::RoleNotFound { code: role_code.to_string() });
    }
    Ok(())
}

struct InsertArInvoice<'a> {
    invoice_no: &'a str,
    customer: &'a str,
    issued_on: NaiveDate,
    due_on: NaiveDate,
    amount: Money,
    voucher_memo: &'a str,
    invoice_memo: &'a str,
    lines: &'a [LineInput],
    created_by: &'a str,
}

/// Create the receivable and its source voucher inside an existing
/// transaction.
///
/// Plain receivables and e-invoices share this path; e-invoicing only adds
/// taking a track number, computing the tax and writing `einvoice`. The
/// rules for creating a receivable and its ledger entry must not diverge.
async fn insert_ar_invoice(tx: &mut Tx<'_>, req: InsertArInvoice<'_>) -> Result<String> {
    let voucher_no = insert_voucher(
        tx,
        req.issued_on,
        req.voucher_memo,
        req.lines,
        "pending",
        "invoice",
        req.created_by,
        None,
    )
    .await?;

    sqlx::query(
        "INSERT INTO ar_invoice
            (invoice_no, customer, issued_on, due_on, amount, memo, origin_voucher_no)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(req.invoice_no)
    .bind(req.customer)
    .bind(req.issued_on)
    .bind(req.due_on)
    .bind(req.amount)
    .bind(req.invoice_memo)
    .bind(&voucher_no)
    .execute(&mut **tx)
    .await?;

    Ok(voucher_no)
}

/// Settlement status of a subsidiary ledger row. `pending_label` is the
/// wording for money in transit (received versus paid).
fn settle_state(
    amount: Money,
    settled: Money,
    in_transit: Money,
    pending_label: &str,
) -> String {
    if settled >= amount {
        "settled".into()
    } else if in_transit > Money::ZERO {
        pending_label.into()
    } else {
        "open".into()
    }
}

/// Join a few names into one summary, eliding the rest. A voucher memo is
/// one line written for a person.
fn short_list(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1..=2 => names.join("、"),
        n => format!("{}、{} 等 {n} 家", names[0], names[1]),
    }
}

/// The GL account for a bank account. Finding none is a configuration
/// error; nothing is guessed.
async fn bank_gl_code(tx: &mut Tx<'_>, bank_account_id: i32) -> Result<String> {
    sqlx::query("SELECT gl_code FROM bank_account WHERE id = $1")
        .bind(bank_account_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|r| r.get("gl_code"))
        .ok_or(StoreError::BankAccountNotFound { id: bank_account_id })
}

/// ROC year and month for leave request numbers. Calendar conversion, not a
/// regulation value.
fn roc_month(date: NaiveDate) -> String {
    let roc_year = date.year() - 1911;
    format!("{roc_year:03}{:02}", date.month())
}

/// Take the next number from `doc_sequence`, restarting at 1 in a new
/// period.
async fn next_doc_no(tx: &mut Tx<'_>, prefix: &str, period: &str) -> Result<i32> {
    Ok(sqlx::query(
        "INSERT INTO doc_sequence (prefix, period, next_no) VALUES ($1, $2, 2)
         ON CONFLICT (prefix) DO UPDATE
           SET next_no = CASE WHEN doc_sequence.period = EXCLUDED.period
                              THEN doc_sequence.next_no + 1 ELSE 2 END,
               period  = EXCLUDED.period
         RETURNING CASE WHEN next_no = 2 THEN 1 ELSE next_no - 1 END",
    )
    .bind(prefix)
    .bind(period)
    .fetch_one(&mut **tx)
    .await?
    .get(0))
}

/// Write one voucher inside an existing transaction.
///
/// Extracted so that operations writing a voucher and a subsidiary ledger
/// row together can share one transaction; half of that succeeding would
/// leave the two disagreeing.
#[allow(clippy::too_many_arguments)]
async fn insert_voucher(
    tx: &mut Tx<'_>,
    date: NaiveDate,
    memo: &str,
    lines: &[LineInput],
    status: &str,
    source: &str,
    created_by: &str,
    audit_id: Option<i64>,
) -> Result<String> {
    laysi_domain::validate_entry(lines)?;

    for l in lines {
        // A reversal must land exactly on the original lines' accounts. Even if
        // an account was later disabled or made non-postable, historical
        // entries must not be moved to a different account. Every other new
        // voucher still accepts active, postable accounts only.
        let postable: Option<bool> = sqlx::query(
            "SELECT CASE WHEN $2 THEN true ELSE is_postable AND is_active END AS usable
               FROM account WHERE code = $1",
        )
        .bind(&l.account_code)
        .bind(source == "reversal")
        .fetch_optional(&mut **tx)
        .await?
        .map(|r| r.get("usable"));
        if postable != Some(true) {
            return Err(StoreError::UnknownAccount { code: l.account_code.clone() });
        }
    }

    // Number format V{yy}{mm}-{nnn}, e.g. V2607-018.
    let period = date.format("%y%m").to_string();
    let seq = next_doc_no(tx, "V", &period).await?;
    let no = format!("V{period}-{seq:03}");
    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO voucher (id, no, voucher_date, memo, status, source, created_by, ai_audit_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(&no)
    .bind(date)
    .bind(memo)
    .bind(status)
    .bind(source)
    .bind(created_by)
    .bind(audit_id)
    .execute(&mut **tx)
    .await?;

    for (i, l) in lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO voucher_line
               (voucher_id, line_no, account_code, debit, credit, memo, bank_account_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(id)
        .bind(i as i32 + 1)
        .bind(&l.account_code)
        .bind(l.debit)
        .bind(l.credit)
        .bind(&l.memo)
        .bind(l.bank_account_id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(no)
}

/// Attach a department to one journal line.
///
/// `LineInput` has no such field (it is a `laysi-domain` type and knows
/// nothing about dimensions), so a follow-up UPDATE sets it. The voucher is
/// still `pending` at that point and can be modified.
async fn set_line_dept(
    tx: &mut Tx<'_>,
    voucher_no: &str,
    line_no: i32,
    dept_id: Option<i32>,
) -> Result<()> {
    sqlx::query(
        "UPDATE voucher_line l SET dept_id = $3
           FROM voucher v
          WHERE v.id = l.voucher_id AND v.no = $1 AND l.line_no = $2",
    )
    .bind(voucher_no)
    .bind(line_no)
    .bind(dept_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn row_to_employee(r: &PgRow) -> Employee {
    Employee {
        id: r.get("id"),
        emp_no: r.get("emp_no"),
        name: r.get("name"),
        dept_id: r.get("dept_id"),
        dept_code: r.get("dept_code"),
        dept_label: r.get("dept_label"),
        title: r.get("title"),
        role_code: r.get("role_code"),
        role_name: r.get("role_name"),
        employment_type: r.get("employment_type"),
        hired_on: r.get("hired_on"),
        left_on: r.get("left_on"),
        status: r.get("status"),
        base_salary: r.get("base_salary"),
        labor_insured_salary: r.get("labor_insured_salary"),
        health_insured_salary: r.get("health_insured_salary"),
        pension_wage: r.get("pension_wage"),
        id_no: r.get("id_no"),
        bank_account: r.get("bank_account"),
        note: r.get("note"),
    }
}

fn row_to_account(r: &PgRow) -> Account {
    Account {
        code: r.get("code"),
        name: r.get("name"),
        kind: r.get("kind"),
        normal_side: r.get("normal_side"),
        is_postable: r.get("is_postable"),
        is_active: r.get("is_active"),
        parent_code: r.get("parent_code"),
        sort_order: r.get("sort_order"),
        report_group: r.get("report_group"),
        cash_flow_section: r.get("cash_flow_section"),
    }
}

fn row_to_leave(r: &PgRow) -> LeaveRequest {
    LeaveRequest {
        leave_no: r.get("leave_no"),
        employee_id: r.get("employee_id"),
        emp_no: r.get("emp_no"),
        name: r.get("name"),
        dept_label: r.get("dept_label"),
        kind: r.get("kind"),
        from_on: r.get("from_on"),
        to_on: r.get("to_on"),
        days: r.get("days"),
        reason: r.get("reason"),
        status: r.get("status"),
        decided_by: r.get("decided_by"),
        decided_role: r.get("decided_role"),
        decided_at: r.get("decided_at"),
        decision_note: r.get("decision_note"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
    }
}

fn punch_row(r: &PgRow) -> AttendancePunch {
    AttendancePunch {
        id: r.get("id"),
        employee_id: r.get("employee_id"),
        emp_no: r.get("emp_no"),
        name: r.get("name"),
        dept_label: r.get("dept_label"),
        punched_on: r.get("punched_on"),
        punched_at: r.get("punched_at"),
        kind: r.get("kind"),
        source: r.get("source"),
        note: r.get("note"),
        created_by: r.get("created_by"),
    }
}

fn row_to_payroll_run(r: &PgRow) -> PayrollRun {
    PayrollRun {
        period: r.get("period"),
        pay_on: r.get("pay_on"),
        headcount: r.get("headcount"),
        gross_total: r.get("gross_total"),
        deduction_total: r.get("deduction_total"),
        net_total: r.get("net_total"),
        employer_total: r.get("employer_total"),
        voucher_no: r.get("voucher_no"),
        voucher_status: r.get("voucher_status"),
        labor_version: r.get("labor_version"),
        health_version: r.get("health_version"),
        pension_version: r.get("pension_version"),
        withholding_version: r.get("withholding_version"),
        note: r.get("note"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
    }
}

fn row_to_payslip(r: &PgRow) -> Payslip {
    Payslip {
        period: r.get("period"),
        pay_on: r.get("pay_on"),
        labor_version: r.get("labor_version"),
        health_version: r.get("health_version"),
        pension_version: r.get("pension_version"),
        withholding_version: r.get("withholding_version"),
        employee_id: r.get("employee_id"),
        emp_no: r.get("emp_no"),
        name: r.get("name"),
        title: r.get("title"),
        dept_label: r.get("dept_label"),
        base_salary: r.get("base_salary"),
        overtime: r.get("overtime"),
        allowance: r.get("allowance"),
        gross: r.get("gross"),
        labor_insured_salary: r.get("labor_insured_salary"),
        health_insured_salary: r.get("health_insured_salary"),
        pension_wage: r.get("pension_wage"),
        labor_employee: r.get("labor_employee"),
        health_employee: r.get("health_employee"),
        income_tax: r.get("income_tax"),
        labor_employer: r.get("labor_employer"),
        health_employer: r.get("health_employer"),
        pension_employer: r.get("pension_employer"),
        deduction: r.get("deduction"),
        net: r.get("net"),
        paid_days: r.get("paid_days"),
        month_days: r.get("month_days"),
        note: r.get("note"),
    }
}

fn row_to_document(r: &PgRow) -> DocumentRow {
    DocumentRow {
        doc_no: r.get("doc_no"),
        name: r.get("name"),
        kind: r.get("kind"),
        link_type: r.get("link_type"),
        link_ref: r.get("link_ref"),
        archived: r.get("archived"),
        note: r.get("note"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
        version_no: r.get("version_no"),
        versions: r.get("versions"),
        object_key: r.get("object_key"),
        file_name: r.get("file_name"),
        byte_size: r.get("byte_size"),
        total_bytes: r.get("total_bytes"),
        content_type: r.get("content_type"),
        checksum: r.get("checksum"),
        ocr_status: r.get("ocr_status"),
        uploaded_at: r.get("uploaded_at"),
        uploaded_by: r.get("uploaded_by"),
    }
}

fn row_to_import_batch(r: &PgRow, issues: &[PgRow]) -> ImportBatch {
    let id: i32 = r.get("id");
    ImportBatch {
        batch_no: r.get("batch_no"),
        source_code: r.get("source_code"),
        source_label: r.get("source_label"),
        file_name: r.get("file_name"),
        row_count: r.get("row_count"),
        ok_count: r.get("ok_count"),
        error_count: r.get("error_count"),
        warn_count: r.get("warn_count"),
        status: r.get("status"),
        voucher_nos: r.get("voucher_nos"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
        settled_at: r.get("settled_at"),
        settled_by: r.get("settled_by"),
        issues: issues
            .iter()
            .filter(|i| i.get::<i32, _>("batch_id") == id)
            .map(|i| ImportIssue {
                row_no: i.get("row_no"),
                severity: i.get("severity"),
                issue: i.get("issue"),
                fix: i.get("fix"),
            })
            .collect(),
    }
}

fn row_to_movement(r: &PgRow) -> AccountMovement {
    // PostgreSQL returns SUM(NUMERIC) as Decimal and COUNT() as i64. The types
    // are stated explicitly so column inference cannot read an aggregate at
    // the wrong integer width.
    let debit: Money = r.get("debit");
    let credit: Money = r.get("credit");
    let line_count: i64 = r.get("line_count");
    AccountMovement {
        code: r.get("code"),
        name: r.get("name"),
        kind: r.get("kind"),
        normal_side: r.get("normal_side"),
        is_postable: r.get("is_postable"),
        is_active: r.get("is_active"),
        report_group: r.get("report_group"),
        cash_flow_section: r.get("cash_flow_section"),
        debit,
        credit,
        line_count,
    }
}

/// The account INSERT, taking a connection so that `create_user` and
/// `bootstrap_first_user` cannot drift into two different sets of columns.
async fn insert_user(conn: &mut sqlx::PgConnection, req: CreateUser<'_>) -> Result<UserAccount> {
    let row = sqlx::query(
        "INSERT INTO user_account
                (username, display_name, email, password_hash, role_code, employee_id,
                 status, must_change_password, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
         RETURNING id, username, display_name, email, password_hash, role_code,
                   employee_id, status, must_change_password, created_at, created_by,
                   last_login_at",
    )
    .bind(req.username)
    .bind(req.display_name)
    .bind(req.email)
    .bind(req.password_hash)
    .bind(req.role_code)
    .bind(req.employee_id)
    .bind(req.status)
    .bind(req.must_change_password)
    .bind(req.created_by)
    .fetch_one(conn)
    .await?;
    Ok(row_to_user_account(&row))
}

fn row_to_user_account(r: &PgRow) -> UserAccount {
    UserAccount {
        id: r.get("id"),
        username: r.get("username"),
        display_name: r.get("display_name"),
        email: r.get("email"),
        password_hash: r.get("password_hash"),
        role_code: r.get("role_code"),
        employee_id: r.get("employee_id"),
        status: r.get("status"),
        must_change_password: r.get("must_change_password"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
        last_login_at: r.get("last_login_at"),
    }
}

fn row_to_user_session(r: &PgRow) -> UserSession {
    UserSession {
        id: r.get("session_id"),
        user: UserAccount {
            id: r.get("account_id"),
            username: r.get("username"),
            display_name: r.get("display_name"),
            email: r.get("email"),
            password_hash: r.get("password_hash"),
            role_code: r.get("role_code"),
            employee_id: r.get("employee_id"),
            status: r.get("status"),
            must_change_password: r.get("must_change_password"),
            created_at: r.get("account_created_at"),
            created_by: r.get("created_by"),
            last_login_at: r.get("last_login_at"),
        },
        created_at: r.get("session_created_at"),
        last_seen_at: r.get("last_seen_at"),
        expires_at: r.get("expires_at"),
        revoked_at: r.get("revoked_at"),
        revoked_by: r.get("revoked_by"),
        user_agent: r.get("user_agent"),
        ip: r.get("ip"),
    }
}

fn row_to_api_token(r: &PgRow) -> ApiToken {
    ApiToken {
        id: r.get("id"),
        name: r.get("name"),
        kind: r.get("kind"),
        scope: r.get("scope"),
        prefix: r.get("prefix"),
        actor: r.get("actor"),
        actor_role: r.get("actor_role"),
        actor_role_name: r.get("actor_role_name"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
        last_used_at: r.get("last_used_at"),
        rotate_days: r.get("rotate_days"),
        expiry_note: r.get("expiry_note"),
        revoked_at: r.get("revoked_at"),
        revoked_by: r.get("revoked_by"),
        owner_employee_id: r.get("owner_employee_id"),
    }
}

/// Token hash: BLAKE3 as hex.
///
/// The same hash function as the brand integrity check (see `laysi-brand`).
/// Cryptographic strength is required here: whoever obtains the database
/// must not be able to derive a working token, and a glance-level digest
/// like `fnv1a` would be nowhere near enough.
pub fn token_hash(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}

fn row_to_bank_txn(r: &PgRow) -> BankTxn {
    BankTxn {
        id: r.get("id"),
        bank_account_id: r.get("bank_account_id"),
        bank_name: r.get("bank_name"),
        account_no: r.get("account_no"),
        txn_date: r.get("txn_date"),
        memo: r.get("memo"),
        amount: r.get("amount"),
        kind: r.get("kind"),
        matched_voucher_no: r.get("matched_voucher_no"),
        matched_at: r.get("matched_at"),
        matched_by: r.get("matched_by"),
    }
}

/// Short digest of a rule file's contents for `reg_ruleset.checksum`.
///
/// Cryptographic strength is not needed here: brand integrity uses BLAKE3
/// plus a signature (see `laysi-brand`), and this column only makes it
/// obvious at a glance whether a rule file changed.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leave_number_period_uses_republic_era_year_and_month() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 26).unwrap();
        assert_eq!(roc_month(date), "11507");
    }

    #[test]
    fn approval_flow_cannot_delete_its_last_step() {
        assert!(ensure_approval_rule_can_delete(2).is_ok());
        assert!(matches!(
            ensure_approval_rule_can_delete(1),
            Err(StoreError::LastApprovalRule)
        ));
        assert!(matches!(
            ensure_approval_rule_can_delete(0),
            Err(StoreError::LastApprovalRule)
        ));
    }

    #[test]
    fn only_posted_vouchers_can_transition_to_void() {
        assert!(ensure_voidable("VTEST", "posted").is_ok());

        for status in ["draft", "pending", "void"] {
            let error = ensure_voidable("VTEST", status).expect_err("非已過帳傳票不得作廢");
            assert!(
                matches!(error, StoreError::NotVoidable { .. }),
                "應回傳作廢狀態錯誤"
            );
            assert!(error.to_string().contains(status), "錯誤訊息沒有指出目前狀態");
        }
    }

    fn sample_token(created_at: DateTime<Utc>, rotate_days: Option<i32>) -> ApiToken {
        ApiToken {
            id: 1,
            name: "測試 Token".into(),
            kind: "mcp".into(),
            scope: "測試".into(),
            prefix: "lys_test".into(),
            actor: "測試員".into(),
            actor_role: "acct".into(),
            actor_role_name: None,
            created_at,
            created_by: "測試".into(),
            last_used_at: None,
            rotate_days,
            expiry_note: String::new(),
            revoked_at: None,
            revoked_by: String::new(),
            owner_employee_id: None,
        }
    }

    /// No `rotate_days` means no fixed cycle and no expiry; revocation stays
    /// the only way to stop it.
    #[test]
    fn a_token_without_rotate_days_never_expires() {
        let created_at = Utc::now() - chrono::Duration::days(3650);
        let token = sample_token(created_at, None);
        assert_eq!(token.expires_at(), None);
        assert!(!token.is_expired(Utc::now()));
    }

    /// Expiry is `created_at + rotate_days`, not "last use + rotate_days";
    /// that is the rotation reminder on the listing screen, a different thing.
    #[test]
    fn a_token_expires_rotate_days_after_creation_not_after_last_use() {
        let created_at = Utc::now() - chrono::Duration::days(31);
        let token = sample_token(created_at, Some(30));
        assert_eq!(token.expires_at(), Some(created_at + chrono::Duration::days(30)));
        assert!(token.is_expired(Utc::now()), "建立後 31 天、輪替 30 天的 token 應已過期");

        let fresh = sample_token(Utc::now() - chrono::Duration::days(1), Some(30));
        assert!(!fresh.is_expired(Utc::now()), "建立後 1 天、輪替 30 天的 token 不該過期");
    }

    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-store
    /// voucher_correction_database_properties -- --ignored`
    ///
    /// The test creates its own accounts and vouchers in the throwaway
    /// database it is given.
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn voucher_correction_database_properties() {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = Store::connect(&database_url, 2)
            .await
            .expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");

        let suffix = Uuid::new_v4().simple().to_string();
        let debit_code = format!("T{}D", &suffix[..8]);
        let credit_code = format!("T{}C", &suffix[..8]);
        sqlx::query(
            "INSERT INTO account
                (code, name, kind, normal_side, is_postable, sort_order,
                 report_group, cash_flow_section, is_active)
             VALUES
                ($1, '沖銷測試資產', 'asset', 'debit', true, 9990,
                 'current_asset', 'operating', true),
                ($2, '沖銷測試權益', 'equity', 'credit', true, 9991,
                 'equity', 'financing', true)",
        )
        .bind(&debit_code)
        .bind(&credit_code)
        .execute(store.pool())
        .await
        .expect("應可建立測試科目");

        let date = NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();
        let amount: Money = "125.00".parse().unwrap();
        let lines = vec![
            LineInput {
                account_code: debit_code.clone(),
                debit: amount,
                credit: Money::ZERO,
                memo: Some("原始借方".into()),
                bank_account_id: None,
            },
            LineInput {
                account_code: credit_code.clone(),
                debit: Money::ZERO,
                credit: amount,
                memo: Some("原始貸方".into()),
                bank_account_id: None,
            },
        ];

        let before = store.ledger_movements(Some(date), date).await.unwrap();
        let before_debit = before.iter().find(|row| row.code == debit_code).unwrap();
        let before_credit = before.iter().find(|row| row.code == credit_code).unwrap();
        assert_eq!(before_debit.debit, Money::ZERO);
        assert_eq!(before_debit.credit, Money::ZERO);
        assert_eq!(before_credit.debit, Money::ZERO);
        assert_eq!(before_credit.credit, Money::ZERO);

        let original_no = store
            .create_voucher(date, "需要沖銷的傳票", &lines, "pending", "manual", "測試", None)
            .await
            .unwrap();
        store.post_voucher(&original_no).await.unwrap();

        let reversal = store.reverse_voucher(&original_no, date, "測試").await.unwrap();
        assert_eq!(reversal.status, "posted");
        assert_eq!(reversal.source, "reversal");
        assert_eq!(reversal.reverses.as_deref(), Some(original_no.as_str()));
        assert_eq!(reversal.date, date);
        let reversed_debit = reversal
            .lines
            .iter()
            .find(|line| line.account_code == debit_code)
            .unwrap();
        let reversed_credit = reversal
            .lines
            .iter()
            .find(|line| line.account_code == credit_code)
            .unwrap();
        assert_eq!(reversed_debit.debit, Money::ZERO);
        assert_eq!(reversed_debit.credit, amount);
        assert_eq!(reversed_credit.debit, amount);
        assert_eq!(reversed_credit.credit, Money::ZERO);

        let original = store.voucher_by_no(&original_no).await.unwrap();
        assert_eq!(original.reversed_by.as_deref(), Some(reversal.no.as_str()));

        let after_reversal = store.ledger_movements(Some(date), date).await.unwrap();
        let after_reversal_debit =
            after_reversal.iter().find(|row| row.code == debit_code).unwrap();
        let after_reversal_credit =
            after_reversal.iter().find(|row| row.code == credit_code).unwrap();
        assert_eq!(
            after_reversal_debit.net_debit(),
            before_debit.net_debit(),
            "沖銷後借方科目餘額應回到原傳票之前"
        );
        assert_eq!(
            after_reversal_credit.net_credit(),
            before_credit.net_credit(),
            "沖銷後貸方科目餘額應回到原傳票之前"
        );

        let trial = store.trial_balance(Some(date)).await.unwrap();
        let total_debit: Money = trial.iter().map(|row| row.debit).sum();
        let total_credit: Money = trial.iter().map(|row| row.credit).sum();
        assert_eq!(total_debit, total_credit, "沖銷後試算表仍要借貸相等");
        assert_eq!(
            trial.iter().find(|row| row.code == debit_code).unwrap().balance,
            Money::ZERO
        );
        assert_eq!(
            trial.iter().find(|row| row.code == credit_code).unwrap().balance,
            Money::ZERO
        );

        let duplicate = store
            .reverse_voucher(&original_no, date, "測試")
            .await
            .expect_err("同一張傳票不得重複沖銷");
        assert!(matches!(
            duplicate,
            StoreError::AlreadyReversed {
                ref reversed_by,
                ..
            } if reversed_by == &reversal.no
        ));

        let void_amount: Money = "75.00".parse().unwrap();
        let void_lines = vec![
            LineInput {
                account_code: debit_code.clone(),
                debit: void_amount,
                credit: Money::ZERO,
                memo: None,
                bank_account_id: None,
            },
            LineInput {
                account_code: credit_code.clone(),
                debit: Money::ZERO,
                credit: void_amount,
                memo: None,
                bank_account_id: None,
            },
        ];
        let void_no = store
            .create_voucher(
                date,
                "不應存在的傳票",
                &void_lines,
                "pending",
                "manual",
                "測試",
                None,
            )
            .await
            .unwrap();
        store.post_voucher(&void_no).await.unwrap();
        let with_void = store.ledger_movements(Some(date), date).await.unwrap();
        let with_void_debit = with_void.iter().find(|row| row.code == debit_code).unwrap();
        assert_eq!(with_void_debit.debit, after_reversal_debit.debit + void_amount);

        let voided = store.void_voucher(&void_no).await.unwrap();
        assert_eq!(voided.status, "void");
        assert_eq!(voided.lines.len(), void_lines.len(), "作廢不得刪除分錄");

        let after_void = store.ledger_movements(Some(date), date).await.unwrap();
        let after_void_debit = after_void.iter().find(|row| row.code == debit_code).unwrap();
        let after_void_credit = after_void.iter().find(|row| row.code == credit_code).unwrap();
        assert_eq!(after_void_debit.debit, after_reversal_debit.debit);
        assert_eq!(after_void_debit.credit, after_reversal_debit.credit);
        assert_eq!(after_void_credit.debit, after_reversal_credit.debit);
        assert_eq!(after_void_credit.credit, after_reversal_credit.credit);

        let second_void = store.void_voucher(&void_no).await.expect_err("不得重複作廢");
        assert!(
            matches!(second_void, StoreError::NotVoidable { ref status, .. } if status == "void")
        );
        assert!(second_void.to_string().contains("void"));
    }
}
