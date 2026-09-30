//! Document management, the first consumer of RustFS object storage.
//!
//! `upload_document` sends the bytes to RustFS; the database keeps only the
//! object key, byte size, MIME type and the BLAKE3 computed at upload time.
//! `get_document` recomputes the hash on retrieval and returns an error
//! without the content if it does not match (see `laysi-store/src/storage.rs`).
//!
//! Upload happens in two steps: allocate the number and revision in the
//! database first (the object key is derived from them), then write
//! `document_version` only after the object lands in the bucket. The other
//! order would leave rows pointing at files that do not exist.
//!
//! Upload is an operation rather than a multipart endpoint so that AI clients
//! can store files too. The cost is base64 transport, hence the per-file limit
//! [`laysi_store::storage::MAX_OBJECT_BYTES`]; larger files need streaming
//! uploads, not a higher limit.
//!
//! The same limit holds on the way out: `get_document` returns content only up
//! to it, and anything larger goes through `get_download_link`, a one-time
//! link that `/v1/downloads/{secret}` serves after checking the hash.

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use base64::Engine;
use laysi_store::downloads::NewDownloadLink;
use laysi_store::storage::{self, MAX_OBJECT_BYTES, MAX_UPLOAD_BYTES};
use laysi_store::ReserveDocument;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListDocuments),
        Box::new(UploadDocument),
        Box::new(GetDocument),
        Box::new(GetDownloadLink),
    ]
}

/// How long a download link stays usable. Enough for a screen to follow it
/// at once or for a person to open one pasted into a chat.
pub const DOWNLOAD_LINK_MINUTES: i64 = 5;

/// Where a download link points. Also mounted by the server under this path.
pub const DOWNLOAD_ENDPOINT: &str = "/v1/downloads/";

/// Bytes in a gigabyte. A unit conversion, not a setting.
const BYTES_PER_GB: f64 = 1024.0 * 1024.0 * 1024.0;
const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// Human-readable file size.
pub fn human_size(bytes: i64) -> String {
    let b = bytes as f64;
    if b >= BYTES_PER_GB {
        format!("{:.1} GB", b / BYTES_PER_GB)
    } else if b >= BYTES_PER_MB {
        format!("{:.1} MB", b / BYTES_PER_MB)
    } else {
        format!("{:.0} KB", (b / 1024.0).ceil())
    }
}

/// Display label for a document kind. Presentation only.
pub(crate) fn kind_label(kind: &str) -> &'static str {
    match kind {
        "receipt" => "收據",
        "vendor_bill" => "供應商帳單",
        "expense_receipt" => "報支收據",
        "bank" => "銀行文件",
        "contract" => "合約",
        "closing" => "結帳文件",
        "id_document" => "證件",
        "personnel" => "人事文件",
        _ => "其他",
    }
}

/// Short form of a hash, e.g. `a91f…c4e2`. The full value is returned
/// alongside; the short form is only for reading.
pub fn short_checksum(sum: &str) -> String {
    if sum.len() < 12 {
        return sum.to_string();
    }
    format!("{}…{}", &sum[..4], &sum[sum.len() - 4..])
}

pub(crate) fn document_json(d: &laysi_store::DocumentRow) -> Value {
    json!({
        "doc_no": d.doc_no,
        "name": d.name,
        "kind": d.kind,
        "kind_label": kind_label(&d.kind),
        "link_type": d.link_type,
        "link_ref": d.link_ref,
        "archived": d.archived,
        "note": d.note,
        "version_no": d.version_no,
        "version_label": format!("v{}", d.version_no),
        "versions": d.versions,
        "file_name": d.file_name,
        "object_key": d.object_key,
        "byte_size": d.byte_size,
        "size_label": human_size(d.byte_size),
        "content_type": d.content_type,
        "checksum": d.checksum,
        "checksum_short": short_checksum(&d.checksum),
        "uploaded_at": d.uploaded_at,
        "uploaded_by": d.uploaded_by,
        "created_at": d.created_at,
        "created_by": d.created_by,
    })
}

// Listing

struct ListDocuments;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListDocumentsInput {
    /// Search file name, document number or linked record number. File
    /// contents are not searched.
    #[serde(default)]
    pub q: Option<String>,
    /// Maximum rows, default 50.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[async_trait]
impl Operation for ListDocuments {
    fn name(&self) -> &'static str {
        "list_documents"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀文件清單：檔名、關聯單據、版次、內容雜湊，以及物件儲存的實際用量。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListDocumentsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListDocumentsInput = parse(input)?;
        let docs = ctx
            .store
            .documents(i.q.as_deref(), i.limit.unwrap_or(50))
            .await?;
        let stats = ctx.store.document_stats().await?;

        // The bucket side is measured independently. If the database says 3 MB
        // and the bucket holds 5 MB, it shows up here, the same approach as
        // reconciling AR detail to the ledger.
        let (bucket, storage_error) = match ctx.objects.as_ref() {
            None => (None, Some("尚未接上物件儲存".to_string())),
            Some(o) => match o.usage().await {
                Ok(u) => (
                    Some(json!({
                        "endpoint": o.endpoint(),
                        "bucket": o.bucket(),
                        "objects": u.objects,
                        "bytes": u.bytes,
                        "size_label": human_size(u.bytes),
                        "staged_objects": u.staged_objects,
                        "staged_bytes": u.staged_bytes,
                    })),
                    None,
                ),
                Err(e) => (None, Some(e.to_string())),
            },
        };

        let bucket_bytes = bucket.as_ref().and_then(|b| b["bytes"].as_i64());
        let gap = bucket_bytes.map(|b| b - stats.bytes);

        let summary = format!(
            "讀取文件 · {} 份／{} 個版本／{}",
            stats.documents,
            stats.versions,
            human_size(stats.bytes)
        );
        Ok(OpOutcome::new(
            json!({
                "documents": docs.iter().map(document_json).collect::<Vec<_>>(),
                "stats": {
                    "documents": stats.documents,
                    "versions": stats.versions,
                    "bytes": stats.bytes,
                    "size_label": human_size(stats.bytes),
                    "archived": stats.archived,
                    "vouchers_without_doc": stats.vouchers_without_doc,
                },
                "bucket": bucket,
                "bucket_difference": gap,
                "bucket_reconciled": gap.map(|g| g == 0),
                "storage_error": storage_error,
                // Must be shown on screen, not left in the code.
                "immutability_note": "稽核封存的文件無法刪除。",
                // The screen uploads through a link, so its ceiling is the
                // link's. The base64 input keeps its own, lower one.
                "max_object_bytes": MAX_UPLOAD_BYTES,
                "max_inline_bytes": MAX_OBJECT_BYTES,
            }),
            summary,
        ))
    }
}

// Upload

struct UploadDocument;

#[derive(Deserialize, JsonSchema)]
pub struct UploadDocumentInput {
    /// File name. Non-ASCII names are kept; the object key is transliterated.
    /// Required with `content_base64`; with `upload_id` it defaults to the
    /// name the file was uploaded under.
    #[serde(default)]
    pub file_name: Option<String>,
    /// File contents, base64, up to 25 MB. Give this or `upload_id`, not
    /// both.
    #[serde(default)]
    pub content_base64: Option<String>,
    /// A file received through `request_upload_link`, as listed by
    /// `list_uploads`. Give this or `content_base64`, not both.
    #[serde(default)]
    pub upload_id: Option<uuid::Uuid>,
    /// Display name. Defaults to the file name.
    #[serde(default)]
    pub name: Option<String>,
    /// One of `receipt`, `vendor_bill`, `expense_receipt`, `bank`,
    /// `contract`, `closing`, `other`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Kind of linked record: `voucher`, `ap_bill`, `ar_invoice`,
    /// `expense_claim`, `bank_account`, `import_batch`, `period` or `other`.
    #[serde(default)]
    pub link_type: Option<String>,
    /// Linked record number, e.g. `V2607-018`.
    #[serde(default)]
    pub link_ref: Option<String>,
    /// When set, adds a revision to an existing document instead of creating
    /// a new one.
    #[serde(default)]
    pub doc_no: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Permitted document kinds. Kept in step with the CHECK in migrations/0001.
const KINDS: [&str; 7] = [
    "receipt",
    "vendor_bill",
    "expense_receipt",
    "bank",
    "contract",
    "closing",
    "other",
];
const LINK_TYPES: [&str; 9] = [
    "",
    "voucher",
    "ap_bill",
    "ar_invoice",
    "expense_claim",
    "bank_account",
    "import_batch",
    "period",
    "other",
];

/// Personnel files are linked to an employee and reached only through the
/// employee-scoped operations, so this list deliberately leaves `employee`
/// out: `upload_document` belongs to a permission that also opens every
/// finance document.
pub(crate) const EMPLOYEE_LINK_TYPE: &str = "employee";
/// Kinds an employee attachment may carry.
pub(crate) const EMPLOYEE_KINDS: [&str; 3] = ["id_document", "contract", "personnel"];

#[async_trait]
impl Operation for UploadDocument {
    fn name(&self) -> &'static str {
        "upload_document"
    }
    fn risk(&self) -> Risk {
        // Writes to object storage, and what is written cannot be deleted. Not
        // read-only, though it touches no amounts.
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "存一份文件到物件儲存，並掛到一張單據上。檔案可帶入上傳連結收到的上傳編號，或以 base64 直接傳入（25 MB 以內）。同一個編號再存一次會成為新版本。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UploadDocumentInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UploadDocumentInput = parse(input)?;
        let objects = ctx.objects()?;

        // Where the bytes come from: the input itself, or a file an upload
        // link already put in the bucket.
        let source = match (i.content_base64.as_deref(), i.upload_id) {
            (Some(_), Some(_)) => {
                return Err(OpError::BadInput("檔案內容與上傳編號請擇一提供。".into()))
            }
            (None, None) => {
                return Err(OpError::BadInput(
                    "請帶入上傳編號，或以 base64 提供檔案內容。".into(),
                ))
            }
            (Some(content), None) => {
                let file_name = i
                    .file_name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .ok_or_else(|| OpError::BadInput("缺少必填欄位「file_name」".into()))?;
                let body = base64::engine::general_purpose::STANDARD
                    .decode(content.trim())
                    .map_err(|e| OpError::BadInput(format!("檔案內容不是合法的 base64：{e}")))?;
                if body.is_empty() {
                    return Err(OpError::BadInput("檔案是空的".into()));
                }
                if body.len() > MAX_OBJECT_BYTES {
                    return Err(OpError::BadInput(format!(
                        "檔案 {} 位元組，超過單檔上限 {}。更大的檔案請改用上傳連結。",
                        body.len(),
                        MAX_OBJECT_BYTES
                    )));
                }
                Source::Inline { file_name, body }
            }
            (None, Some(upload_id)) => Source::Staged(upload_id),
        };

        let kind = i.kind.as_deref().unwrap_or("other");
        if !KINDS.contains(&kind) {
            return Err(OpError::BadInput(format!(
                "未支援「{kind}」文件類型，可用類型：{}",
                KINDS.join("、")
            )));
        }
        let link_type = i.link_type.as_deref().unwrap_or("");
        if !LINK_TYPES.contains(&link_type) {
            return Err(OpError::BadInput(format!(
                "未支援「{link_type}」關聯單據種類"
            )));
        }
        let link_ref = i.link_ref.as_deref().unwrap_or("");
        if link_type.is_empty() != link_ref.is_empty() {
            return Err(OpError::BadInput(
                "關聯單據的種類與單號請一起填，或一起留空".into(),
            ));
        }
        // Linking to a voucher requires that voucher to exist. A dangling
        // reference leaves an audit trail that cannot be followed.
        if link_type == "voucher" {
            ctx.store.voucher_by_no(link_ref).await?;
        }

        let filing = Filing {
            doc_no: i.doc_no.as_deref(),
            name: i.name.as_deref(),
            kind,
            link_type,
            link_ref,
            note: i.note.as_deref().unwrap_or(""),
        };
        let doc = match source {
            Source::Inline { file_name, body } => {
                file_inline(ctx, objects, &filing, &file_name, &body).await?
            }
            Source::Staged(upload_id) => {
                file_staged(ctx, objects, &filing, i.file_name.as_deref(), upload_id).await?
            }
        };

        let summary = format!(
            "上傳 {} · {} v{} {} · 指紋 {}{}",
            doc.file_name,
            doc.doc_no,
            doc.version_no,
            human_size(doc.byte_size),
            short_checksum(&doc.checksum),
            if link_ref.is_empty() {
                String::new()
            } else {
                format!(" · 掛在 {link_ref}")
            }
        );
        Ok(OpOutcome::new(
            json!({
                "document": document_json(&doc),
                "bucket": objects.bucket(),
                "new_version": doc.version_no > 1,
            }),
            summary,
        ))
    }
}

/// Where a document's bytes come from.
enum Source {
    /// Carried in the input as base64.
    Inline { file_name: String, body: Vec<u8> },
    /// Already in the bucket, received through an upload link.
    Staged(uuid::Uuid),
}

/// Everything about a document except its bytes.
struct Filing<'a> {
    doc_no: Option<&'a str>,
    name: Option<&'a str>,
    kind: &'a str,
    link_type: &'a str,
    link_ref: &'a str,
    note: &'a str,
}

/// Allocate the number and revision; the object key needs them.
async fn reserve(
    ctx: &OpCtx,
    filing: &Filing<'_>,
    file_name: &str,
) -> Result<laysi_store::DocumentSlot, OpError> {
    Ok(ctx
        .store
        .reserve_document_slot(ReserveDocument {
            doc_no: filing.doc_no,
            name: filing.name.unwrap_or(file_name),
            kind: filing.kind,
            link_type: filing.link_type,
            link_ref: filing.link_ref,
            note: filing.note,
            today: ctx.today,
            actor: &ctx.actor,
        })
        .await?)
}

fn document_key(ctx: &OpCtx, slot: &laysi_store::DocumentSlot, file_name: &str) -> String {
    storage::object_key(
        &ctx.today.format("%y%m").to_string(),
        &slot.doc_no,
        slot.version_no,
        file_name,
    )
}

/// Register an object that is already in the bucket. If that fails the
/// object has to go: the number is spent either way, but a file nothing
/// points at is invisible and counts against storage forever.
async fn record(
    ctx: &OpCtx,
    objects: &laysi_store::ObjectStore,
    slot: &laysi_store::DocumentSlot,
    stored: &storage::StoredObject,
    file_name: &str,
) -> Result<laysi_store::DocumentRow, OpError> {
    match ctx
        .store
        .record_document_version(slot, stored, file_name, &ctx.actor)
        .await
    {
        Ok(doc) => Ok(doc),
        Err(e) => {
            objects.discard(&stored.key).await;
            Err(e.into())
        }
    }
}

/// File bytes carried in the input. The object goes into the bucket before
/// any row points at it, so a failure stops with nothing to point at a file
/// that does not exist.
async fn file_inline(
    ctx: &OpCtx,
    objects: &laysi_store::ObjectStore,
    filing: &Filing<'_>,
    file_name: &str,
    body: &[u8],
) -> Result<laysi_store::DocumentRow, OpError> {
    let slot = reserve(ctx, filing, file_name).await?;
    let key = document_key(ctx, &slot, file_name);
    let content_type = storage::guess_content_type(file_name);
    let stored = objects.put(&key, body, content_type, file_name).await?;
    record(ctx, objects, &slot, &stored, file_name).await
}

/// File a staged upload: copy it to its document key inside the bucket,
/// register it, then drop the staged copy.
///
/// The staged file is taken before anything else, so two calls with the same
/// id cannot both file it, and put back if the filing fails, so a failure
/// leaves it where it was for another try. Its type and hash are the ones
/// recorded when it arrived: the hash was taken over the bytes as they came
/// in, which is what retrieval will hold the document to.
async fn file_staged(
    ctx: &OpCtx,
    objects: &laysi_store::ObjectStore,
    filing: &Filing<'_>,
    rename: Option<&str>,
    upload_id: uuid::Uuid,
) -> Result<laysi_store::DocumentRow, OpError> {
    let holder = super::uploads::holder(ctx)?;
    let Some(staged) = ctx.store.take_staged_upload(upload_id, holder).await? else {
        return Err(OpError::BadInput(
            match ctx.store.staged_upload_doc_no(upload_id, holder).await? {
                Some(Some(doc_no)) => format!("這個檔案已經存成文件 {doc_no}。"),
                Some(None) => "這個檔案正在存成文件，請稍後查看文件清單。".into(),
                None => "查無此上傳編號，或檔案已在收到一天後刪除。請重新上傳。".into(),
            },
        ));
    };
    let file_name = rename
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(&staged.file_name)
        .to_string();

    let filed = async {
        let slot = reserve(ctx, filing, &file_name).await?;
        let key = document_key(ctx, &slot, &file_name);
        objects
            .copy_to(
                &staged.object_key,
                &key,
                &staged.content_type,
                &staged.checksum,
            )
            .await?;
        let stored = storage::StoredObject {
            key,
            byte_size: staged.byte_size,
            content_type: staged.content_type.clone(),
            checksum: staged.checksum.clone(),
        };
        record(ctx, objects, &slot, &stored, &file_name).await
    }
    .await;

    match filed {
        Ok(doc) => {
            // A failure here leaves the staged row taken without a document
            // number, which the sweep treats as filed; its object is then an
            // orphan the sweep removes. Neither is worth failing a filing
            // that has already happened.
            if let Err(error) = ctx
                .store
                .mark_staged_upload_filed(upload_id, &doc.doc_no)
                .await
            {
                tracing::warn!(%error, %upload_id, "could not record which document a staged upload became");
            }
            objects.discard(&staged.object_key).await;
            Ok(doc)
        }
        Err(e) => {
            if let Err(error) = ctx.store.return_staged_upload(upload_id).await {
                tracing::warn!(%error, %upload_id, "could not put a staged upload back after a failed filing");
            }
            Err(e)
        }
    }
}

// Retrieval

struct GetDocument;

#[derive(Deserialize, JsonSchema)]
pub struct GetDocumentInput {
    /// Document number, e.g. `D2607-001`.
    pub doc_no: String,
    /// Revision. Defaults to the latest.
    #[serde(default)]
    pub version: Option<i32>,
    /// Whether to return the content as base64, for documents up to 25 MB.
    /// Off by default, which only verifies the hash. Larger documents are
    /// downloaded through `get_download_link`.
    #[serde(default)]
    pub include_content: bool,
}

#[async_trait]
impl Operation for GetDocument {
    fn name(&self) -> &'static str {
        "get_document"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "從物件儲存取回一份文件並重算雜湊與登記值比對。預設只回驗證結果與後設資料；要求回傳內容時才以 base64 附上內容，限 25 MB 以內，更大的文件請取得下載連結。雜湊不符會回報錯誤，不回傳內容。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<GetDocumentInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: GetDocumentInput = parse(input)?;
        let objects = ctx.objects()?;
        let doc = ctx.store.document_version(&i.doc_no, i.version).await?;

        if i.include_content && doc.byte_size > MAX_OBJECT_BYTES as i64 {
            return Err(OpError::BadInput(format!(
                "{} v{} 有 {}，超過直接回傳內容的上限 {}，請改用下載連結。",
                doc.doc_no,
                doc.version_no,
                human_size(doc.byte_size),
                human_size(MAX_OBJECT_BYTES as i64)
            )));
        }

        // Always compared on retrieval. A mismatch is an error: a tampered
        // record is more dangerous than a missing one because it looks
        // genuine. Without content the object is hashed as it streams past,
        // so checking a 90 MB document does not hold 90 MB.
        let (content, size) = if i.include_content {
            let body = objects.get(&doc.object_key, &doc.checksum).await?;
            let size = body.len() as u64;
            (
                Some(base64::engine::general_purpose::STANDARD.encode(&body)),
                size,
            )
        } else {
            (
                None,
                objects
                    .verify(
                        &doc.object_key,
                        &doc.checksum,
                        u64::try_from(doc.byte_size).unwrap_or(0),
                    )
                    .await?,
            )
        };

        let summary = format!(
            "取回 {} v{} {} · 雜湊比對相符（{}）",
            doc.doc_no,
            doc.version_no,
            doc.file_name,
            short_checksum(&doc.checksum)
        );
        Ok(OpOutcome::new(
            json!({
                "document": document_json(&doc),
                // Both reads fail rather than return on a mismatch, so reaching
                // here means the recomputed hash is the registered one.
                "verified": true,
                "checksum_recomputed": doc.checksum,
                "byte_size_actual": size,
                "content_base64": content,
                "bucket": objects.bucket(),
            }),
            summary,
        ))
    }
}

// Download links

struct GetDownloadLink;

#[derive(Deserialize, JsonSchema)]
pub struct GetDownloadLinkInput {
    /// Document number, e.g. `D2607-001`.
    pub doc_no: String,
    /// Revision. Defaults to the latest.
    #[serde(default)]
    pub version: Option<i32>,
}

#[async_trait]
impl Operation for GetDownloadLink {
    fn name(&self) -> &'static str {
        "get_download_link"
    }
    fn risk(&self) -> Risk {
        // Writes a row, the link, and no business record: an installation
        // whose licence has lapsed still hands its documents out.
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "取得一份文件的下載連結：5 分鐘內可使用一次，伺服器比對內容雜湊相符後才送出檔案，不限檔案大小。連結交給使用者以瀏覽器開啟，或在程式執行環境裡執行回傳的 curl 指令。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<GetDownloadLinkInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: GetDownloadLinkInput = parse(input)?;
        let holder = ctx
            .link_holder()
            .ok_or_else(|| OpError::BadInput("下載連結限登入的帳號或 MCP 連線取得。".into()))?;
        // Refused here rather than when the link is followed: a link that can
        // only fail is worse than no link.
        ctx.objects()?;

        let doc_no = i.doc_no.trim();
        let doc = ctx.store.document_version(doc_no, i.version).await?;
        // Personnel files are reached only through the employee-scoped
        // operations, and answered as if they did not exist, so the number
        // alone confirms nothing.
        if doc.link_type == EMPLOYEE_LINK_TYPE {
            return Err(laysi_store::StoreError::DocumentNotFound {
                no: doc_no.to_string(),
            }
            .into());
        }

        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(DOWNLOAD_LINK_MINUTES);
        let issued = ctx
            .store
            .create_download_link(NewDownloadLink {
                holder,
                actor: &ctx.actor,
                actor_role: &ctx.actor_role,
                channel: ctx.channel.as_db(),
                doc_no: &doc.doc_no,
                version_no: doc.version_no,
                expires_at,
            })
            .await?
            .ok_or_else(|| laysi_store::StoreError::DocumentVersionNotFound {
                no: doc.doc_no.clone(),
                version: doc.version_no,
            })?;

        let base = ctx.public_base.as_deref().unwrap_or("");
        let path = format!("{DOWNLOAD_ENDPOINT}{}", issued.secret);
        let url = format!("{base}{path}");
        Ok(OpOutcome::new(
            json!({
                "document": document_json(&doc),
                "download_url": url,
                "download_path": path,
                "curl": format!("curl --fail-with-body -o '<存檔路徑>' '{url}'"),
                "expires_at": issued.expires_at,
                "note": "連結只能使用一次；用過或過期之後請重新取得。",
            }),
            format!(
                "取得下載連結 {} v{} {} · {}",
                doc.doc_no,
                doc.version_no,
                doc.file_name,
                human_size(doc.byte_size)
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lapsed licence refuses what writes, and the documents are the
    /// customer's: the link has to stay reachable while nothing else can be
    /// written.
    #[test]
    fn a_download_link_is_not_a_write() {
        assert!(!GetDownloadLink.writes());
        assert!(matches!(GetDownloadLink.risk(), Risk::ReadOnly));
    }

    /// The checksum column reads as `a91f…c4e2`. The short form is for
    /// reading; the full value is returned as well.
    #[test]
    fn checksums_are_shortened_the_way_the_prototype_shows_them() {
        let full = "a91f0c3d55e7b8241f6a09cd7b3e5518d24c9f7061ba38ed4297cf50a63bc4e2";
        assert_eq!(short_checksum(full), "a91f…c4e2");
        assert_eq!(short_checksum("abc"), "abc", "太短就原樣顯示，不要切出亂碼");
    }

    #[test]
    fn sizes_read_like_sizes() {
        assert_eq!(human_size(0), "0 KB");
        assert_eq!(human_size(1024), "1 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(human_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn kinds_match_the_prototype_labels() {
        assert_eq!(kind_label("receipt"), "收據");
        assert_eq!(kind_label("vendor_bill"), "供應商帳單");
        assert_eq!(kind_label("bank"), "銀行文件");
        assert_eq!(kind_label("什麼都不是"), "其他");
    }
}
