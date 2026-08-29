//! RustFS object storage: files really are stored there, and the database
//! keeps only the key and the hash.
//!
//! PostgreSQL could hold bytea, but that makes backups, replication and the
//! WAL carry the bulk of scanned documents, which are the only thing here
//! that reaches gigabyte scale. RustFS is the S3-compatible object store
//! already running in compose, and this module is its first user.
//!
//! The database side stores four things: object key, byte size, content type
//! and BLAKE3 hash. There is no second copy of the content and no room for
//! "the database says 3 MB while the bucket holds 5 MB".
//!
//! The hash is not about saving space, it proves the content was not
//! swapped. Supporting documents exist so that the original can be produced
//! when someone asks later, and a bucket can be overwritten from an admin
//! console or restored from a backup. Only a hash recorded at upload time
//! can show on retrieval that this is the same file.
//!
//! [`ObjectStore::get`] recomputes and compares on every retrieval and
//! returns [`StorageError::ChecksumMismatch`] without the content when they
//! differ. It uses the same hash function as API tokens (BLAKE3) for the
//! same reason: cryptographic strength is required.
//!
//! The bucket is not public. RustFS port 9000 is not mapped to the host and
//! no presigned URLs are issued, so every download is proxied by the backend
//! and therefore passes the operation registry and reaches the audit trail.
//! A presigned URL, once issued, is a channel around the audit that cannot
//! be withdrawn.

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use std::time::Duration;

/// Bucket name used when the environment variable is unset.
const DEFAULT_BUCKET: &str = "laysi-documents";

/// RustFS has no regions, but SigV4 requires one to be signed. The name
/// itself carries no meaning.
const DEFAULT_REGION: &str = "us-east-1";

/// Maximum size of a single object, in bytes.
///
/// An operational limit rather than a regulation value: content travels as
/// base64 in JSON, so a 25 MB scan becomes a 34 MB request body. Larger
/// files need streaming uploads, not a larger constant.
pub const MAX_OBJECT_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("物件儲存尚未設定：{0}")]
    NotConfigured(String),

    #[error("物件儲存 {op} 失敗：{message}")]
    S3 { op: &'static str, message: String },

    #[error("物件 {key} 不在 bucket 裡")]
    NotFound { key: String },

    #[error(
        "檔案內容與登記的雜湊不符（{key}）：登記 {expected}，實際 {actual}。\
         這份檔案在上傳之後被動過，系統不會把它交出去。"
    )]
    ChecksumMismatch {
        key: String,
        expected: String,
        actual: String,
    },

    #[error("檔案 {name} 有 {size} 位元組，超過單檔上限 {limit}")]
    TooLarge {
        name: String,
        size: usize,
        limit: usize,
    },

    #[error("檔案是空的：{name}")]
    Empty { name: String },
}

impl StorageError {
    fn s3<E: std::fmt::Display, R>(op: &'static str) -> impl FnOnce(SdkError<E, R>) -> Self {
        move |e| StorageError::S3 {
            op,
            message: format!("{e}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;

/// Result of one upload. These four fields are everything the database
/// stores.
#[derive(Debug, Clone)]
pub struct StoredObject {
    pub key: String,
    pub byte_size: i64,
    pub content_type: String,
    /// BLAKE3 as hex, recomputed and compared on retrieval.
    pub checksum: String,
}

/// Bucket status. `/healthz` uses it to upgrade "is the port open" into
/// "can the buckets be listed".
#[derive(Debug, Clone)]
pub struct BucketHealth {
    pub endpoint: String,
    pub bucket: String,
    /// The bucket list really came back, not just a TCP connection.
    pub reachable: bool,
    /// Our bucket is in that list.
    pub bucket_exists: bool,
    pub bucket_count: usize,
    pub error: Option<String>,
}

/// Usage inside the bucket, the source of the "space used" figure on screen.
#[derive(Debug, Clone, Default)]
pub struct BucketUsage {
    pub objects: i64,
    pub bytes: i64,
}

/// RustFS client. `Clone` is cheap; it is an `Arc` inside.
#[derive(Clone)]
pub struct ObjectStore {
    client: Client,
    bucket: String,
    endpoint: String,
}

impl ObjectStore {
    /// Build from the environment.
    ///
    /// A missing `RUSTFS_ENDPOINT`, `RUSTFS_ACCESS_KEY` or
    /// `RUSTFS_SECRET_KEY` is an error rather than a start that only fails
    /// when someone tries to upload.
    pub fn from_env() -> Result<Self> {
        let endpoint = std::env::var("RUSTFS_ENDPOINT")
            .map_err(|_| StorageError::NotConfigured("RUSTFS_ENDPOINT".into()))?;
        let access_key = std::env::var("RUSTFS_ACCESS_KEY")
            .map_err(|_| StorageError::NotConfigured("RUSTFS_ACCESS_KEY".into()))?;
        let secret_key = crate::env_or_file("RUSTFS_SECRET_KEY")
            .ok_or_else(|| StorageError::NotConfigured("RUSTFS_SECRET_KEY".into()))?;
        let bucket = std::env::var("RUSTFS_BUCKET").unwrap_or_else(|_| DEFAULT_BUCKET.into());
        let region = std::env::var("RUSTFS_REGION").unwrap_or_else(|_| DEFAULT_REGION.into());
        Ok(Self::new(
            &endpoint,
            &access_key,
            &secret_key,
            &bucket,
            &region,
        ))
    }

    pub fn new(
        endpoint: &str,
        access_key: &str,
        secret_key: &str,
        bucket: &str,
        region: &str,
    ) -> Self {
        // Pure Rust TLS (ring). The default aws-lc-rs needs cmake, which the
        // build image does not have.
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();

        let conf = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region.to_string()))
            .endpoint_url(endpoint)
            // RustFS has no virtual-hosted bucket URLs, so path style is
            // required.
            .force_path_style(true)
            .credentials_provider(Credentials::new(
                access_key,
                secret_key,
                None,
                None,
                "laysi-env",
            ))
            .http_client(http)
            .timeout_config(
                aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                    .operation_attempt_timeout(Duration::from_secs(20))
                    .connect_timeout(Duration::from_secs(5))
                    .build(),
            )
            .build();

        Self {
            client: Client::from_conf(conf),
            bucket: bucket.to_string(),
            endpoint: endpoint.to_string(),
        }
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Create the bucket if it does not exist. Run once at startup and
    /// confirmed again before every upload.
    ///
    /// Returns `true` when this call actually created a new bucket.
    pub async fn ensure_bucket(&self) -> Result<bool> {
        match self.client.head_bucket().bucket(&self.bucket).send().await {
            Ok(_) => Ok(false),
            Err(_) => {
                match self
                    .client
                    .create_bucket()
                    .bucket(&self.bucket)
                    .send()
                    .await
                {
                    Ok(_) => Ok(true),
                    // Two processes starting together will race; treat "someone
                    // else already created it" as success.
                    Err(e) => {
                        let msg = format!("{e}");
                        if msg.contains("BucketAlreadyOwnedByYou")
                            || msg.contains("BucketAlreadyExists")
                        {
                            Ok(false)
                        } else {
                            Err(StorageError::S3 {
                                op: "create_bucket",
                                message: msg,
                            })
                        }
                    }
                }
            }
        }
    }

    /// Health check: actually call S3 to list buckets rather than confirming
    /// an open port.
    ///
    /// An open port only proves a process is listening. Wrong credentials, a
    /// deleted bucket or a full disk all still accept TCP, and that kind of
    /// "healthy" is worthless at the moment a file has to be stored.
    pub async fn health(&self) -> BucketHealth {
        match self.client.list_buckets().send().await {
            Ok(out) => {
                let names: Vec<String> = out
                    .buckets()
                    .iter()
                    .filter_map(|b| b.name().map(str::to_string))
                    .collect();
                BucketHealth {
                    endpoint: self.endpoint.clone(),
                    bucket: self.bucket.clone(),
                    reachable: true,
                    bucket_exists: names.iter().any(|n| n == &self.bucket),
                    bucket_count: names.len(),
                    error: None,
                }
            }
            Err(e) => BucketHealth {
                endpoint: self.endpoint.clone(),
                bucket: self.bucket.clone(),
                reachable: false,
                bucket_exists: false,
                bucket_count: 0,
                error: Some(format!("{e}")),
            },
        }
    }

    /// Upload one file.
    ///
    /// The hash is computed before sending and written into the object's
    /// metadata as well. Both the database and the bucket hold a copy, so
    /// both would have to be tampered with to fool the check on retrieval.
    pub async fn put(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
        display_name: &str,
    ) -> Result<StoredObject> {
        if body.is_empty() {
            return Err(StorageError::Empty {
                name: display_name.to_string(),
            });
        }
        if body.len() > MAX_OBJECT_BYTES {
            return Err(StorageError::TooLarge {
                name: display_name.to_string(),
                size: body.len(),
                limit: MAX_OBJECT_BYTES,
            });
        }

        self.ensure_bucket().await?;
        let checksum = blake3::hash(body).to_hex().to_string();

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(body.to_vec()))
            .content_type(content_type)
            .metadata("blake3", &checksum)
            .send()
            .await
            .map_err(StorageError::s3("put_object"))?;

        Ok(StoredObject {
            key: key.to_string(),
            byte_size: body.len() as i64,
            content_type: content_type.to_string(),
            checksum,
        })
    }

    /// Remove one object, reporting nothing.
    ///
    /// Only for undoing an upload whose database row could not be written: an
    /// orphan in the bucket is invisible and counts against storage forever,
    /// while a failed removal leaves exactly the orphan there would have been
    /// anyway. Documents themselves are never deleted -- the archive is
    /// immutable -- so this has no caller on any success path.
    pub async fn discard(&self, key: &str) {
        let outcome = self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;
        if let Err(e) = outcome {
            tracing::warn!(key, error = %e, "上傳失敗後清不掉物件，bucket 裡留下一個孤兒");
        }
    }

    /// Retrieve a file and verify its hash.
    ///
    /// `expected` is the hash registered in the database. A mismatch returns
    /// an error without the content: a tampered document is more dangerous
    /// than a missing one, because it looks genuine.
    pub async fn get(&self, key: &str, expected: &str) -> Result<Vec<u8>> {
        let out = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                let msg = format!("{e}");
                if msg.contains("NoSuchKey") || msg.contains("NotFound") {
                    StorageError::NotFound {
                        key: key.to_string(),
                    }
                } else {
                    StorageError::S3 {
                        op: "get_object",
                        message: msg,
                    }
                }
            })?;

        let body = out
            .body
            .collect()
            .await
            .map_err(|e| StorageError::S3 {
                op: "get_object.body",
                message: format!("{e}"),
            })?
            .into_bytes();

        let actual = blake3::hash(&body).to_hex().to_string();
        if actual != expected {
            return Err(StorageError::ChecksumMismatch {
                key: key.to_string(),
                expected: expected.to_string(),
                actual,
            });
        }
        Ok(body.to_vec())
    }

    /// Object count and byte size in the bucket, the only source of the
    /// "space used" figure on screen.
    pub async fn usage(&self) -> Result<BucketUsage> {
        let mut usage = BucketUsage::default();
        let mut token: Option<String> = None;
        loop {
            let out = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .set_continuation_token(token)
                .send()
                .await
                .map_err(StorageError::s3("list_objects_v2"))?;

            for o in out.contents() {
                usage.objects += 1;
                usage.bytes += o.size().unwrap_or(0);
            }
            token = out.next_continuation_token().map(str::to_string);
            if token.is_none() {
                return Ok(usage);
            }
        }
    }
}

/// Object key layout: `<yyyymm>/<number>-v<revision>-<safe-name>`.
///
/// The year and month make the bucket navigable from an admin console; the
/// number and revision make one key correspond to exactly one piece of
/// content. A key is never overwritten; a new revision writes a new key.
/// Without object versioning, overwriting destroys the old document.
pub fn object_key(period: &str, doc_no: &str, version_no: i32, file_name: &str) -> String {
    format!("{period}/{doc_no}-v{version_no}-{}", sanitise(file_name))
}

/// Only alphanumerics and `.-_` survive in a file name; everything else
/// becomes `_`.
///
/// Non-ASCII names are preserved in full in the database (`document.name`),
/// while the object key is only a location. Putting them in the key would
/// break URL encoding, command line tools and backup scripts each in their
/// own way.
fn sanitise(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "file".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

/// Guess the content type from the extension, falling back to
/// `application/octet-stream`.
///
/// A wrong guess only means the browser downloads instead of displaying
/// inline. There is no reason to refuse to guess here, because no amount or
/// legal effect depends on it.
pub fn guess_content_type(file_name: &str) -> &'static str {
    let ext = file_name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "csv" => "text/csv; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "json" => "application/json",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

/// BLAKE3 of the content as hex. Used on both upload and retrieval.
pub fn checksum(body: &[u8]) -> String {
    blake3::hash(body).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_keys_never_collide_between_versions() {
        let a = object_key("2607", "D2607-001", 1, "電費單_202606.pdf");
        let b = object_key("2607", "D2607-001", 2, "電費單_202606.pdf");
        assert_ne!(a, b, "改版必須是新的鍵，覆寫等於銷毀舊憑證");
        assert!(a.starts_with("2607/D2607-001-v1-"));
    }

    /// Non-ASCII names stay out of the object key without leaving it empty.
    #[test]
    fn keys_are_ascii_and_never_empty() {
        let key = object_key("2607", "D2607-002", 1, "供應商請款單.pdf");
        assert!(
            key.is_ascii(),
            "物件鍵要能安全地出現在網址與命令列裡：{key}"
        );
        assert!(key.ends_with("_.pdf") || key.ends_with(".pdf"));

        let only_chinese = object_key("2607", "D2607-003", 1, "合約");
        assert!(
            only_chinese.ends_with("file"),
            "全中文檔名也要有個位置：{only_chinese}"
        );
    }

    #[test]
    fn content_types_come_from_the_extension() {
        assert_eq!(guess_content_type("a.PDF"), "application/pdf");
        assert_eq!(guess_content_type("對帳單.csv"), "text/csv; charset=utf-8");
        assert_eq!(guess_content_type("沒有副檔名"), "application/octet-stream");
    }

    /// The hash proves "same content", so one differing byte must differ.
    #[test]
    fn the_checksum_changes_with_a_single_byte() {
        let a = checksum(b"V2607-018 8420");
        let b = checksum(b"V2607-018 8421");
        assert_ne!(a, b);
        assert_eq!(a.len(), 64, "BLAKE3 十六進位是 64 個字");
        assert_eq!(
            a,
            checksum(b"V2607-018 8420"),
            "同樣的內容一定得到同樣的雜湊"
        );
    }
}
