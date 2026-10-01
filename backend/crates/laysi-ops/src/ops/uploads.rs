//! Upload links: files that travel outside the operation input.
//!
//! An operation input carries a file as base64, which a model has to write
//! out itself and a screen can send only up to the JSON body limit.
//! `request_upload_link` hands out a link instead, good for
//! [`LINK_MINUTES`] minutes and [`LINK_MAX_FILES`] files. The files are posted
//! to `/v1/uploads/{secret}` -- by the model's own code sandbox with curl, or by
//! a person in a browser -- and streamed into the bucket under `staging/`.
//! `list_uploads` says what arrived; `upload_document` files one of them by
//! its id.
//!
//! The link is a credential and is shown only once, in the answer to the call
//! that issued it; the audit trail records that a link was issued, not the
//! link. Who may see and file what arrived is the holder of the link: the
//! account on screen, or the MCP token, each seeing only its own.

use super::documents::human_size;
use crate::{parse, schema_of, Empty, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use laysi_store::storage::MAX_UPLOAD_BYTES;
use laysi_store::uploads::NewUploadBatch;
use laysi_store::LinkHolder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// How long a link accepts files. Long enough to find a file and pick it in
/// a browser, short enough that a link pasted somewhere is soon worthless.
pub const LINK_MINUTES: i64 = 10;

/// Files one link accepts.
pub const LINK_MAX_FILES: i32 = 20;

/// Where the files are posted. Also mounted by the server under this path.
pub const UPLOAD_ENDPOINT: &str = "/v1/uploads/";

/// The page a person opens to upload in a browser.
pub const UPLOAD_PAGE: &str = "/upload/";

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(RequestUploadLink), Box::new(ListUploads)]
}

/// The holder this call acts as, or the reason it has none.
pub(crate) fn holder(ctx: &OpCtx) -> Result<LinkHolder, OpError> {
    ctx.link_holder()
        .ok_or_else(|| OpError::BadInput("上傳連結限登入的帳號或 MCP 連線取得。".into()))
}

pub(crate) fn staged_json(s: &laysi_store::StagedUpload) -> Value {
    json!({
        "upload_id": s.id,
        "file_name": s.file_name,
        "byte_size": s.byte_size,
        "size_label": human_size(s.byte_size),
        "content_type": s.content_type,
        "checksum": s.checksum,
        "received_at": s.created_at,
        "filed_as": s.consumed_doc_no,
    })
}

// Issuing

struct RequestUploadLink;

#[async_trait]
impl Operation for RequestUploadLink {
    fn name(&self) -> &'static str {
        "request_upload_link"
    }
    fn risk(&self) -> Risk {
        // Issues a credential and takes bytes into the bucket, though nothing
        // becomes a document until `upload_document` files it.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "取得上傳連結：10 分鐘內可上傳至多 20 個檔案，每檔 90 MB 以內。檔案在程式執行環境裡就用回傳的 curl 指令上傳，否則把網頁連結交給使用者以瀏覽器開啟；上傳後查看這條連結收到的檔案，取得各檔的上傳編號，再存成文件。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<Empty>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let _: Empty = parse(input)?;
        let holder = holder(ctx)?;
        // Refused here rather than when the first file arrives: a link that
        // can only fail is worse than no link.
        ctx.objects()?;

        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(LINK_MINUTES);
        let issued = ctx
            .store
            .create_upload_batch(NewUploadBatch {
                holder,
                actor: &ctx.actor,
                actor_role: &ctx.actor_role,
                channel: ctx.channel.as_db(),
                max_files: LINK_MAX_FILES,
                expires_at,
            })
            .await?;

        let base = ctx.public_base.as_deref().unwrap_or("");
        let endpoint = format!("{base}{UPLOAD_ENDPOINT}{}", issued.secret);
        let page = format!("{base}{UPLOAD_PAGE}{}", issued.secret);
        Ok(OpOutcome::new(
            json!({
                "batch_id": issued.batch.id,
                "upload_page": page,
                "upload_endpoint": endpoint,
                "upload_path": format!("{UPLOAD_ENDPOINT}{}", issued.secret),
                "curl": format!("curl --fail-with-body -F 'file=@<檔案路徑>' '{endpoint}'"),
                "expires_at": issued.batch.expires_at,
                "max_files": issued.batch.max_files,
                "max_file_bytes": MAX_UPLOAD_BYTES,
                "note": "每次請求上傳一個檔案；多個檔案就執行多次。",
            }),
            format!("取得上傳連結 · {LINK_MINUTES} 分鐘內至多 {LINK_MAX_FILES} 個檔案"),
        ))
    }
}

// Listing

struct ListUploads;

#[derive(Deserialize, JsonSchema)]
pub struct ListUploadsInput {
    /// `batch_id` returned by `request_upload_link`.
    pub batch_id: Uuid,
}

#[async_trait]
impl Operation for ListUploads {
    fn name(&self) -> &'static str {
        "list_uploads"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "列出一條上傳連結已收到的檔案：檔名、大小、內容雜湊與上傳編號，以及還能再收幾個、何時失效。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListUploadsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListUploadsInput = parse(input)?;
        let holder = holder(ctx)?;
        let batch = ctx
            .store
            .upload_batch_for(i.batch_id, holder)
            .await?
            .ok_or_else(|| {
                OpError::BadInput(
                    "查無此上傳連結，或它已在失效一天後刪除。請重新取得上傳連結。".into(),
                )
            })?;
        let files = ctx.store.staged_uploads(batch.id).await?;
        let expired = batch.is_expired(chrono::Utc::now());
        let summary = format!("查看上傳連結 · 已收到 {} 個檔案", files.len());
        Ok(OpOutcome::new(
            json!({
                "batch": {
                    "batch_id": batch.id,
                    "expires_at": batch.expires_at,
                    "expired": expired,
                    "max_files": batch.max_files,
                    "remaining": if expired { 0 } else { batch.max_files - batch.used_files },
                },
                "files": files.iter().map(staged_json).collect::<Vec<_>>(),
            }),
            summary,
        ))
    }
}
