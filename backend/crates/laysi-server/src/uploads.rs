//! The receiving end of an upload link: `/v1/uploads/{secret}`.
//!
//! The second exception to "do not hand-write axum routes", after
//! `/v1/auth/*`, and for a similar reason: what arrives here is a file rather
//! than an operation input, and the point of the link is that the bytes do
//! not travel inside one. Issuing the link is an operation
//! (`request_upload_link`), so the tool switch, the role and the audit all
//! applied there; filing what arrives is an operation too
//! (`upload_document`). This route only moves bytes into the staging area of
//! the bucket, and the link is its whole authorisation.
//!
//! The link is checked before a byte of the body is read -- that it exists,
//! has not expired, still has a place, and that its holder still qualifies --
//! so a dead link costs a lookup rather than a 90 MB transfer. Every file,
//! received or refused, is written to the audit trail under the holder.

use crate::Shared;
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use laysi_ops::ops::documents::{human_size, short_checksum};
use laysi_store::storage::{self, MAX_UPLOAD_BYTES};
use laysi_store::uploads::{staging_key, NewStagedUpload, STAGING_PREFIX};
use laysi_store::{ObjectStore, Store, UploadBatch};
use serde_json::{json, Value};
use uuid::Uuid;

/// Largest body the route accepts: one file at the link's ceiling plus the
/// multipart framing around it.
pub const UPLOAD_BODY_LIMIT: usize = MAX_UPLOAD_BYTES + 1024 * 1024;

// A file at the ceiling has to reach the per-file check, which says what the
// ceiling is, rather than stop at the body limit, which only says 413.
const _: () = assert!(UPLOAD_BODY_LIMIT > MAX_UPLOAD_BYTES);

/// Requests per source address per minute. A link takes twenty files, so a
/// person uploading a folder stays well inside it; guessing secrets does not.
pub const UPLOADS_PER_MINUTE: u32 = 60;

/// How long a staged file waits to be filed before it is removed.
pub const STAGED_KEEP_HOURS: i64 = 24;

/// Longest file name kept, in characters. The name is only displayed.
const MAX_FILE_NAME_CHARS: usize = 200;

/// Prefix of every secret `create_upload_batch` issues.
const SECRET_PREFIX: &str = "lysu_";

/// Whether `secret` has the shape of a link secret with `prefix`: the
/// prefix, then two simple UUIDs. Anything else is refused before it costs a
/// query.
pub(crate) fn plausible_secret(secret: &str, prefix: &str) -> bool {
    secret.len() == prefix.len() + 64
        && secret.starts_with(prefix)
        && secret[prefix.len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
}

/// The error shape of the link routes, the same envelope an operation answers
/// with.
pub(crate) fn fail(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "ok": false, "error": { "code": code, "message": message.into() } })),
    )
        .into_response()
}

fn gone() -> Response {
    fail(
        StatusCode::GONE,
        "link_expired",
        "上傳連結已失效，請回到原本的畫面或對話重新取得。",
    )
}

/// The name as displayed: the last path component, without control
/// characters, within [`MAX_FILE_NAME_CHARS`]. Browsers already send a bare
/// name; a hand-written request may not.
fn clean_file_name(raw: Option<&str>) -> String {
    let base = raw
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let name: String = base
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FILE_NAME_CHARS)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "未命名檔案".into()
    } else {
        name.to_string()
    }
}

/// The link a request names, or the response that ends it. Shared by the
/// status read and the upload, which refuse in the same words.
async fn open_link(
    s: &Shared,
    headers: &HeaderMap,
    secret: &str,
) -> Result<(UploadBatch, ObjectStore), Box<Response>> {
    let key = crate::auth::client_ip(headers).unwrap_or_else(|| "unknown".into());
    if !s.upload_rate_limiter.allow(&key) {
        return Err(Box::new(fail(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "上傳次數過多，請稍候一分鐘再試。",
        )));
    }
    let not_found = || {
        fail(
            StatusCode::NOT_FOUND,
            "link_not_found",
            "查無此上傳連結，請確認網址完整，或重新取得上傳連結。",
        )
    };
    if !plausible_secret(secret, SECRET_PREFIX) {
        return Err(Box::new(not_found()));
    }
    let batch = match s.store.upload_batch_by_secret(secret).await {
        Ok(Some(batch)) => batch,
        Ok(None) => return Err(Box::new(not_found())),
        Err(error) => {
            tracing::error!(%error, "reading an upload link failed");
            return Err(Box::new(fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "目前無法讀取上傳連結，請稍後再試。",
            )));
        }
    };
    if batch.is_expired(Utc::now()) || !batch.holder_active {
        return Err(Box::new(gone()));
    }
    let Some(objects) = s.objects.clone() else {
        return Err(Box::new(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "尚未接上物件儲存，請洽系統管理員。",
        )));
    };
    Ok((batch, objects))
}

fn remaining(batch: &UploadBatch) -> i32 {
    (batch.max_files - batch.used_files).max(0)
}

/// What the upload page shows before a file is picked.
pub async fn status(
    State(s): State<Shared>,
    Path(secret): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (batch, _) = match open_link(&s, &headers, &secret).await {
        Ok(open) => open,
        Err(response) => return *response,
    };
    let files = s.store.staged_uploads(batch.id).await.unwrap_or_default();
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "ok": true,
            "data": {
                "expires_at": batch.expires_at,
                "max_files": batch.max_files,
                "remaining": remaining(&batch),
                "max_file_bytes": MAX_UPLOAD_BYTES,
                "files": files.iter().map(|f| json!({
                    "file_name": f.file_name,
                    "byte_size": f.byte_size,
                })).collect::<Vec<_>>(),
            },
        })),
    )
        .into_response()
}

async fn audit(
    s: &Shared,
    headers: &HeaderMap,
    batch: &UploadBatch,
    params: Value,
    summary: &str,
    outcome: &str,
) {
    if let Err(error) = s
        .store
        .write_audit(
            &batch.actor,
            &batch.actor_role,
            "upload_file",
            "needs_confirm",
            None,
            &batch.channel,
            &params,
            summary,
            outcome,
            None,
            crate::auth::client_ip(headers).as_deref(),
        )
        .await
    {
        tracing::error!(%error, "audit write for an upload failed");
    }
}

/// Receive one file.
///
/// One file per request: the field named `file`. A request that carries a
/// second one has its first stored and is told the rest were not, rather
/// than being refused whole or having them dropped silently.
pub async fn receive(
    State(s): State<Shared>,
    Path(secret): Path<String>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let (batch, objects) = match open_link(&s, &headers, &secret).await {
        Ok(open) => open,
        Err(response) => return *response,
    };

    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > UPLOAD_BODY_LIMIT) {
        let message = format!(
            "檔案超過單檔上限 {} MB，請壓縮或分成幾個檔案再上傳。",
            MAX_UPLOAD_BYTES / (1024 * 1024)
        );
        audit(
            &s,
            &headers,
            &batch,
            json!({ "batch_id": batch.id, "declared_bytes": declared }),
            &format!("拒收上傳 · {message}"),
            "blocked",
        )
        .await;
        return fail(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message);
    }

    match s.store.claim_upload_place(batch.id).await {
        Ok(true) => {}
        Ok(false) => {
            return fail(
                StatusCode::CONFLICT,
                "link_full",
                format!(
                    "本連結已收滿 {} 個檔案，其餘檔案請重新取得上傳連結。",
                    batch.max_files
                ),
            )
        }
        Err(error) => {
            tracing::error!(%error, "claiming an upload place failed");
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "目前無法收件，請稍後再試。",
            );
        }
    }

    let outcome = stage(&s, &batch, &objects, request).await;
    match outcome {
        Ok((staged, extra)) => {
            audit(
                &s,
                &headers,
                &batch,
                json!({
                    "batch_id": batch.id,
                    "upload_id": staged.id,
                    "file_name": staged.file_name,
                    "byte_size": staged.byte_size,
                }),
                &format!(
                    "收到上傳 {} · {} · 指紋 {}",
                    staged.file_name,
                    human_size(staged.byte_size),
                    short_checksum(&staged.checksum)
                ),
                "ok",
            )
            .await;
            (
                [(header::CACHE_CONTROL, "no-store")],
                Json(json!({
                    "ok": true,
                    "data": {
                        "upload_id": staged.id,
                        "file_name": staged.file_name,
                        "byte_size": staged.byte_size,
                        "content_type": staged.content_type,
                        "checksum": staged.checksum,
                        "remaining": remaining(&batch) - 1,
                        "note": extra.then_some("一次請求只收一個檔案，其餘檔案沒有收下，請分開上傳。"),
                    },
                })),
            )
                .into_response()
        }
        Err(refusal) => {
            if let Err(error) = s.store.release_upload_place(batch.id).await {
                tracing::warn!(%error, "could not give an upload place back");
            }
            audit(
                &s,
                &headers,
                &batch,
                json!({ "batch_id": batch.id, "file_name": refusal.file_name }),
                &format!("拒收上傳 · {}", refusal.message),
                if refusal.status.is_server_error() {
                    "error"
                } else {
                    "blocked"
                },
            )
            .await;
            fail(refusal.status, refusal.code, refusal.message)
        }
    }
}

/// Why a file was not staged.
struct Refusal {
    status: StatusCode,
    code: &'static str,
    message: String,
    file_name: Option<String>,
}

impl Refusal {
    fn bad(message: impl Into<String>, file_name: Option<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_input",
            message: message.into(),
            file_name,
        }
    }

    fn storage(error: storage::StorageError, file_name: &str) -> Self {
        let file_name = Some(file_name.to_string());
        match error {
            storage::StorageError::TooLargeUpload { .. } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "too_large",
                message: error.to_string(),
                file_name,
            },
            storage::StorageError::ContentMismatch { .. } | storage::StorageError::Empty { .. } => {
                Self::bad(error.to_string(), file_name)
            }
            other => {
                tracing::error!(error = %other, "staging an upload failed");
                Self {
                    status: StatusCode::BAD_GATEWAY,
                    code: "storage_error",
                    message: "物件儲存暫時無法寫入，請稍後再試。".into(),
                    file_name,
                }
            }
        }
    }
}

/// Stream the first `file` field into the staging area and record it.
/// Returns the staged file and whether the request carried more files.
async fn stage(
    s: &Shared,
    batch: &UploadBatch,
    objects: &ObjectStore,
    request: Request,
) -> Result<(laysi_store::StagedUpload, bool), Refusal> {
    let mut form = Multipart::from_request(request, s).await.map_err(|_| {
        Refusal::bad(
            "請以 multipart/form-data 上傳，檔案放在名為 file 的欄位。",
            None,
        )
    })?;

    let mut field = loop {
        match form.next_field().await {
            Ok(Some(field)) if field.name() == Some("file") => break field,
            Ok(Some(_)) => continue,
            Ok(None) => return Err(Refusal::bad("請求裡沒有名為 file 的檔案欄位。", None)),
            Err(_) => return Err(Refusal::bad("上傳中斷，請重新上傳。", None)),
        }
    };

    let file_name = clean_file_name(field.file_name());
    let upload_id = Uuid::new_v4();
    let key = staging_key(batch.id, upload_id);
    let mut upload = objects.begin_upload(&key, &file_name, MAX_UPLOAD_BYTES);

    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                if let Err(error) = upload.write(&chunk).await {
                    upload.abort().await;
                    return Err(Refusal::storage(error, &file_name));
                }
            }
            Ok(None) => break,
            Err(error) => {
                upload.abort().await;
                let too_large = error.status() == StatusCode::PAYLOAD_TOO_LARGE;
                return Err(if too_large {
                    Refusal::storage(
                        storage::StorageError::TooLargeUpload {
                            name: file_name.clone(),
                            limit_mb: MAX_UPLOAD_BYTES / (1024 * 1024),
                        },
                        &file_name,
                    )
                } else {
                    Refusal::bad("上傳中斷，請重新上傳。", Some(file_name))
                });
            }
        }
    }
    drop(field);
    let stored = upload
        .finish()
        .await
        .map_err(|e| Refusal::storage(e, &file_name))?;

    let staged = match s
        .store
        .record_staged_upload(NewStagedUpload {
            id: upload_id,
            batch_id: batch.id,
            file_name: &file_name,
            byte_size: stored.byte_size,
            content_type: &stored.content_type,
            checksum: &stored.checksum,
            object_key: &stored.key,
        })
        .await
    {
        Ok(staged) => staged,
        Err(error) => {
            tracing::error!(%error, "recording a staged upload failed");
            objects.discard(&stored.key).await;
            return Err(Refusal {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "store_error",
                message: "檔案已收到但無法登記，請重新上傳。".into(),
                file_name: Some(file_name),
            });
        }
    };

    // Whatever follows is not read: a second file is reported, not taken.
    let extra = matches!(form.next_field().await, Ok(Some(f)) if f.name() == Some("file"));
    Ok((staged, extra))
}

/// Remove what nobody filed.
///
/// Four things, each able to fail without stopping the others: staged files
/// older than `keep`, with their objects; links expired longer than `keep`
/// with nothing left waiting; objects under the staging prefix no waiting row
/// points at (an interrupted recording, a filing whose clean-up failed); and
/// multipart uploads abandoned mid-way, whose parts are invisible to a
/// listing and hold space until aborted.
pub async fn sweep(
    store: &Store,
    objects: &ObjectStore,
    now: DateTime<Utc>,
    keep: chrono::Duration,
) {
    let cutoff = now - keep;
    let mut removed = 0;
    match store.stale_staged_uploads(cutoff).await {
        Ok(stale) => {
            for (id, key) in stale {
                objects.discard(&key).await;
                match store.delete_staged_upload(id).await {
                    Ok(()) => removed += 1,
                    Err(error) => {
                        tracing::warn!(%error, %id, "removing a stale staged upload failed")
                    }
                }
            }
        }
        Err(error) => tracing::warn!(%error, "listing stale staged uploads failed"),
    }
    let batches = store
        .delete_spent_upload_batches(cutoff)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "removing spent upload links failed");
            0
        });

    // An hour past `keep`, so an object whose row is being written right now
    // is never taken for an orphan.
    let mut orphans = 0;
    let orphan_cutoff = cutoff - chrono::Duration::hours(1);
    match (
        store.waiting_staged_keys().await,
        objects.keys_older_than(STAGING_PREFIX, orphan_cutoff).await,
    ) {
        (Ok(waiting), Ok(keys)) => {
            for key in keys.into_iter().filter(|k| !waiting.contains(k)) {
                objects.discard(&key).await;
                orphans += 1;
            }
        }
        (Err(error), _) => tracing::warn!(%error, "listing waiting staged uploads failed"),
        (_, Err(error)) => tracing::warn!(%error, "listing the staging area failed"),
    }

    let aborted = objects
        .abort_stale_uploads(STAGING_PREFIX, now - chrono::Duration::hours(2))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "listing unfinished multipart uploads failed");
            0
        });

    if removed + batches + orphans as u64 + aborted as u64 > 0 {
        tracing::info!(removed, batches, orphans, aborted, "staging area swept");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_keeps_only_its_last_component() {
        assert_eq!(clean_file_name(Some("C:\\帳務\\發票.pdf")), "發票.pdf");
        assert_eq!(clean_file_name(Some("../../etc/passwd")), "passwd");
        assert_eq!(clean_file_name(Some("收據\u{0}\n.jpg")), "收據.jpg");
        assert_eq!(clean_file_name(Some("   ")), "未命名檔案");
        assert_eq!(clean_file_name(None), "未命名檔案");
        assert_eq!(
            clean_file_name(Some(&"長".repeat(500))).chars().count(),
            MAX_FILE_NAME_CHARS
        );
    }

    #[test]
    fn only_issued_secrets_reach_the_database() {
        let issued = format!(
            "lysu_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        assert!(plausible_secret(&issued, SECRET_PREFIX));
        assert!(!plausible_secret("lysu_short", SECRET_PREFIX));
        assert!(!plausible_secret(
            &issued.replace("lysu_", "lys_x"),
            SECRET_PREFIX
        ));
        assert!(!plausible_secret(
            &format!("{}Z", &issued[..68]),
            SECRET_PREFIX
        ));
    }
}
