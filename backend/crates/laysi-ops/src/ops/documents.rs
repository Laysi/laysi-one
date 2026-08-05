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
//! There is no OCR engine. The CHECK on `document_version.ocr_status` accepts
//! only `pending` and `not_applicable` (see migrations/0007).

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use base64::Engine;
use laysi_store::storage::{self, MAX_OBJECT_BYTES};
use laysi_store::ReserveDocument;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(ListDocuments), Box::new(UploadDocument), Box::new(GetDocument)]
}

/// Bytes in a gigabyte. A unit conversion, not a setting.
const BYTES_PER_GB: f64 = 1024.0 * 1024.0 * 1024.0;
const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// Human-readable file size.
fn human_size(bytes: i64) -> String {
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
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "receipt" => "收據",
        "vendor_bill" => "供應商帳單",
        "expense_receipt" => "報支收據",
        "bank" => "銀行文件",
        "contract" => "合約",
        "closing" => "結帳文件",
        _ => "其他",
    }
}

/// Display label for an OCR status. There is no "recognised" value; the
/// database cannot store one.
fn ocr_label(status: &str) -> &'static str {
    match status {
        "not_applicable" => "不適用",
        _ => "待處理",
    }
}

/// Short form of a hash, e.g. `a91f…c4e2`. The full value is returned
/// alongside; the short form is only for reading.
fn short_checksum(sum: &str) -> String {
    if sum.len() < 12 {
        return sum.to_string();
    }
    format!("{}…{}", &sum[..4], &sum[sum.len() - 4..])
}

fn document_json(d: &laysi_store::DocumentRow) -> Value {
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
        "ocr_status": d.ocr_status,
        "ocr_label": ocr_label(&d.ocr_status),
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
    /// Search file name, document number or linked record number.
    ///
    /// Full text is not searched: without OCR there is no text to search.
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
        "讀文件清單：檔名、關聯單據、版次、內容雜湊，以及 RustFS 的實際用量。OCR 一律回 pending；系統未配置辨識引擎，不提供推估的辨識率。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ListDocumentsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListDocumentsInput = parse(input)?;
        let docs = ctx.store.documents(i.q.as_deref(), i.limit.unwrap_or(50)).await?;
        let stats = ctx.store.document_stats().await?;

        // The bucket side is measured independently. If the database says 3 MB
        // and the bucket holds 5 MB, it shows up here, the same approach as
        // reconciling AR detail to the ledger.
        let (bucket, storage_error) = match ctx.objects.as_ref() {
            None => (None, Some("還沒接上物件儲存（RUSTFS_* 環境變數沒設齊）".to_string())),
            Some(o) => match o.usage().await {
                Ok(u) => (
                    Some(json!({
                        "endpoint": o.endpoint(),
                        "bucket": o.bucket(),
                        "objects": u.objects,
                        "bytes": u.bytes,
                        "size_label": human_size(u.bytes),
                    })),
                    None,
                ),
                Err(e) => (None, Some(e.to_string())),
            },
        };

        let bucket_bytes = bucket.as_ref().and_then(|b| b["bytes"].as_i64());
        let gap = bucket_bytes.map(|b| b - stats.bytes);

        let summary = format!(
            "讀取文件 · {} 份／{} 個版本／{} · 未 OCR {} 份（系統未配置辨識引擎）",
            stats.documents,
            stats.versions,
            human_size(stats.bytes),
            stats.ocr_pending
        );
        Ok(OpOutcome::new(
            json!({
                "documents": docs.iter().map(document_json).collect::<Vec<_>>(),
                "stats": {
                    "documents": stats.documents,
                    "versions": stats.versions,
                    "bytes": stats.bytes,
                    "size_label": human_size(stats.bytes),
                    "ocr_pending": stats.ocr_pending,
                    "archived": stats.archived,
                    "vouchers_without_doc": stats.vouchers_without_doc,
                },
                "bucket": bucket,
                "bucket_difference": gap,
                "bucket_reconciled": gap.map(|g| g == 0),
                "storage_error": storage_error,
                // Both of these must be shown on screen, not left in the code.
                "ocr_note": "尚未接上 OCR 引擎，所有文件的辨識狀態都是「待處理」。全文搜尋只找得到檔名與關聯單據號。",
                "immutability_note": "上傳後檔案不可覆寫，改版會存成新版本；稽核封存的文件連老闆也刪不掉（由資料庫的 trigger 擋）。",
                "max_object_bytes": MAX_OBJECT_BYTES,
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
    pub file_name: String,
    /// File contents, base64.
    pub content_base64: String,
    /// Display name. Defaults to the file name.
    #[serde(default)]
    pub name: Option<String>,
    /// One of `receipt`, `vendor_bill`, `expense_receipt`, `bank`,
    /// `contract`, `closing`, `other`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Kind of linked record: `voucher`, `ap_bill`, `ar_invoice`,
    /// `expense_claim`, `bank_account`, `einvoice`, `import_batch`, `period`
    /// or `other`.
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

/// Permitted document kinds. Kept in step with the CHECK in migrations/0007.
const KINDS: [&str; 7] =
    ["receipt", "vendor_bill", "expense_receipt", "bank", "contract", "closing", "other"];
const LINK_TYPES: [&str; 10] = [
    "",
    "voucher",
    "ap_bill",
    "ar_invoice",
    "expense_claim",
    "bank_account",
    "einvoice",
    "import_batch",
    "period",
    "other",
];

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
        "上傳一份檔案到 RustFS 物件儲存，並掛到一張單據上。內容以 base64 傳入；系統存下 BLAKE3 雜湊，之後每次取回都會比對。同一個編號再上傳一次就是新版本，舊版不會被覆寫。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UploadDocumentInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UploadDocumentInput = parse(input)?;
        let objects = ctx.objects()?;

        let body = base64::engine::general_purpose::STANDARD
            .decode(i.content_base64.trim())
            .map_err(|e| OpError::BadInput(format!("content_base64 不是合法的 base64：{e}")))?;
        if body.is_empty() {
            return Err(OpError::BadInput("檔案是空的".into()));
        }
        if body.len() > MAX_OBJECT_BYTES {
            return Err(OpError::BadInput(format!(
                "檔案 {} 位元組，超過單檔上限 {}。內容走 JSON 的 base64，要更大的檔案得先改成串流上傳。",
                body.len(),
                MAX_OBJECT_BYTES
            )));
        }

        let kind = i.kind.as_deref().unwrap_or("other");
        if !KINDS.contains(&kind) {
            return Err(OpError::BadInput(format!(
                "未支援「{kind}」文件類型，可用類型：{}",
                KINDS.join("、")
            )));
        }
        let link_type = i.link_type.as_deref().unwrap_or("");
        if !LINK_TYPES.contains(&link_type) {
            return Err(OpError::BadInput(format!("未支援「{link_type}」關聯單據種類")));
        }
        let link_ref = i.link_ref.as_deref().unwrap_or("");
        if link_type.is_empty() != link_ref.is_empty() {
            return Err(OpError::BadInput(
                "關聯單據的種類與單號要嘛都填，要嘛都不填，僅提供其一將無法回查".into(),
            ));
        }
        // Linking to a voucher requires that voucher to exist. A dangling
        // reference leaves an audit trail that cannot be followed.
        if link_type == "voucher" {
            ctx.store.voucher_by_no(link_ref).await?;
        }

        // 1. Allocate the number and revision; the object key needs them.
        let slot = ctx
            .store
            .reserve_document_slot(ReserveDocument {
                doc_no: i.doc_no.as_deref(),
                name: i.name.as_deref().unwrap_or(&i.file_name),
                kind,
                link_type,
                link_ref,
                note: i.note.as_deref().unwrap_or(""),
                today: ctx.today,
                actor: &ctx.actor,
            })
            .await?;

        // 2. Put the object in the bucket. On failure stop here, so no database
        //    row points at a file that does not exist.
        let key = storage::object_key(
            &ctx.today.format("%y%m").to_string(),
            &slot.doc_no,
            slot.version_no,
            &i.file_name,
        );
        let content_type = storage::guess_content_type(&i.file_name);
        let stored = objects.put(&key, &body, content_type, &i.file_name).await?;

        // 3. Register it. Formats that never need OCR (CSV, XML) are marked
        //    not applicable, everything else pending: there is no engine, and
        //    the database would reject any other value anyway.
        let ocr_status = if content_type.starts_with("application/pdf")
            || content_type.starts_with("image/")
        {
            "pending"
        } else {
            "not_applicable"
        };
        let doc = ctx
            .store
            .record_document_version(&slot, &stored, &i.file_name, ocr_status, &ctx.actor)
            .await?;

        let summary = format!(
            "上傳 {} · {} v{} {} · BLAKE3 {}{}",
            doc.file_name,
            doc.doc_no,
            doc.version_no,
            human_size(doc.byte_size),
            short_checksum(&doc.checksum),
            if link_ref.is_empty() { String::new() } else { format!(" · 掛在 {link_ref}") }
        );
        Ok(OpOutcome::new(
            json!({
                "document": document_json(&doc),
                "bucket": objects.bucket(),
                "new_version": doc.version_no > 1,
                "ocr_note": "系統未配置 OCR 引擎，本文件的辨識狀態為「待處理」。",
            }),
            summary,
        ))
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
    /// Whether to return the content as base64. Off by default, which only
    /// verifies the hash.
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
        "從 RustFS 取回一份文件並重算 BLAKE3 與登記值比對。預設只回驗證結果與後設資料；include_content 為 true 才把內容以 base64 帶回來。雜湊對不上就回錯誤，不交出內容。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<GetDocumentInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: GetDocumentInput = parse(input)?;
        let objects = ctx.objects()?;
        let doc = ctx.store.document_version(&i.doc_no, i.version).await?;

        // Always compared on retrieval. A mismatch is an error: a tampered
        // record is more dangerous than a missing one because it looks
        // genuine.
        let body = objects.get(&doc.object_key, &doc.checksum).await?;
        let recomputed = storage::checksum(&body);

        let content = i
            .include_content
            .then(|| base64::engine::general_purpose::STANDARD.encode(&body));

        let summary = format!(
            "取回 {} v{} {} · 雜湊比對相符（{}）",
            doc.doc_no,
            doc.version_no,
            doc.file_name,
            short_checksum(&recomputed)
        );
        Ok(OpOutcome::new(
            json!({
                "document": document_json(&doc),
                "verified": recomputed == doc.checksum,
                "checksum_recomputed": recomputed,
                "byte_size_actual": body.len(),
                "content_base64": content,
                "bucket": objects.bucket(),
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checksum column reads as `a91f…c4e2`. The short form is for
    /// reading; the full value is returned as well.
    #[test]
    fn checksums_are_shortened_the_way_the_prototype_shows_them() {
        let full = "a91f0c3d55e7b8241f6a09cd7b3e5518d24c9f7061ba38ed4297cf50a63bc4e2";
        assert_eq!(short_checksum(full), "a91f…c4e2");
        assert_eq!(short_checksum("abc"), "abc", "太短就原樣顯示，不要切出亂碼");
    }

    /// There is no "recognised" status, and the database CHECK cannot store
    /// one either.
    #[test]
    fn there_is_no_such_thing_as_recognised() {
        assert_eq!(ocr_label("pending"), "待處理");
        assert_eq!(ocr_label("not_applicable"), "不適用");
        // Even a value forced in does not become "recognised".
        assert_eq!(ocr_label("done"), "待處理");
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
