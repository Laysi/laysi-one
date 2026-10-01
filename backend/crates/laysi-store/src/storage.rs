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
//!
//! A document too large to return inside an operation result goes out
//! through a download link instead, and the rule above holds for it too: the
//! link is issued by an operation, used once through the backend, and
//! [`ObjectStore::spool`] lands the whole object on local disk and checks its
//! hash before the first byte is sent. Checking while sending would have sent
//! most of a tampered file by the time the mismatch showed.

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, MetadataDirective};
use aws_sdk_s3::Client;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

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

/// Largest file an upload link accepts, in bytes.
///
/// Streamed, so memory is not what bounds it. What does is the per-request
/// ceiling of whatever sits in front of the server: Cloudflare's Free and Pro
/// plans refuse a request body over 100 MB, and one file travels as one
/// multipart request, so this leaves room for the multipart framing.
pub const MAX_UPLOAD_BYTES: usize = 90 * 1024 * 1024;

/// Size of each part of a streamed upload. S3 requires at least 5 MiB for
/// every part but the last; this is what one upload holds in memory at most.
pub const PART_BYTES: usize = 8 * 1024 * 1024;

/// How much of the start of a file the type is decided from.
pub const DETECT_BYTES: usize = 8 * 1024;

/// How much of an object is read at a time while it is hashed.
const READ_BYTES: usize = 64 * 1024;

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

    #[error("「{name}」超過單檔上限 {limit_mb} MB，請壓縮或分成幾個檔案再上傳。")]
    TooLargeUpload { name: String, limit_mb: usize },

    #[error("「{name}」的副檔名與檔案內容不符，請確認檔案沒有損毀，或改用正確的副檔名後再上傳。")]
    ContentMismatch { name: String },

    #[error("暫存檔寫入失敗：{0}")]
    LocalFile(String),
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

/// A document's bytes on local disk, already checked against its hash.
pub struct VerifiedFile {
    /// Positioned at the start. Unlinked when it was created, so the bytes
    /// leave the disk when this closes, however the request that holds it
    /// ends.
    pub file: tokio::fs::File,
    pub size: u64,
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
///
/// Staged uploads are counted apart: they are not documents yet, and the
/// documents screen reconciles `bytes` against what the database registers.
#[derive(Debug, Clone, Default)]
pub struct BucketUsage {
    pub objects: i64,
    pub bytes: i64,
    pub staged_objects: i64,
    pub staged_bytes: i64,
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
            tracing::warn!(key, error = %e, "could not remove the object after a failed upload, an orphan is left in the bucket");
        }
    }

    /// The object's body, not yet read.
    async fn open(&self, key: &str) -> Result<ByteStream> {
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
        Ok(out.body)
    }

    /// Retrieve a file and verify its hash.
    ///
    /// `expected` is the hash registered in the database. A mismatch returns
    /// an error without the content: a tampered document is more dangerous
    /// than a missing one, because it looks genuine.
    pub async fn get(&self, key: &str, expected: &str) -> Result<Vec<u8>> {
        let body = self
            .open(key)
            .await?
            .collect()
            .await
            .map_err(|e| StorageError::S3 {
                op: "get_object.body",
                message: format!("{e}"),
            })?
            .into_bytes();

        compare(key, expected, checksum(&body))?;
        Ok(body.to_vec())
    }

    /// Recompute an object's hash without holding the object, for a document
    /// too large to return. `size` is the registered size: reading stops past
    /// it, since the content can no longer be the registered one. Returns the
    /// size read.
    pub async fn verify(&self, key: &str, expected: &str, size: u64) -> Result<u64> {
        let body = self.open(key).await?.into_async_read();
        let Some((actual, read)) = hash_through(body, None, size).await? else {
            return Err(oversized(key, expected, size));
        };
        compare(key, expected, actual)?;
        Ok(read)
    }

    /// Copy an object into an anonymous file under `dir`, hashing it on the
    /// way, and hand the file over only if the hash is `expected`. Copying
    /// stops past the registered `size`, so an object swapped for a larger
    /// one cannot fill the disk before the mismatch shows.
    pub async fn spool(
        &self,
        key: &str,
        expected: &str,
        size: u64,
        dir: &Path,
    ) -> Result<VerifiedFile> {
        let body = self.open(key).await?.into_async_read();
        spool_verified(body, key, expected, size, dir).await
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
                let size = o.size().unwrap_or(0);
                if o.key()
                    .is_some_and(|k| k.starts_with(crate::uploads::STAGING_PREFIX))
                {
                    usage.staged_objects += 1;
                    usage.staged_bytes += size;
                } else {
                    usage.objects += 1;
                    usage.bytes += size;
                }
            }
            token = out.next_continuation_token().map(str::to_string);
            if token.is_none() {
                return Ok(usage);
            }
        }
    }

    /// Start a streamed upload to `key`. Nothing is sent until the first part
    /// fills or [`ObjectUpload::finish`] is called.
    pub fn begin_upload(&self, key: &str, file_name: &str, limit: usize) -> ObjectUpload {
        ObjectUpload {
            store: self.clone(),
            key: key.to_string(),
            file_name: file_name.to_string(),
            limit,
            size: 0,
            hasher: blake3::Hasher::new(),
            parts: PartBuffer::default(),
            content_type: None,
            multipart: None,
        }
    }

    /// Copy a staged object to its document key, stamping the type and the
    /// hash on the copy the way [`ObjectStore::put`] stamps them on a direct
    /// upload. The staged object is left for the caller to remove.
    ///
    /// Server side, so the bytes do not pass through this process again.
    pub async fn copy_to(
        &self,
        from: &str,
        to: &str,
        content_type: &str,
        checksum: &str,
    ) -> Result<()> {
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .copy_source(format!("{}/{from}", self.bucket))
            .key(to)
            .metadata_directive(MetadataDirective::Replace)
            .content_type(content_type)
            .metadata("blake3", checksum)
            .send()
            .await
            .map_err(StorageError::s3("copy_object"))?;
        Ok(())
    }

    /// Keys under `prefix` last written before `cutoff`.
    pub async fn keys_older_than(
        &self,
        prefix: &str,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let out = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .set_continuation_token(token)
                .send()
                .await
                .map_err(StorageError::s3("list_objects_v2"))?;
            for o in out.contents() {
                let old = o
                    .last_modified()
                    .is_some_and(|at| at.secs() < cutoff.timestamp());
                if let (true, Some(key)) = (old, o.key()) {
                    keys.push(key.to_string());
                }
            }
            token = out.next_continuation_token().map(str::to_string);
            if token.is_none() {
                return Ok(keys);
            }
        }
    }

    /// Abort multipart uploads under `prefix` started before `cutoff`. Their
    /// parts are invisible to a listing and hold space until aborted, and an
    /// upload whose connection dropped leaves exactly that behind.
    pub async fn abort_stale_uploads(
        &self,
        prefix: &str,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize> {
        let out = self
            .client
            .list_multipart_uploads()
            .bucket(&self.bucket)
            .prefix(prefix)
            .send()
            .await
            .map_err(StorageError::s3("list_multipart_uploads"))?;
        let mut aborted = 0;
        for upload in out.uploads() {
            let stale = upload
                .initiated()
                .is_some_and(|at| at.secs() < cutoff.timestamp());
            let (true, Some(key), Some(id)) = (stale, upload.key(), upload.upload_id()) else {
                continue;
            };
            match self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(id)
                .send()
                .await
            {
                Ok(_) => aborted += 1,
                Err(e) => {
                    tracing::warn!(key, error = %e, "could not abort a stale multipart upload")
                }
            }
        }
        Ok(aborted)
    }
}

/// `Ok` only when the recomputed hash is the registered one.
fn compare(key: &str, expected: &str, actual: String) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(StorageError::ChecksumMismatch {
            key: key.to_string(),
            expected: expected.to_string(),
            actual,
        })
    }
}

/// An object that turned out longer than its registered size: a mismatch,
/// found before the whole of it was read.
fn oversized(key: &str, expected: &str, limit: u64) -> StorageError {
    StorageError::ChecksumMismatch {
        key: key.to_string(),
        expected: expected.to_string(),
        actual: format!("超過登記的 {limit} 位元組"),
    }
}

/// Read `body` to the end, hashing it and, when given one, copying it into
/// `sink`. Returns the hash as hex and the size, or `None` as soon as more
/// than `limit` bytes have arrived; nothing past `limit` reaches `sink`.
async fn hash_through<R: AsyncRead + Unpin>(
    mut body: R,
    mut sink: Option<&mut tokio::fs::File>,
    limit: u64,
) -> Result<Option<(String, u64)>> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_BYTES];
    let mut size = 0u64;
    loop {
        let n = body.read(&mut buf).await.map_err(|e| StorageError::S3 {
            op: "get_object.body",
            message: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        if size + n as u64 > limit {
            return Ok(None);
        }
        hasher.update(&buf[..n]);
        if let Some(file) = sink.as_deref_mut() {
            file.write_all(&buf[..n])
                .await
                .map_err(|e| StorageError::LocalFile(e.to_string()))?;
        }
        size += n as u64;
    }
    if let Some(file) = sink {
        file.flush()
            .await
            .map_err(|e| StorageError::LocalFile(e.to_string()))?;
    }
    Ok(Some((hasher.finalize().to_hex().to_string(), size)))
}

/// A file under `dir` that only the returned handle can reach: created
/// readable by this user alone, then unlinked at once. The open handle keeps
/// the bytes, and nothing is left on disk once it closes, whether the request
/// finished, the client went away or the process died.
async fn anonymous_file(dir: &Path) -> std::io::Result<tokio::fs::File> {
    let path = dir.join(format!(".laysi-download-{}", uuid::Uuid::new_v4().simple()));
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(&path).await?;
    tokio::fs::remove_file(&path).await?;
    Ok(file)
}

/// The half of [`ObjectStore::spool`] that does not need a bucket.
async fn spool_verified<R: AsyncRead + Unpin>(
    body: R,
    key: &str,
    expected: &str,
    limit: u64,
    dir: &Path,
) -> Result<VerifiedFile> {
    let mut file = anonymous_file(dir)
        .await
        .map_err(|e| StorageError::LocalFile(e.to_string()))?;
    // On a mismatch the file is dropped here, and with it the only way to
    // its bytes.
    let Some((actual, size)) = hash_through(body, Some(&mut file), limit).await? else {
        return Err(oversized(key, expected, limit));
    };
    compare(key, expected, actual)?;
    file.rewind()
        .await
        .map_err(|e| StorageError::LocalFile(e.to_string()))?;
    Ok(VerifiedFile { file, size })
}

/// Splits a stream of chunks into parts of [`PART_BYTES`], holding at most
/// one part in memory.
#[derive(Default)]
struct PartBuffer {
    buf: Vec<u8>,
}

impl PartBuffer {
    /// Add a chunk and return every part it completed.
    fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(chunk);
        let mut full = Vec::new();
        while self.buf.len() >= PART_BYTES {
            let rest = self.buf.split_off(PART_BYTES);
            full.push(std::mem::replace(&mut self.buf, rest));
        }
        full
    }

    fn head(&self) -> &[u8] {
        &self.buf[..self.buf.len().min(DETECT_BYTES)]
    }

    fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

struct Multipart {
    upload_id: String,
    parts: Vec<CompletedPart>,
}

/// One file being streamed into the bucket.
///
/// Hashes every byte as it arrives and sends each part as it fills, so a
/// 90 MB file costs one part of memory rather than the file. A file that
/// never fills a part goes up as a single `put_object`. The type is decided
/// from the first bytes before anything is sent, which is also the point at
/// which a file whose name lies about its content is refused.
///
/// A caller that stops early has to call [`ObjectUpload::abort`]: parts
/// already sent are otherwise kept by the bucket, invisible, until the sweep
/// aborts them.
pub struct ObjectUpload {
    store: ObjectStore,
    key: String,
    file_name: String,
    limit: usize,
    size: usize,
    hasher: blake3::Hasher,
    parts: PartBuffer,
    content_type: Option<&'static str>,
    multipart: Option<Multipart>,
}

impl ObjectUpload {
    pub async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        if self.size + chunk.len() > self.limit {
            return Err(StorageError::TooLargeUpload {
                name: self.file_name.clone(),
                limit_mb: self.limit / (1024 * 1024),
            });
        }
        self.size += chunk.len();
        self.hasher.update(chunk);
        for part in self.parts.push(chunk) {
            self.send_part(part).await?;
        }
        Ok(())
    }

    /// Bytes received so far.
    pub fn size(&self) -> usize {
        self.size
    }

    async fn send_part(&mut self, part: Vec<u8>) -> Result<()> {
        let client = &self.store.client;
        if self.multipart.is_none() {
            let head = &part[..part.len().min(DETECT_BYTES)];
            let content_type = detect_content_type(head, &self.file_name)?;
            self.content_type = Some(content_type);
            self.store.ensure_bucket().await?;
            let out = client
                .create_multipart_upload()
                .bucket(&self.store.bucket)
                .key(&self.key)
                .content_type(content_type)
                .send()
                .await
                .map_err(StorageError::s3("create_multipart_upload"))?;
            let upload_id = out.upload_id().unwrap_or_default().to_string();
            self.multipart = Some(Multipart {
                upload_id,
                parts: Vec::new(),
            });
        }
        let Some(multipart) = self.multipart.as_mut() else {
            return Ok(());
        };
        let number = multipart.parts.len() as i32 + 1;
        let out = client
            .upload_part()
            .bucket(&self.store.bucket)
            .key(&self.key)
            .upload_id(&multipart.upload_id)
            .part_number(number)
            .body(ByteStream::from(part))
            .send()
            .await
            .map_err(StorageError::s3("upload_part"))?;
        multipart.parts.push(
            CompletedPart::builder()
                .part_number(number)
                .set_e_tag(out.e_tag().map(str::to_string))
                .build(),
        );
        Ok(())
    }

    /// Send what is left and make the object visible.
    pub async fn finish(mut self) -> Result<StoredObject> {
        if self.size == 0 {
            return Err(StorageError::Empty {
                name: self.file_name.clone(),
            });
        }
        let checksum = self.hasher.finalize().to_hex().to_string();
        let content_type = if self.multipart.is_none() {
            let content_type = detect_content_type(self.parts.head(), &self.file_name)?;
            self.store.ensure_bucket().await?;
            self.store
                .client
                .put_object()
                .bucket(&self.store.bucket)
                .key(&self.key)
                .body(ByteStream::from(self.parts.take()))
                .content_type(content_type)
                .send()
                .await
                .map_err(StorageError::s3("put_object"))?;
            content_type
        } else {
            let rest = self.parts.take();
            if !rest.is_empty() {
                self.send_part(rest).await?;
            }
            let Some(multipart) = self.multipart.take() else {
                return Err(StorageError::S3 {
                    op: "complete_multipart_upload",
                    message: "分段上傳的狀態遺失".into(),
                });
            };
            let done = self
                .store
                .client
                .complete_multipart_upload()
                .bucket(&self.store.bucket)
                .key(&self.key)
                .upload_id(&multipart.upload_id)
                .multipart_upload(
                    CompletedMultipartUpload::builder()
                        .set_parts(Some(multipart.parts))
                        .build(),
                )
                .send()
                .await;
            if let Err(e) = done {
                self.multipart = Some(Multipart {
                    upload_id: multipart.upload_id,
                    parts: Vec::new(),
                });
                self.abort().await;
                return Err(StorageError::S3 {
                    op: "complete_multipart_upload",
                    message: format!("{e}"),
                });
            }
            self.content_type.unwrap_or("application/octet-stream")
        };
        Ok(StoredObject {
            key: self.key.clone(),
            byte_size: self.size as i64,
            content_type: content_type.to_string(),
            checksum,
        })
    }

    /// Give up: drop whatever parts were sent. Nothing to do when none were.
    pub async fn abort(self) {
        let Some(multipart) = self.multipart else {
            return;
        };
        if let Err(e) = self
            .store
            .client
            .abort_multipart_upload()
            .bucket(&self.store.bucket)
            .key(&self.key)
            .upload_id(&multipart.upload_id)
            .send()
            .await
        {
            tracing::warn!(key = %self.key, error = %e, "could not abort a multipart upload, the sweep will retry");
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

/// What the first bytes of a file say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sniffed {
    Pdf,
    Png,
    Jpeg,
    Gif,
    Webp,
    /// PK: xlsx, docx and zip all start this way.
    Zip,
    /// The OLE compound file of the older xls and doc.
    Ole,
}

fn sniff(head: &[u8]) -> Option<Sniffed> {
    if head.starts_with(b"%PDF-") {
        Some(Sniffed::Pdf)
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Sniffed::Png)
    } else if head.starts_with(b"\xff\xd8\xff") {
        Some(Sniffed::Jpeg)
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some(Sniffed::Gif)
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        Some(Sniffed::Webp)
    } else if head.starts_with(b"PK\x03\x04") {
        Some(Sniffed::Zip)
    } else if head.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        Some(Sniffed::Ole)
    } else {
        None
    }
}

/// What a file's extension promises about its bytes: one of these starts, or
/// text.
enum Promise {
    Binary(&'static [Sniffed]),
    Text,
}

fn promise(ext: &str) -> Option<Promise> {
    Some(match ext {
        "pdf" => Promise::Binary(&[Sniffed::Pdf]),
        "png" => Promise::Binary(&[Sniffed::Png]),
        "jpg" | "jpeg" => Promise::Binary(&[Sniffed::Jpeg]),
        "gif" => Promise::Binary(&[Sniffed::Gif]),
        "webp" => Promise::Binary(&[Sniffed::Webp]),
        // A password-protected xlsx or docx is not a zip: Office wraps the
        // encrypted package in an OLE compound file.
        "xlsx" | "docx" => Promise::Binary(&[Sniffed::Zip, Sniffed::Ole]),
        "zip" => Promise::Binary(&[Sniffed::Zip]),
        "xls" | "doc" => Promise::Binary(&[Sniffed::Ole]),
        "csv" | "txt" | "json" | "xml" => Promise::Text,
        _ => return None,
    })
}

/// Text as a spreadsheet or a system export writes it: no NUL byte, unless
/// the file opens with a UTF-16 byte order mark, which is how Excel saves
/// "Unicode text".
fn looks_like_text(head: &[u8]) -> bool {
    head.starts_with(b"\xff\xfe") || head.starts_with(b"\xfe\xff") || !head.contains(&0)
}

fn extension_of(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((_, ext)) => ext.to_ascii_lowercase(),
        None => String::new(),
    }
}

/// The content type a file is stored under, decided from its first bytes.
///
/// The name alone decides nothing: a type is only ever one the bytes confirm,
/// and anything the bytes do not identify is stored as
/// `application/octet-stream`, so no uploaded file is ever labelled as
/// something a browser renders, HTML and SVG included. A name that promises a
/// type the bytes contradict -- a `.pdf` that is not a PDF -- is refused
/// rather than stored under either type: it is a damaged file or a renamed
/// one, and the person holding it is the one who can tell which.
pub fn detect_content_type(head: &[u8], file_name: &str) -> Result<&'static str> {
    let ext = extension_of(file_name);
    let sniffed = sniff(head);
    let mismatch = || StorageError::ContentMismatch {
        name: file_name.to_string(),
    };
    match (promise(&ext), sniffed) {
        // Zip and OLE are containers, and the extension is what tells an xlsx
        // from a docx once the bytes have confirmed the container.
        (Some(Promise::Binary(allowed)), Some(kind)) if allowed.contains(&kind) => Ok(match kind {
            Sniffed::Zip | Sniffed::Ole => guess_content_type(file_name),
            kind => sniffed_type(kind),
        }),
        (Some(Promise::Binary(_)), _) => Err(mismatch()),
        (Some(Promise::Text), None) if looks_like_text(head) => Ok(guess_content_type(file_name)),
        (Some(Promise::Text), _) => Err(mismatch()),
        (None, sniffed) => Ok(sniffed
            .map(sniffed_type)
            .unwrap_or("application/octet-stream")),
    }
}

fn sniffed_type(kind: Sniffed) -> &'static str {
    match kind {
        Sniffed::Pdf => "application/pdf",
        Sniffed::Png => "image/png",
        Sniffed::Jpeg => "image/jpeg",
        Sniffed::Gif => "image/gif",
        Sniffed::Webp => "image/webp",
        Sniffed::Zip => "application/zip",
        // An OLE file under a name that is neither xls nor doc could be any of
        // a dozen Office formats; the bytes do not say which.
        Sniffed::Ole => "application/octet-stream",
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

    /// A 90 MB file fed in small chunks goes up as full 8 MiB parts plus a
    /// remainder, never holding more than a part, and the running hash equals
    /// the hash of the whole file.
    #[test]
    fn a_streamed_file_splits_into_parts_and_hashes_as_a_whole() {
        let file: Vec<u8> = (0..MAX_UPLOAD_BYTES).map(|i| (i % 251) as u8).collect();
        let mut parts = PartBuffer::default();
        let mut hasher = blake3::Hasher::new();
        let mut sent: Vec<Vec<u8>> = Vec::new();
        for chunk in file.chunks(64 * 1024 + 7) {
            hasher.update(chunk);
            sent.extend(parts.push(chunk));
            assert!(parts.buf.len() < PART_BYTES, "緩衝不得超過一個分段");
        }
        let rest = parts.take();

        assert_eq!(sent.len(), MAX_UPLOAD_BYTES / PART_BYTES);
        assert!(sent.iter().all(|p| p.len() == PART_BYTES));
        assert_eq!(rest.len(), MAX_UPLOAD_BYTES % PART_BYTES);
        assert_eq!(
            hasher.finalize().to_hex().to_string(),
            checksum(&file),
            "邊收邊算的雜湊要等於整份檔案的雜湊"
        );
        let joined: Vec<u8> = sent.into_iter().flatten().chain(rest).collect();
        assert!(joined == file, "分段接回去要是原本的檔案");
    }

    #[test]
    fn the_type_comes_from_the_bytes() {
        assert_eq!(
            detect_content_type(b"%PDF-1.7\n", "發票.pdf").unwrap(),
            "application/pdf"
        );
        assert_eq!(
            detect_content_type(b"\xff\xd8\xff\xe0JFIF", "收據.JPG").unwrap(),
            "image/jpeg"
        );
        assert_eq!(
            detect_content_type(b"PK\x03\x04rest", "詢價單.xlsx").unwrap(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            detect_content_type("日期,摘要\n".as_bytes(), "對帳單.csv").unwrap(),
            "text/csv; charset=utf-8"
        );
        assert_eq!(
            detect_content_type(b"\xff\xfe\x31\x00", "匯出.txt").unwrap(),
            "text/plain; charset=utf-8",
            "Excel 的 Unicode 文字是 UTF-16"
        );
        assert_eq!(
            detect_content_type(b"%PDF-1.4", "沒有副檔名").unwrap(),
            "application/pdf"
        );
    }

    /// A payroll workbook with a password is an OLE file, not a zip, and is
    /// as much an xlsx as the unprotected one.
    #[test]
    fn a_password_protected_office_file_is_accepted() {
        let ole = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1rest";
        assert_eq!(
            detect_content_type(ole, "九月薪資.xlsx").unwrap(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            detect_content_type(ole, "勞動契約.docx").unwrap(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert!(
            matches!(
                detect_content_type(ole, "封存.zip"),
                Err(StorageError::ContentMismatch { .. })
            ),
            "zip 只有 zip 一種容器"
        );
        assert!(
            matches!(
                detect_content_type(b"PK\x03\x04rest", "舊格式.xls"),
                Err(StorageError::ContentMismatch { .. })
            ),
            "xls 是 OLE，不是 zip"
        );
    }

    /// A name that promises what the bytes do not deliver is refused.
    #[test]
    fn a_renamed_file_is_refused() {
        for (head, name) in [
            (&b"just some text"[..], "假的.pdf"),
            (&b"%PDF-1.7"[..], "其實是.png"),
            (&b"\x00\x01binary"[..], "資料.csv"),
            (&b"%PDF-1.7"[..], "表格.csv"),
        ] {
            assert!(
                matches!(
                    detect_content_type(head, name),
                    Err(StorageError::ContentMismatch { .. })
                ),
                "{name} 應被拒絕"
            );
        }
    }

    /// Nothing an upload stores is labelled as a type a browser renders.
    #[test]
    fn markup_is_never_stored_as_markup() {
        for (head, name) in [
            (
                &b"<svg xmlns=\"http://www.w3.org/2000/svg\">"[..],
                "logo.svg",
            ),
            (&b"<!doctype html><script>"[..], "收據.html"),
            (&b"<html>"[..], "page.htm"),
        ] {
            assert_eq!(
                detect_content_type(head, name).unwrap(),
                "application/octet-stream",
                "{name}"
            );
        }
    }

    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "laysi-spool-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&dir).expect("應可建立測試目錄");
        dir
    }

    fn entries(dir: &Path) -> usize {
        std::fs::read_dir(dir).expect("應可讀取測試目錄").count()
    }

    /// What was spooled is what the bucket sent, and the file is gone from
    /// the directory while its handle still reads it.
    #[tokio::test]
    async fn a_spooled_download_is_the_object_and_leaves_nothing_on_disk() {
        let dir = scratch_dir();
        let object: Vec<u8> = (0..3 * READ_BYTES + 17).map(|i| (i % 251) as u8).collect();

        let mut spooled = spool_verified(
            &object[..],
            "2609/D2609-001-v1-a.pdf",
            &checksum(&object),
            object.len() as u64,
            &dir,
        )
        .await
        .expect("雜湊相符要交出檔案");
        assert_eq!(spooled.size, object.len() as u64);
        assert_eq!(entries(&dir), 0, "暫存檔建立後立刻 unlink");

        let mut back = Vec::new();
        spooled.file.read_to_end(&mut back).await.unwrap();
        assert!(back == object, "交出去的要是取回的那一份");
        std::fs::remove_dir(&dir).unwrap();
    }

    /// A tampered object yields an error and no file: there is nothing left to
    /// send.
    #[tokio::test]
    async fn a_tampered_download_hands_over_nothing() {
        let dir = scratch_dir();
        let registered = checksum(b"V2609-018 8420");
        let outcome = spool_verified(
            &b"V2609-018 8421"[..],
            "2609/D2609-002-v1-a.pdf",
            &registered,
            14,
            &dir,
        )
        .await;
        assert!(
            matches!(outcome, Err(StorageError::ChecksumMismatch { .. })),
            "雜湊不符就不交出內容"
        );
        assert_eq!(entries(&dir), 0, "不符的內容不留在磁碟上");
        std::fs::remove_dir(&dir).unwrap();
    }

    /// An object swapped for a larger one is refused once it passes the
    /// registered size, without the rest of it reaching the disk.
    #[tokio::test]
    async fn an_object_longer_than_registered_stops_at_the_registered_size() {
        let dir = scratch_dir();
        let registered = b"V2609-018 8420";
        let swapped = vec![b'x'; 4 * READ_BYTES];
        let outcome = spool_verified(
            &swapped[..],
            "2609/D2609-003-v1-a.pdf",
            &checksum(registered),
            registered.len() as u64,
            &dir,
        )
        .await;
        assert!(
            matches!(outcome, Err(StorageError::ChecksumMismatch { .. })),
            "超過登記大小就是內容不符"
        );
        assert_eq!(entries(&dir), 0);
        std::fs::remove_dir(&dir).unwrap();
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
