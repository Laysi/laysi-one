//! The sending end of a download link: `GET /v1/downloads/{secret}`.
//!
//! A third hand-written route, beside `/v1/auth/*` and `/v1/uploads/*`, and
//! for the same reason as the upload one: what leaves here is a file rather
//! than an operation result. Issuing the link is an operation
//! (`get_download_link`), so the tool switch, the role and the audit all
//! applied there; this route only sends the one document version the link
//! names, once, and the link is its whole authorisation.
//!
//! Nothing is sent before the hash is checked. The object is spooled to an
//! unlinked local file while its BLAKE3 is taken, and only a match turns into
//! a response with a body; a mismatch answers with an error and the file,
//! the one place the bytes were, is dropped. At most [`DOWNLOAD_SLOTS`]
//! downloads hold a spooled file at once, which bounds the disk they use.

use crate::uploads::{fail, plausible_secret};
use crate::Shared;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use laysi_ops::ops::documents::{human_size, short_checksum};
use laysi_store::storage::{StorageError, VerifiedFile};
use laysi_store::DownloadLink;
use serde_json::{json, Value};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::OwnedSemaphorePermit;

/// Downloads that may hold a spooled file at once. Each holds at most one
/// document, 90 MB at the upload ceiling, until its response is sent.
pub const DOWNLOAD_SLOTS: usize = 8;

/// How long a download waits for a slot before it is turned away. The link
/// is not spent by waiting, so the same link works on a second try.
const SLOT_WAIT_SECONDS: u64 = 30;

/// Requests per source address per minute. A screen asks for one link per
/// click; guessing secrets needs far more.
pub const DOWNLOADS_PER_MINUTE: u32 = 60;

/// Prefix of every secret `create_download_link` issues.
const SECRET_PREFIX: &str = "lysd_";

fn gone() -> Response {
    fail(
        StatusCode::GONE,
        "link_expired",
        "下載連結已使用過或已失效，請回到原本的畫面或對話重新下載。",
    )
}

/// `Content-Disposition` for a file name that may be anything a person typed:
/// an ASCII fallback for old clients, and the name itself as RFC 5987
/// UTF-8, which is what every current browser reads.
fn attachment(file_name: &str) -> String {
    let fallback: String = file_name
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() && c != '"' && c != '\\') || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in file_name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

async fn audit(s: &Shared, headers: &HeaderMap, link: &DownloadLink, summary: &str, outcome: &str) {
    let params: Value = json!({
        "doc_no": link.doc_no,
        "version": link.version_no,
        "link_id": link.id,
    });
    if let Err(error) = s
        .store
        .write_audit(
            &link.actor,
            &link.actor_role,
            "download_file",
            "read_only",
            None,
            &link.channel,
            &params,
            summary,
            outcome,
            None,
            crate::auth::client_ip(headers).as_deref(),
        )
        .await
    {
        tracing::error!(%error, "audit write for a download failed");
    }
}

/// A spooled file that keeps its download slot until the response has been
/// read to the end or dropped.
struct Spooled {
    file: tokio::fs::File,
    _slot: OwnedSemaphorePermit,
}

impl AsyncRead for Spooled {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_read(cx, buf)
    }
}

fn send(link: &DownloadLink, verified: VerifiedFile, slot: OwnedSemaphorePermit) -> Response {
    let content_type = HeaderValue::from_str(&link.content_type)
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    let disposition = HeaderValue::from_str(&attachment(&link.file_name))
        .unwrap_or(HeaderValue::from_static("attachment"));
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(Spooled {
        file: verified.file,
        _slot: slot,
    }));
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_LENGTH, HeaderValue::from(verified.size)),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        body,
    )
        .into_response()
}

/// Send the document a link opens, once, after checking its hash.
pub async fn serve(
    State(s): State<Shared>,
    Path(secret): Path<String>,
    headers: HeaderMap,
) -> Response {
    let key = crate::auth::client_ip(&headers).unwrap_or_else(|| "unknown".into());
    if !s.download_rate_limiter.allow(&key) {
        return fail(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "下載次數過多，請稍候一分鐘再試。",
        );
    }
    let not_found = || {
        fail(
            StatusCode::NOT_FOUND,
            "link_not_found",
            "查無此下載連結，請確認網址完整，或重新下載。",
        )
    };
    if !plausible_secret(&secret, SECRET_PREFIX) {
        return not_found();
    }
    let link = match s.store.download_link_by_secret(&secret).await {
        Ok(Some(link)) => link,
        Ok(None) => return not_found(),
        Err(error) => {
            tracing::error!(%error, "reading a download link failed");
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "目前無法讀取下載連結，請稍後再試。",
            );
        }
    };
    if link.used_at.is_some() || link.is_expired(chrono::Utc::now()) || !link.holder_active {
        return gone();
    }
    let Some(objects) = s.objects.clone() else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "尚未接上物件儲存，請洽系統管理員。",
        );
    };

    // A slot before the link is spent, so a download turned away for being
    // one too many can try the same link again.
    let slot = match tokio::time::timeout(
        std::time::Duration::from_secs(SLOT_WAIT_SECONDS),
        s.download_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(slot)) => slot,
        _ => {
            return fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "目前同時下載的檔案較多，請稍候再試。",
            )
        }
    };

    match s.store.use_download_link(link.id).await {
        Ok(true) => {}
        Ok(false) => return gone(),
        Err(error) => {
            tracing::error!(%error, "spending a download link failed");
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "目前無法讀取下載連結，請稍後再試。",
            );
        }
    }

    let label = format!("{} v{} {}", link.doc_no, link.version_no, link.file_name);
    match objects
        .spool(
            &link.object_key,
            &link.checksum,
            u64::try_from(link.byte_size).unwrap_or(0),
            &std::env::temp_dir(),
        )
        .await
    {
        Ok(verified) => {
            audit(
                &s,
                &headers,
                &link,
                &format!(
                    "下載 {label} · {} · 雜湊比對相符（{}）",
                    human_size(link.byte_size),
                    short_checksum(&link.checksum)
                ),
                "ok",
            )
            .await;
            send(&link, verified, slot)
        }
        Err(StorageError::ChecksumMismatch { .. }) => {
            audit(
                &s,
                &headers,
                &link,
                &format!("拒絕下載 {label} · 檔案內容與登記的雜湊不符"),
                "blocked",
            )
            .await;
            fail(
                StatusCode::CONFLICT,
                "checksum_mismatch",
                format!(
                    "{} v{} 的檔案內容與上傳時登記的雜湊不符，檔案在上傳之後被更動過，無法下載。請洽系統管理員從備份還原。",
                    link.doc_no, link.version_no
                ),
            )
        }
        Err(StorageError::NotFound { .. }) => {
            audit(
                &s,
                &headers,
                &link,
                &format!("下載失敗 {label} · 物件儲存裡找不到這份檔案"),
                "error",
            )
            .await;
            fail(
                StatusCode::NOT_FOUND,
                "object_missing",
                format!(
                    "物件儲存裡找不到 {} v{} 的檔案，請洽系統管理員從備份還原。",
                    link.doc_no, link.version_no
                ),
            )
        }
        Err(error) => {
            tracing::error!(%error, doc_no = %link.doc_no, "spooling a download failed");
            audit(
                &s,
                &headers,
                &link,
                &format!("下載失敗 {label} · {error}"),
                "error",
            )
            .await;
            fail(
                StatusCode::BAD_GATEWAY,
                "storage_error",
                "物件儲存暫時無法讀取，請稍後重新下載。",
            )
        }
    }
}

/// `HEAD` would run the whole download to learn its headers and spend the
/// link doing it.
pub async fn head_refused() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET")]).into_response()
}

/// Remove links that expired more than a day ago.
pub async fn sweep(store: &laysi_store::Store, now: chrono::DateTime<chrono::Utc>) {
    match store
        .delete_spent_download_links(now - chrono::Duration::hours(24))
        .await
    {
        Ok(0) => {}
        Ok(removed) => tracing::info!(removed, "spent download links removed"),
        Err(error) => tracing::warn!(%error, "removing spent download links failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_survives_the_header() {
        assert_eq!(
            attachment("invoice.pdf"),
            "attachment; filename=\"invoice.pdf\"; filename*=UTF-8''invoice.pdf"
        );
        let header = attachment("九月 電費\"單\".pdf");
        assert!(header.is_ascii(), "{header}");
        assert!(HeaderValue::from_str(&header).is_ok(), "{header}");
        assert!(
            header.contains(
                "filename*=UTF-8''%E4%B9%9D%E6%9C%88%20%E9%9B%BB%E8%B2%BB%22%E5%96%AE%22.pdf"
            ),
            "{header}"
        );
        assert!(
            header.starts_with("attachment; filename=\"__ _____.pdf\""),
            "{header}"
        );
    }

    #[test]
    fn only_issued_secrets_reach_the_database() {
        let issued = format!(
            "{SECRET_PREFIX}{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        assert!(plausible_secret(&issued, SECRET_PREFIX));
        assert!(
            !plausible_secret(&issued.replacen("lysd_", "lysu_", 1), SECRET_PREFIX),
            "上傳連結的秘密不是下載連結"
        );
    }
}
