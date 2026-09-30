//! Upload links and the files they collect before they are filed.
//!
//! A link is one `upload_batch` row: the BLAKE3 of its secret, who holds it,
//! how many files it may still take and when it stops. Every file posted to
//! it becomes an `upload_staging` row pointing at an object under
//! [`STAGING_PREFIX`]. `upload_document` later takes a staged file by id, which
//! is when it becomes a document; until then it belongs to nobody but its
//! holder and is removed a day after it arrived.
//!
//! The holder decides who may see and take a staged file, and is checked in
//! SQL on every read, so an id that belongs to somebody else is simply not
//! found. It is also checked live when a file arrives: a link outlives
//! nothing its holder has lost, whether that is a revoked token or an account
//! suspended or moved to another role.

use crate::{token_hash, Result, Store};
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

/// Object key prefix of every staged file. Nothing else lives under it, so
/// the sweep can treat anything there that no row points at as an orphan.
pub const STAGING_PREFIX: &str = "staging/";

/// Who a link belongs to: a signed-in account or an MCP token, never both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadHolder {
    Account(i32),
    Token(i32),
}

impl UploadHolder {
    fn columns(self) -> (Option<i32>, Option<i32>) {
        match self {
            UploadHolder::Account(id) => (Some(id), None),
            UploadHolder::Token(id) => (None, Some(id)),
        }
    }
}

/// Whether the holder of batch `b` can still act, in the same terms `/mcp`
/// and the session check use: an active account under the role the link was
/// issued to, or a token that is neither revoked nor expired and whose owner
/// still qualifies.
macro_rules! holder_active {
    () => {
        concat!(
            "(CASE
                WHEN b.owner_user_id IS NOT NULL THEN
                  EXISTS (SELECT 1 FROM user_account u
                           WHERE u.id = b.owner_user_id
                             AND u.status = 'active'
                             AND u.role_code = b.actor_role)
                ELSE
                  EXISTS (SELECT 1 FROM api_token t
                           WHERE t.id = b.owner_token_id
                             AND t.revoked_at IS NULL
                             AND t.actor_role = b.actor_role
                             AND (t.rotate_days IS NULL
                                  OR now() < t.created_at + make_interval(days => t.rotate_days))
                             AND ",
            token_owner_active!(),
            ")
              END)"
        )
    };
}

/// A link to be issued.
pub struct NewUploadBatch<'a> {
    pub holder: UploadHolder,
    pub actor: &'a str,
    pub actor_role: &'a str,
    /// `ui` or `mcp`.
    pub channel: &'a str,
    pub max_files: i32,
    pub expires_at: DateTime<Utc>,
}

/// An issued link. The secret is in [`IssuedUploadBatch`] only, once.
#[derive(Debug, Clone)]
pub struct UploadBatch {
    pub id: Uuid,
    pub holder: UploadHolder,
    pub actor: String,
    pub actor_role: String,
    pub channel: String,
    pub max_files: i32,
    pub used_files: i32,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Whether the holder still qualifies. Only filled by
    /// [`Store::upload_batch_by_secret`], where the link is being used.
    pub holder_active: bool,
}

impl UploadBatch {
    pub fn is_expired(&self, at: DateTime<Utc>) -> bool {
        at >= self.expires_at
    }
}

pub struct IssuedUploadBatch {
    pub batch: UploadBatch,
    /// The link secret in plain text. The table keeps only its hash.
    pub secret: String,
}

/// One file received through a link.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StagedUpload {
    pub id: Uuid,
    pub batch_id: Uuid,
    pub file_name: String,
    pub byte_size: i64,
    pub content_type: String,
    pub checksum: String,
    #[serde(skip)]
    pub object_key: String,
    pub created_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub consumed_doc_no: Option<String>,
}

/// A file that has landed in the bucket and is about to be recorded.
pub struct NewStagedUpload<'a> {
    pub id: Uuid,
    pub batch_id: Uuid,
    pub file_name: &'a str,
    pub byte_size: i64,
    pub content_type: &'a str,
    pub checksum: &'a str,
    pub object_key: &'a str,
}

/// Where a staged object lives: the batch, then the file id. The file name
/// the sender chose never reaches the key.
pub fn staging_key(batch_id: Uuid, upload_id: Uuid) -> String {
    format!("{STAGING_PREFIX}{batch_id}/{upload_id}")
}

// Macros rather than consts: sqlx takes only `&'static str`, which `concat!`
// can build and `format!` cannot.
macro_rules! batch_columns {
    () => {
        "b.id, b.owner_user_id, b.owner_token_id, b.actor, b.actor_role,
         b.channel, b.max_files, b.used_files, b.created_at, b.expires_at"
    };
}

macro_rules! staged_columns {
    () => {
        "s.id, s.batch_id, s.file_name, s.byte_size, s.content_type,
         s.checksum, s.object_key, s.created_at, s.consumed_at, s.consumed_doc_no"
    };
}

fn row_to_batch(row: &PgRow, holder_active: bool) -> UploadBatch {
    let user: Option<i32> = row.get("owner_user_id");
    let token: Option<i32> = row.get("owner_token_id");
    let holder = match (user, token) {
        (Some(id), _) => UploadHolder::Account(id),
        (None, Some(id)) => UploadHolder::Token(id),
        // The CHECK constraint rules this out; a row that slipped past it
        // belongs to nobody, which is what a token id of 0 matches.
        (None, None) => UploadHolder::Token(0),
    };
    UploadBatch {
        id: row.get("id"),
        holder,
        actor: row.get("actor"),
        actor_role: row.get("actor_role"),
        channel: row.get("channel"),
        max_files: row.get("max_files"),
        used_files: row.get("used_files"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
        holder_active,
    }
}

fn row_to_staged(row: &PgRow) -> StagedUpload {
    StagedUpload {
        id: row.get("id"),
        batch_id: row.get("batch_id"),
        file_name: row.get("file_name"),
        byte_size: row.get("byte_size"),
        content_type: row.get("content_type"),
        checksum: row.get("checksum"),
        object_key: row.get("object_key"),
        created_at: row.get("created_at"),
        consumed_at: row.get("consumed_at"),
        consumed_doc_no: row.get("consumed_doc_no"),
    }
}

impl Store {
    /// Issue a link. The secret appears only in the return value.
    pub async fn create_upload_batch(&self, req: NewUploadBatch<'_>) -> Result<IssuedUploadBatch> {
        // Same construction as an API token: two v4 UUIDs, 244 bits from the
        // operating system CSPRNG.
        let secret = format!(
            "lysu_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let (owner_user_id, owner_token_id) = req.holder.columns();
        let row = sqlx::query(concat!(
            "INSERT INTO upload_batch AS b
               (id, secret_hash, owner_user_id, owner_token_id, actor, actor_role, channel,
                max_files, expires_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             RETURNING ",
            batch_columns!()
        ))
        .bind(Uuid::new_v4())
        .bind(token_hash(&secret))
        .bind(owner_user_id)
        .bind(owner_token_id)
        .bind(req.actor)
        .bind(req.actor_role)
        .bind(req.channel)
        .bind(req.max_files)
        .bind(req.expires_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(IssuedUploadBatch {
            batch: row_to_batch(&row, true),
            secret,
        })
    }

    /// The link a secret opens, with whether its holder still qualifies.
    /// Expired links are returned too, so the caller can say which it was.
    pub async fn upload_batch_by_secret(&self, secret: &str) -> Result<Option<UploadBatch>> {
        let row = sqlx::query(concat!(
            "SELECT ",
            batch_columns!(),
            ", ",
            holder_active!(),
            " AS holder_active FROM upload_batch b WHERE b.secret_hash = $1"
        ))
        .bind(token_hash(secret))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let active = r.get("holder_active");
            row_to_batch(&r, active)
        }))
    }

    /// The link with this id, if it belongs to `holder`.
    pub async fn upload_batch_for(
        &self,
        batch_id: Uuid,
        holder: UploadHolder,
    ) -> Result<Option<UploadBatch>> {
        let (user, token) = holder.columns();
        let row = sqlx::query(concat!(
            "SELECT ",
            batch_columns!(),
            " FROM upload_batch b
              WHERE b.id = $1
                AND b.owner_user_id IS NOT DISTINCT FROM $2
                AND b.owner_token_id IS NOT DISTINCT FROM $3"
        ))
        .bind(batch_id)
        .bind(user)
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| row_to_batch(&r, true)))
    }

    /// Take one file place on a link, before any byte is read. `false` means
    /// the link is spent or has expired.
    ///
    /// One statement, so two uploads racing for the last place cannot both
    /// get it; the CHECK on `used_files` backs it.
    pub async fn claim_upload_place(&self, batch_id: Uuid) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE upload_batch SET used_files = used_files + 1
              WHERE id = $1 AND used_files < max_files AND expires_at > now()",
        )
        .bind(batch_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0)
    }

    /// Give a place back after an upload that did not land.
    pub async fn release_upload_place(&self, batch_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE upload_batch SET used_files = used_files - 1
              WHERE id = $1 AND used_files > 0",
        )
        .bind(batch_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a file that is already in the bucket.
    pub async fn record_staged_upload(&self, req: NewStagedUpload<'_>) -> Result<StagedUpload> {
        let row = sqlx::query(concat!(
            "INSERT INTO upload_staging AS s
               (id, batch_id, file_name, byte_size, content_type, checksum, object_key)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             RETURNING ",
            staged_columns!()
        ))
        .bind(req.id)
        .bind(req.batch_id)
        .bind(req.file_name)
        .bind(req.byte_size)
        .bind(req.content_type)
        .bind(req.checksum)
        .bind(req.object_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(row_to_staged(&row))
    }

    /// Every file a link has received, oldest first.
    pub async fn staged_uploads(&self, batch_id: Uuid) -> Result<Vec<StagedUpload>> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            staged_columns!(),
            " FROM upload_staging s WHERE s.batch_id = $1 ORDER BY s.created_at, s.id"
        ))
        .bind(batch_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_staged).collect())
    }

    /// Take a staged file for filing, if it belongs to `holder` and nothing
    /// has taken it yet.
    ///
    /// Marked taken in the same statement that finds it, so two calls with
    /// the same id cannot both file it. A caller whose filing then fails puts
    /// it back with [`Store::return_staged_upload`].
    pub async fn take_staged_upload(
        &self,
        upload_id: Uuid,
        holder: UploadHolder,
    ) -> Result<Option<StagedUpload>> {
        let (user, token) = holder.columns();
        let row = sqlx::query(concat!(
            "UPDATE upload_staging s SET consumed_at = now()
               FROM upload_batch b
              WHERE s.id = $1
                AND s.consumed_at IS NULL
                AND b.id = s.batch_id
                AND b.owner_user_id IS NOT DISTINCT FROM $2
                AND b.owner_token_id IS NOT DISTINCT FROM $3
              RETURNING ",
            staged_columns!()
        ))
        .bind(upload_id)
        .bind(user)
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_staged))
    }

    /// Whether a staged file exists for `holder` at all, taken or not. Only
    /// for choosing the message when [`Store::take_staged_upload`] finds
    /// nothing.
    pub async fn staged_upload_doc_no(
        &self,
        upload_id: Uuid,
        holder: UploadHolder,
    ) -> Result<Option<Option<String>>> {
        let (user, token) = holder.columns();
        let row = sqlx::query(
            "SELECT s.consumed_doc_no FROM upload_staging s
               JOIN upload_batch b ON b.id = s.batch_id
              WHERE s.id = $1
                AND b.owner_user_id IS NOT DISTINCT FROM $2
                AND b.owner_token_id IS NOT DISTINCT FROM $3",
        )
        .bind(upload_id)
        .bind(user)
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get("consumed_doc_no")))
    }

    /// Undo [`Store::take_staged_upload`] after a filing that failed.
    pub async fn return_staged_upload(&self, upload_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE upload_staging SET consumed_at = NULL
              WHERE id = $1 AND consumed_doc_no IS NULL",
        )
        .bind(upload_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record which document a staged file became.
    pub async fn mark_staged_upload_filed(&self, upload_id: Uuid, doc_no: &str) -> Result<()> {
        sqlx::query("UPDATE upload_staging SET consumed_doc_no = $2 WHERE id = $1")
            .bind(upload_id)
            .bind(doc_no)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Staged files nobody filed, received before `cutoff`, with their keys.
    pub async fn stale_staged_uploads(&self, cutoff: DateTime<Utc>) -> Result<Vec<(Uuid, String)>> {
        let rows = sqlx::query(
            "SELECT id, object_key FROM upload_staging
              WHERE consumed_at IS NULL AND created_at < $1",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("id"), r.get("object_key")))
            .collect())
    }

    /// Remove one staged file's row once its object is gone.
    pub async fn delete_staged_upload(&self, upload_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM upload_staging WHERE id = $1")
            .bind(upload_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Remove links that expired before `cutoff`, with the rows of files
    /// already filed from them. A link that still has a file waiting stays
    /// until the sweep has removed that file's object.
    pub async fn delete_spent_upload_batches(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        Ok(sqlx::query(
            "DELETE FROM upload_batch b
              WHERE b.expires_at < $1
                AND NOT EXISTS (SELECT 1 FROM upload_staging s
                                 WHERE s.batch_id = b.id AND s.consumed_at IS NULL)",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Object keys of staged files still waiting to be filed. Anything else
    /// under [`STAGING_PREFIX`] is an orphan.
    pub async fn waiting_staged_keys(&self) -> Result<std::collections::HashSet<String>> {
        let rows = sqlx::query("SELECT object_key FROM upload_staging WHERE consumed_at IS NULL")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("object_key")).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> Store {
        let url = std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
        let store = Store::connect(&url, 2).await.expect("應可連上測試資料庫");
        store.migrate().await.expect("測試資料庫 migrations 應成功");
        store
    }

    async fn account(store: &Store, role: &str) -> i32 {
        sqlx::query_scalar(
            "INSERT INTO user_account (username, display_name, role_code, status)
             VALUES ($1, '上傳測試', $2, 'active') RETURNING id",
        )
        .bind(format!(
            "upload-test-{}@example.com",
            Uuid::new_v4().simple()
        ))
        .bind(role)
        .fetch_one(&store.pool)
        .await
        .expect("應可建立測試帳號")
    }

    async fn token(store: &Store, role: &str) -> i32 {
        sqlx::query_scalar(
            "INSERT INTO api_token (name, kind, scope, prefix, token_hash, actor, actor_role, created_by)
             VALUES ('上傳測試', 'mcp', 'all', 'lys_test', $1, '上傳測試', $2, '測試')
             RETURNING id",
        )
        .bind(Uuid::new_v4().simple().to_string())
        .bind(role)
        .fetch_one(&store.pool)
        .await
        .expect("應可建立測試 Token")
    }

    async fn batch(store: &Store, holder: UploadHolder, max_files: i32) -> IssuedUploadBatch {
        store
            .create_upload_batch(NewUploadBatch {
                holder,
                actor: "上傳測試",
                actor_role: "acct",
                channel: "mcp",
                max_files,
                expires_at: Utc::now() + chrono::Duration::minutes(10),
            })
            .await
            .expect("應可發出上傳連結")
    }

    async fn staged(store: &Store, batch_id: Uuid) -> StagedUpload {
        let id = Uuid::new_v4();
        store
            .record_staged_upload(NewStagedUpload {
                id,
                batch_id,
                file_name: "發票.pdf",
                byte_size: 1234,
                content_type: "application/pdf",
                checksum: "ab".repeat(32).as_str(),
                object_key: &staging_key(batch_id, id),
            })
            .await
            .expect("應可登記暫存檔")
    }

    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-store
    /// a_link_takes_only_its_places_and_only_its_holder_files -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn a_link_takes_only_its_places_and_only_its_holder_files() {
        let store = test_store().await;
        let mine = UploadHolder::Token(token(&store, "acct").await);
        let theirs = UploadHolder::Token(token(&store, "acct").await);

        let issued = batch(&store, mine, 2).await;
        let found = store
            .upload_batch_by_secret(&issued.secret)
            .await
            .unwrap()
            .expect("秘密要找得到自己的連結");
        assert!(found.holder_active);
        assert!(store
            .upload_batch_by_secret("lysu_not-issued")
            .await
            .unwrap()
            .is_none());

        assert!(store.claim_upload_place(issued.batch.id).await.unwrap());
        assert!(store.claim_upload_place(issued.batch.id).await.unwrap());
        assert!(
            !store.claim_upload_place(issued.batch.id).await.unwrap(),
            "名額用完就不能再收"
        );
        store.release_upload_place(issued.batch.id).await.unwrap();
        assert!(
            store.claim_upload_place(issued.batch.id).await.unwrap(),
            "失敗的上傳退回名額之後可以再收"
        );

        let file = staged(&store, issued.batch.id).await;
        assert!(
            store
                .upload_batch_for(issued.batch.id, theirs)
                .await
                .unwrap()
                .is_none(),
            "別人的連結是查無此連結"
        );
        assert!(
            store
                .take_staged_upload(file.id, theirs)
                .await
                .unwrap()
                .is_none(),
            "別人的上傳編號是查無此上傳"
        );
        assert!(store
            .take_staged_upload(file.id, UploadHolder::Account(0))
            .await
            .unwrap()
            .is_none());

        let taken = store
            .take_staged_upload(file.id, mine)
            .await
            .unwrap()
            .expect("持有者取得到自己的檔案");
        assert_eq!(taken.object_key, file.object_key);
        assert!(
            store
                .take_staged_upload(file.id, mine)
                .await
                .unwrap()
                .is_none(),
            "同一個檔案不能存成兩份文件"
        );
        assert_eq!(
            store.staged_upload_doc_no(file.id, mine).await.unwrap(),
            Some(None),
            "取走但還沒存成文件"
        );

        store.return_staged_upload(file.id).await.unwrap();
        assert!(store
            .take_staged_upload(file.id, mine)
            .await
            .unwrap()
            .is_some());
        store
            .mark_staged_upload_filed(file.id, "D2609-001")
            .await
            .unwrap();
        store.return_staged_upload(file.id).await.unwrap();
        assert_eq!(
            store.staged_upload_doc_no(file.id, mine).await.unwrap(),
            Some(Some("D2609-001".into())),
            "存成文件之後不會被退回"
        );
    }

    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-store
    /// a_link_dies_with_its_holder -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn a_link_dies_with_its_holder() {
        let store = test_store().await;

        let token_id = token(&store, "acct").await;
        let issued = batch(&store, UploadHolder::Token(token_id), 1).await;
        sqlx::query("UPDATE api_token SET revoked_at = now() WHERE id = $1")
            .bind(token_id)
            .execute(&store.pool)
            .await
            .unwrap();
        let found = store
            .upload_batch_by_secret(&issued.secret)
            .await
            .unwrap()
            .unwrap();
        assert!(!found.holder_active, "撤銷 Token 之後連結跟著失效");

        let expiring = token(&store, "acct").await;
        let issued = batch(&store, UploadHolder::Token(expiring), 1).await;
        sqlx::query(
            "UPDATE api_token SET rotate_days = 1, created_at = now() - interval '2 days'
              WHERE id = $1",
        )
        .bind(expiring)
        .execute(&store.pool)
        .await
        .unwrap();
        let found = store
            .upload_batch_by_secret(&issued.secret)
            .await
            .unwrap()
            .unwrap();
        assert!(!found.holder_active, "過期的 Token 發出的連結也失效");

        let user = account(&store, "acct").await;
        let issued = store
            .create_upload_batch(NewUploadBatch {
                holder: UploadHolder::Account(user),
                actor: "上傳測試",
                actor_role: "acct",
                channel: "ui",
                max_files: 1,
                expires_at: Utc::now() + chrono::Duration::minutes(10),
            })
            .await
            .unwrap();
        let active = |store: &Store, secret: String| {
            let store = store.clone();
            async move {
                store
                    .upload_batch_by_secret(&secret)
                    .await
                    .unwrap()
                    .unwrap()
                    .holder_active
            }
        };
        assert!(active(&store, issued.secret.clone()).await);
        sqlx::query("UPDATE user_account SET role_code = 'staff' WHERE id = $1")
            .bind(user)
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            !active(&store, issued.secret.clone()).await,
            "換了角色，舊角色發出的連結不再收件"
        );
        sqlx::query(
            "UPDATE user_account SET role_code = 'acct', status = 'suspended' WHERE id = $1",
        )
        .bind(user)
        .execute(&store.pool)
        .await
        .unwrap();
        assert!(
            !active(&store, issued.secret.clone()).await,
            "停用帳號之後連結跟著失效"
        );

        let expired = store
            .create_upload_batch(NewUploadBatch {
                holder: UploadHolder::Token(token(&store, "acct").await),
                actor: "上傳測試",
                actor_role: "acct",
                channel: "mcp",
                max_files: 3,
                expires_at: Utc::now() - chrono::Duration::seconds(1),
            })
            .await
            .unwrap();
        assert!(
            !store.claim_upload_place(expired.batch.id).await.unwrap(),
            "過期的連結不收件"
        );
    }

    /// The sweep's database half, with a cutoff in the future standing in
    /// for a day having passed.
    ///
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-store
    /// what_nobody_filed_is_found_for_removal -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn what_nobody_filed_is_found_for_removal() {
        let store = test_store().await;
        let holder = UploadHolder::Token(token(&store, "acct").await);
        let issued = store
            .create_upload_batch(NewUploadBatch {
                holder,
                actor: "上傳測試",
                actor_role: "acct",
                channel: "mcp",
                max_files: 3,
                expires_at: Utc::now() - chrono::Duration::seconds(1),
            })
            .await
            .unwrap();
        let waiting = staged(&store, issued.batch.id).await;
        let filed = staged(&store, issued.batch.id).await;
        store.take_staged_upload(filed.id, holder).await.unwrap();
        store
            .mark_staged_upload_filed(filed.id, "D2609-002")
            .await
            .unwrap();

        let later = Utc::now() + chrono::Duration::hours(25);
        assert!(
            store
                .stale_staged_uploads(Utc::now() - chrono::Duration::hours(24))
                .await
                .unwrap()
                .iter()
                .all(|(id, _)| *id != waiting.id),
            "未滿一天的暫存檔不清"
        );
        let stale = store.stale_staged_uploads(later).await.unwrap();
        assert!(stale
            .iter()
            .any(|(id, key)| *id == waiting.id && *key == waiting.object_key));
        assert!(
            stale.iter().all(|(id, _)| *id != filed.id),
            "已存成文件的不是暫存"
        );
        let keys = store.waiting_staged_keys().await.unwrap();
        assert!(keys.contains(&waiting.object_key));
        assert!(!keys.contains(&filed.object_key));

        // A cutoff of now, not `later`: the other tests in this file hold
        // links that expire in ten minutes, and they run alongside this one.
        store.delete_spent_upload_batches(Utc::now()).await.unwrap();
        assert!(
            store
                .upload_batch_for(issued.batch.id, holder)
                .await
                .unwrap()
                .is_some(),
            "還有檔案在等的連結先留著"
        );
        store.delete_staged_upload(waiting.id).await.unwrap();
        store.delete_spent_upload_batches(Utc::now()).await.unwrap();
        assert!(
            store
                .upload_batch_for(issued.batch.id, holder)
                .await
                .unwrap()
                .is_none(),
            "過期而沒有檔案在等的連結刪掉"
        );
        assert!(
            store
                .staged_upload_doc_no(filed.id, holder)
                .await
                .unwrap()
                .is_none(),
            "連結刪掉時已存檔的紀錄一起刪"
        );
    }

    #[test]
    fn staged_keys_carry_no_file_name() {
        let batch = Uuid::new_v4();
        let upload = Uuid::new_v4();
        let key = staging_key(batch, upload);
        assert!(key.starts_with(STAGING_PREFIX));
        assert_eq!(key, format!("staging/{batch}/{upload}"));
        assert!(key.is_ascii());
    }
}
