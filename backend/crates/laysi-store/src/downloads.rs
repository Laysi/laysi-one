//! One-time download links.
//!
//! A link is one `download_link` row: the BLAKE3 of its secret, who holds it,
//! which document version it opens and until when. `GET /v1/downloads/{secret}`
//! finds the row by the secret, checks the holder live, marks the row used and
//! only then fetches the object, so a link is spent the moment anyone starts
//! using it and a second request finds it used.

use crate::links::LinkHolder;
use crate::{token_hash, Result, Store};
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

/// A link to be issued for one version of one document.
pub struct NewDownloadLink<'a> {
    pub holder: LinkHolder,
    pub actor: &'a str,
    pub actor_role: &'a str,
    /// `ui` or `mcp`.
    pub channel: &'a str,
    pub doc_no: &'a str,
    pub version_no: i32,
    pub expires_at: DateTime<Utc>,
}

pub struct IssuedDownloadLink {
    pub id: Uuid,
    pub expires_at: DateTime<Utc>,
    /// The link secret in plain text. The table keeps only its hash.
    pub secret: String,
}

/// A link as found by its secret, with the document version it opens.
#[derive(Debug, Clone)]
pub struct DownloadLink {
    pub id: Uuid,
    pub holder: LinkHolder,
    pub actor: String,
    pub actor_role: String,
    pub channel: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    /// Whether the holder still qualifies, checked when the row was read.
    pub holder_active: bool,
    pub doc_no: String,
    pub version_no: i32,
    pub file_name: String,
    pub byte_size: i64,
    pub content_type: String,
    pub checksum: String,
    pub object_key: String,
}

impl DownloadLink {
    pub fn is_expired(&self, at: DateTime<Utc>) -> bool {
        at >= self.expires_at
    }
}

impl Store {
    /// Issue a link. `None` when the document has no such version; the caller
    /// has already looked it up, so that only happens in a race with nothing.
    pub async fn create_download_link(
        &self,
        req: NewDownloadLink<'_>,
    ) -> Result<Option<IssuedDownloadLink>> {
        // Same construction as an API token: two v4 UUIDs, 244 bits from the
        // operating system CSPRNG.
        let secret = format!(
            "lysd_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let (owner_user_id, owner_token_id) = req.holder.columns();
        let row = sqlx::query(
            "INSERT INTO download_link
               (id, secret_hash, owner_user_id, owner_token_id, actor, actor_role, channel,
                document_version_id, expires_at)
             SELECT $1, $2, $3, $4, $5, $6, $7, v.id, $8
               FROM document_version v
               JOIN document d ON d.id = v.document_id
              WHERE d.doc_no = $9 AND v.version_no = $10
             RETURNING id, expires_at",
        )
        .bind(Uuid::new_v4())
        .bind(token_hash(&secret))
        .bind(owner_user_id)
        .bind(owner_token_id)
        .bind(req.actor)
        .bind(req.actor_role)
        .bind(req.channel)
        .bind(req.expires_at)
        .bind(req.doc_no)
        .bind(req.version_no)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| IssuedDownloadLink {
            id: r.get("id"),
            expires_at: r.get("expires_at"),
            secret,
        }))
    }

    /// The link a secret opens, with whether its holder still qualifies.
    /// Expired and used links are returned too, so the caller can say which
    /// it was.
    pub async fn download_link_by_secret(&self, secret: &str) -> Result<Option<DownloadLink>> {
        let row = sqlx::query(concat!(
            "SELECT b.id, b.owner_user_id, b.owner_token_id, b.actor, b.actor_role,
                    b.channel, b.expires_at, b.used_at,
                    d.doc_no, v.version_no, v.file_name, v.byte_size, v.content_type,
                    v.checksum, v.object_key, ",
            holder_active!(),
            " AS holder_active
               FROM download_link b
               JOIN document_version v ON v.id = b.document_version_id
               JOIN document d ON d.id = v.document_id
              WHERE b.secret_hash = $1"
        ))
        .bind(token_hash(secret))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| DownloadLink {
            id: r.get("id"),
            holder: LinkHolder::from_columns(r.get("owner_user_id"), r.get("owner_token_id")),
            actor: r.get("actor"),
            actor_role: r.get("actor_role"),
            channel: r.get("channel"),
            expires_at: r.get("expires_at"),
            used_at: r.get("used_at"),
            holder_active: r.get("holder_active"),
            doc_no: r.get("doc_no"),
            version_no: r.get("version_no"),
            file_name: r.get("file_name"),
            byte_size: r.get("byte_size"),
            content_type: r.get("content_type"),
            checksum: r.get("checksum"),
            object_key: r.get("object_key"),
        }))
    }

    /// Spend a link. `false` means it was already used or has expired.
    ///
    /// One statement, so two requests racing with the same link cannot both
    /// get it.
    pub async fn use_download_link(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE download_link SET used_at = now()
              WHERE id = $1 AND used_at IS NULL AND expires_at > now()",
        )
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0)
    }

    /// Remove links that expired before `cutoff`. Nothing reads a spent link;
    /// the audit trail is what records that it was issued and used.
    pub async fn delete_spent_download_links(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM download_link WHERE expires_at < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
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

    async fn token(store: &Store, role: &str) -> i32 {
        sqlx::query_scalar(
            "INSERT INTO api_token (name, kind, scope, prefix, token_hash, actor, actor_role, created_by)
             VALUES ('下載測試', 'mcp', 'all', 'lys_test', $1, '下載測試', $2, '測試')
             RETURNING id",
        )
        .bind(Uuid::new_v4().simple().to_string())
        .bind(role)
        .fetch_one(&store.pool)
        .await
        .expect("應可建立測試 Token")
    }

    /// A document with one version, straight into the tables: the object
    /// behind it does not matter to the link.
    async fn document(store: &Store) -> String {
        let doc_no = format!("DT-{}", Uuid::new_v4().simple());
        let id: i32 = sqlx::query_scalar(
            "INSERT INTO document (doc_no, name, kind, created_by)
             VALUES ($1, '下載測試', 'other', '測試') RETURNING id",
        )
        .bind(&doc_no)
        .fetch_one(&store.pool)
        .await
        .expect("應可建立測試文件");
        sqlx::query(
            "INSERT INTO document_version
               (document_id, version_no, object_key, file_name, byte_size, content_type,
                checksum, uploaded_by)
             VALUES ($1, 1, $2, '發票.pdf', 1234, 'application/pdf', $3, '測試')",
        )
        .bind(id)
        .bind(format!("test/{doc_no}"))
        .bind("ab".repeat(32))
        .execute(&store.pool)
        .await
        .expect("應可建立測試版本");
        doc_no
    }

    async fn link(
        store: &Store,
        holder: LinkHolder,
        doc_no: &str,
        expires_at: DateTime<Utc>,
    ) -> Option<IssuedDownloadLink> {
        store
            .create_download_link(NewDownloadLink {
                holder,
                actor: "下載測試",
                actor_role: "acct",
                channel: "mcp",
                doc_no,
                version_no: 1,
                expires_at,
            })
            .await
            .expect("應可發出下載連結")
    }

    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-store
    /// a_download_link_opens_once -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn a_download_link_opens_once() {
        let store = test_store().await;
        let holder = LinkHolder::Token(token(&store, "acct").await);
        let doc_no = document(&store).await;
        let soon = Utc::now() + chrono::Duration::minutes(5);

        let issued = link(&store, holder, &doc_no, soon)
            .await
            .expect("存在的版本要發得出連結");
        let found = store
            .download_link_by_secret(&issued.secret)
            .await
            .unwrap()
            .expect("秘密要找得到自己的連結");
        assert!(found.holder_active);
        assert_eq!(found.doc_no, doc_no);
        assert_eq!(found.version_no, 1);
        assert_eq!(found.checksum, "ab".repeat(32));
        assert!(found.used_at.is_none());
        assert!(store
            .download_link_by_secret("lysd_not-issued")
            .await
            .unwrap()
            .is_none());

        assert!(store.use_download_link(found.id).await.unwrap());
        assert!(
            !store.use_download_link(found.id).await.unwrap(),
            "同一條連結只能用一次"
        );
        assert!(
            store
                .download_link_by_secret(&issued.secret)
                .await
                .unwrap()
                .unwrap()
                .used_at
                .is_some(),
            "用過的連結仍找得到，才說得出它已經用過"
        );

        let expired = link(
            &store,
            holder,
            &doc_no,
            Utc::now() - chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
        assert!(
            !store.use_download_link(expired.id).await.unwrap(),
            "過期的連結不能用"
        );

        let missing = store
            .create_download_link(NewDownloadLink {
                holder,
                actor: "下載測試",
                actor_role: "acct",
                channel: "mcp",
                doc_no: &doc_no,
                version_no: 2,
                expires_at: soon,
            })
            .await
            .unwrap();
        assert!(missing.is_none(), "沒有的版本發不出連結");

        store.delete_spent_download_links(Utc::now()).await.unwrap();
        assert!(
            store
                .download_link_by_secret(&expired.secret)
                .await
                .unwrap()
                .is_none(),
            "過期的連結由清除工作刪掉"
        );
        assert!(
            store
                .download_link_by_secret(&issued.secret)
                .await
                .unwrap()
                .is_some(),
            "還沒過期的連結留著"
        );
    }

    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-store
    /// a_download_link_dies_with_its_holder -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可套用 migrations 的 PostgreSQL"]
    async fn a_download_link_dies_with_its_holder() {
        let store = test_store().await;
        let token_id = token(&store, "acct").await;
        let doc_no = document(&store).await;
        let issued = link(
            &store,
            LinkHolder::Token(token_id),
            &doc_no,
            Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE api_token SET revoked_at = now() WHERE id = $1")
            .bind(token_id)
            .execute(&store.pool)
            .await
            .unwrap();
        let found = store
            .download_link_by_secret(&issued.secret)
            .await
            .unwrap()
            .unwrap();
        assert!(!found.holder_active, "撤銷 Token 之後連結跟著失效");
    }
}
