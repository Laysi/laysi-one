//! Update requests and results: what passes between the product and the
//! host-side updater.
//!
//! The api container asks for an update by writing a request into a directory
//! it shares with the host, and the updater answers by writing a result into
//! another directory. The requests directory is mounted into `api` only, so
//! MCP cannot ask for an update; the results directory is mounted read-only
//! into both `api` and `mcp`, which ask it [`switching`].
//!
//! ```text
//! inside the api and mcp containers            on the host (stack user)
//! /var/lib/laysi-update/requests  read-write   <root>/requests   (:U, api only)
//! /var/lib/laysi-update/results   read-only    <root>/results    (:ro, api and mcp)
//! ```
//!
//! Writing a request: write the encoded [`UpdateRequest`] to a temporary name
//! in the requests directory (any name other than [`REQUEST_FILE`]), fsync it,
//! give it mode 0644 and rename it to [`REQUEST_FILE`]. The rename is what
//! starts the updater. A request written while an update is running is
//! discarded, answered with [`UpdateStatus::RefusedBusy`] when the updater can
//! still read it.
//!
//! Reading a result: the file named after the request id in the results
//! directory, decoded with [`UpdateResult::decode`]. It is replaced, never
//! edited, as the update moves on, and its status is final once
//! [`UpdateStatus::is_final`] says so. The updater keeps the newest
//! [`KEEP_RESULTS`] result files.
//!
//! Refusing writes while an update switches: [`SWITCHING_FILE`] exists in the
//! results directory from before the services stop until the final result
//! is written. Anything api or mcp writes in that span can be taken back by
//! a rollback, which restores the database as it was when the services
//! stopped, so both refuse writes while [`switching`] is true. It needs no
//! request id: a service started by the updater itself, or one that never
//! asked for the update, sees it the same way.
//!
//! Telling the screen the previous updater started the stack:
//! [`FALLBACK_BOOT_FILE`] exists in the results directory from the moment
//! the fallback unit has the previous updater start the stack until the
//! current updater next starts it without failing. Readers only ask
//! [`fallback_boot_in`].
//!
//! Both files are line text like the release manifest rather than JSON: the
//! manifest inside a request is carried byte for byte, and this crate stays
//! free of dependencies the host-side updater would have to link.
//!
//! The updater treats a request as untrusted input. It opens it without
//! following symlinks and without blocking, reads at most
//! [`MAX_REQUEST_BYTES`], and acts only on a manifest that verifies. The
//! request id is not covered by the signature; it only pairs a result with the
//! request that asked for it.

use std::fmt;
use std::io::ErrorKind;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::release::{ReleaseVersion, MAX_MANIFEST_BYTES};

/// The requests directory as the api container sees it.
pub const CONTAINER_REQUEST_DIR: &str = "/var/lib/laysi-update/requests";

/// The results directory as the api and mcp containers see it. Mounted
/// read-only.
pub const CONTAINER_RESULT_DIR: &str = "/var/lib/laysi-update/results";

/// Present in the results directory while an update is switching: written,
/// durably, before the updater stops the first service (step 4), and
/// removed only once the final result is written. A run that finds no
/// update in progress removes one left behind, so a power cut or a killed
/// run cannot leave the product refusing writes; until that run, which the
/// boot unit and the recovery unit both are, it stays.
///
/// It contains the request id, for whoever looks; readers only ask whether
/// it exists. The dot keeps it from ever being a request id, so no result is
/// named this and the pruning of old results never touches it.
pub const SWITCHING_FILE: &str = "switching.marker";

/// The name a finished request is renamed to. Only this name starts an update.
pub const REQUEST_FILE: &str = "request";

/// How many result files the updater keeps, newest first.
pub const KEEP_RESULTS: usize = 50;

const REQUEST_HEADER: &str = "laysi-update-request 1";
const RESULT_HEADER: &str = "laysi-update-result 1";

/// Header, request line and the largest manifest anything reads.
pub const MAX_REQUEST_BYTES: usize =
    REQUEST_HEADER.len() + 1 + 8 + MAX_REQUEST_ID_LEN + 1 + MAX_MANIFEST_BYTES;

/// The largest result file anything should read.
pub const MAX_RESULT_BYTES: usize = 8 * 1024;

/// A reason longer than this is cut, so a result always fits
/// [`MAX_RESULT_BYTES`].
pub const MAX_REASON_CHARS: usize = 600;

/// The longest tool version a result records; longer ones are cut.
pub const MAX_TOOL_VERSION_CHARS: usize = 40;

const MIN_REQUEST_ID_LEN: usize = 8;
const MAX_REQUEST_ID_LEN: usize = 64;

/// Whether an update is switching, judged from a results directory: true
/// while [`SWITCHING_FILE`] is in `dir`. One `lstat`, cheap enough to ask on
/// every write.
///
/// False when the file is not there, and when `dir` itself is missing: an
/// install without the host-side updater, a development one for instance,
/// never switches. Any other error, such as a directory that cannot be
/// searched or a file where the directory should be, counts as switching. A
/// refused write can be made again; one the rollback takes back is lost
/// without anybody being told.
pub fn switching_in(dir: &Path) -> bool {
    match std::fs::symlink_metadata(dir.join(SWITCHING_FILE)) {
        Ok(_) => true,
        Err(e) => e.kind() != ErrorKind::NotFound,
    }
}

/// [`switching_in`] the results directory as api and mcp see it,
/// [`CONTAINER_RESULT_DIR`]. Writes are refused while this is true.
pub fn switching() -> bool {
    switching_in(Path::new(CONTAINER_RESULT_DIR))
}

/// Present in the results directory once the previous updater has started
/// the stack: the fallback unit runs it when the boot or recovery unit, which
/// run the current updater, failed. Written as that start begins, and removed
/// when the current updater next starts the stack without failing. The system
/// runs, but on an updater an update would replace, so the screen says the
/// current one needs looking at.
///
/// It contains the time it was written, for whoever looks; readers only ask
/// whether it is there. The dot keeps it from ever being a request id, as
/// with [`SWITCHING_FILE`].
pub const FALLBACK_BOOT_FILE: &str = "fallback-boot.marker";

/// Whether the previous updater started the stack last, judged from a results
/// directory: [`FALLBACK_BOOT_FILE`] is a regular file in `dir`. Any error, a
/// missing directory included, is false: all it decides is whether the screen
/// shows a notice.
pub fn fallback_boot_in(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(FALLBACK_BOOT_FILE)).is_ok_and(|m| m.is_file())
}

/// Names one request and the result that answers it. Also the result's file
/// name, so it is restricted to ASCII letters, digits and `-`: no dot, no
/// slash, nothing a path could be built from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// 8 to 64 characters of `A-Z`, `a-z`, `0-9` and `-`. A UUID qualifies.
    pub fn parse(text: &str) -> Result<RequestId, RequestError> {
        let ok = (MIN_REQUEST_ID_LEN..=MAX_REQUEST_ID_LEN).contains(&text.len())
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if ok {
            Ok(RequestId(text.to_string()))
        } else {
            Err(RequestError::RequestId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the api writes to ask for an update: a request id of its own and the
/// signed release manifest it received from the licence server, unchanged.
///
/// ```text
/// laysi-update-request 1
/// request <request id>
/// <the manifest, byte for byte>
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRequest {
    pub id: RequestId,
    /// The manifest text exactly as it was signed. Not verified here: the
    /// updater verifies it, against the keys it was built with.
    pub manifest: String,
}

impl UpdateRequest {
    pub fn new(id: RequestId, manifest: &str) -> Result<UpdateRequest, RequestError> {
        if manifest.len() > MAX_MANIFEST_BYTES {
            return Err(RequestError::TooLarge {
                len: manifest.len(),
            });
        }
        Ok(UpdateRequest {
            id,
            manifest: manifest.to_string(),
        })
    }

    pub fn encode(&self) -> String {
        format!("{REQUEST_HEADER}\nrequest {}\n{}", self.id, self.manifest)
    }

    pub fn decode(text: &str) -> Result<UpdateRequest, RequestError> {
        if text.len() > MAX_REQUEST_BYTES {
            return Err(RequestError::TooLarge { len: text.len() });
        }
        let rest = text
            .strip_prefix(REQUEST_HEADER)
            .and_then(|rest| rest.strip_prefix('\n'))
            .ok_or(RequestError::Malformed)?;
        let (line, manifest) = rest.split_once('\n').ok_or(RequestError::Malformed)?;
        let id = line
            .strip_prefix("request ")
            .ok_or(RequestError::Malformed)?;
        UpdateRequest::new(RequestId::parse(id)?, manifest)
    }
}

/// A request that cannot be read. The updater cannot answer it: without a
/// request id there is no result file to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    TooLarge { len: usize },
    Malformed,
    RequestId,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestError::TooLarge { len } => write!(f, "更新指示有 {len} 位元組，超過上限"),
            RequestError::Malformed => f.write_str("更新指示格式不符"),
            RequestError::RequestId => f.write_str("更新指示的請求編號格式不符"),
        }
    }
}

impl std::error::Error for RequestError {}

/// Where an update stands. The first four are written while it runs; every
/// other status is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateStatus {
    /// The request was read; nothing has been checked yet.
    Accepted,
    /// The manifest verified and is newer. Checking space and pulling images;
    /// the system is still serving.
    Preparing,
    /// Services are stopped from here until the result is final. The
    /// downtime starts at `downtime_from`.
    Switching,
    /// The new version did not come up; returning to the previous one.
    RollingBack,
    /// The new version is running and the updater was replaced with its own.
    Succeeded,
    /// The new version is running, but its updater failed its self-check, so
    /// the previous updater stays in place. `reason` says why.
    SucceededUpdaterKept,
    /// Failed before anything was stopped. Nothing changed.
    FailedUnchanged,
    /// Failed after services were stopped. The previous version is running
    /// again on the database as it was before the update.
    RolledBack,
    /// The previous version did not come back either. The machine stays on
    /// it and does not try again; both reasons are recorded.
    RollbackFailed,
    /// The manifest did not verify.
    RefusedSignature,
    /// The manifest names an older release than the one installed.
    RefusedDowngrade,
    /// The manifest names the release already installed.
    RefusedSameVersion,
    /// The installed release could not be read.
    RefusedInstalledUnknown,
    /// Not enough disk for the new images and a database backup.
    RefusedDiskSpace,
    /// Another update or the start-up recovery was running.
    RefusedBusy,
    /// A previous rollback left its database backup behind. Until somebody
    /// has looked at it, no update starts.
    RefusedBlocked,
}

impl UpdateStatus {
    pub const ALL: [UpdateStatus; 16] = [
        UpdateStatus::Accepted,
        UpdateStatus::Preparing,
        UpdateStatus::Switching,
        UpdateStatus::RollingBack,
        UpdateStatus::Succeeded,
        UpdateStatus::SucceededUpdaterKept,
        UpdateStatus::FailedUnchanged,
        UpdateStatus::RolledBack,
        UpdateStatus::RollbackFailed,
        UpdateStatus::RefusedSignature,
        UpdateStatus::RefusedDowngrade,
        UpdateStatus::RefusedSameVersion,
        UpdateStatus::RefusedInstalledUnknown,
        UpdateStatus::RefusedDiskSpace,
        UpdateStatus::RefusedBusy,
        UpdateStatus::RefusedBlocked,
    ];

    /// The word in the result file.
    pub fn code(self) -> &'static str {
        match self {
            UpdateStatus::Accepted => "accepted",
            UpdateStatus::Preparing => "preparing",
            UpdateStatus::Switching => "switching",
            UpdateStatus::RollingBack => "rolling-back",
            UpdateStatus::Succeeded => "succeeded",
            UpdateStatus::SucceededUpdaterKept => "succeeded-updater-kept",
            UpdateStatus::FailedUnchanged => "failed-unchanged",
            UpdateStatus::RolledBack => "rolled-back",
            UpdateStatus::RollbackFailed => "rollback-failed",
            UpdateStatus::RefusedSignature => "refused-signature",
            UpdateStatus::RefusedDowngrade => "refused-downgrade",
            UpdateStatus::RefusedSameVersion => "refused-same-version",
            UpdateStatus::RefusedInstalledUnknown => "refused-installed-unknown",
            UpdateStatus::RefusedDiskSpace => "refused-disk-space",
            UpdateStatus::RefusedBusy => "refused-busy",
            UpdateStatus::RefusedBlocked => "refused-blocked",
        }
    }

    pub fn from_code(code: &str) -> Option<UpdateStatus> {
        UpdateStatus::ALL.into_iter().find(|s| s.code() == code)
    }

    /// Whether the updater is done with the request.
    pub fn is_final(self) -> bool {
        !matches!(
            self,
            UpdateStatus::Accepted
                | UpdateStatus::Preparing
                | UpdateStatus::Switching
                | UpdateStatus::RollingBack
        )
    }

    /// Whether the release the request offered is the one running now.
    pub fn installed_offer(self) -> bool {
        matches!(
            self,
            UpdateStatus::Succeeded | UpdateStatus::SucceededUpdaterKept
        )
    }
}

impl fmt::Display for UpdateStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// What the updater writes back for one request.
///
/// ```text
/// laysi-update-result 1
/// request <request id>
/// status <status code>
/// received <RFC 3339, UTC>
/// updated <RFC 3339, UTC>
/// offered <version>             optional: the manifest's version
/// running <version>             optional: the release the machine is on
/// downtime-from <RFC 3339>      optional: services stopped (step 4)
/// downtime-to <RFC 3339>        optional: every service up again
/// reason <one line>             optional
/// rollback-reason <one line>    optional: why returning also failed
/// tools podman=<v> podman-compose=<v>   optional: what the run used
/// ```
///
/// `running` is absent while the switch is under way. After
/// `rollback-failed` it names the release the stack files were put back to,
/// which did not come up, and is absent when even that could not be done.
/// `reason` says why the update failed or was refused; after
/// `succeeded-updater-kept` it says why the new updater was not installed.
/// After `rolled-back`, `rollback-reason` is present when the first restore
/// of the database failed and the one retry succeeded, and says why the
/// first one failed; after a `rollback-failed` that `retry-rollback` brought
/// back, it says so and why the restores failed before that, newest first.
///
/// A reader skips keys it does not know, so a later updater can add one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResult {
    pub request_id: RequestId,
    pub status: UpdateStatus,
    pub received_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub offered: Option<ReleaseVersion>,
    pub running: Option<ReleaseVersion>,
    pub downtime_from: Option<DateTime<Utc>>,
    pub downtime_to: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub rollback_reason: Option<String>,
    pub tools: Option<ToolVersions>,
}

impl UpdateResult {
    /// A result with only the required fields.
    pub fn new(request_id: RequestId, status: UpdateStatus, at: DateTime<Utc>) -> UpdateResult {
        UpdateResult {
            request_id,
            status,
            received_at: at,
            updated_at: at,
            offered: None,
            running: None,
            downtime_from: None,
            downtime_to: None,
            reason: None,
            rollback_reason: None,
            tools: None,
        }
    }

    pub fn encode(&self) -> String {
        let mut out = format!(
            "{RESULT_HEADER}\nrequest {}\nstatus {}\nreceived {}\nupdated {}\n",
            self.request_id,
            self.status,
            timestamp(&self.received_at),
            timestamp(&self.updated_at)
        );
        let mut line = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                out.push_str(key);
                out.push(' ');
                out.push_str(&value);
                out.push('\n');
            }
        };
        line("offered", self.offered.as_ref().map(|v| v.to_string()));
        line("running", self.running.as_ref().map(|v| v.to_string()));
        line("downtime-from", self.downtime_from.as_ref().map(timestamp));
        line("downtime-to", self.downtime_to.as_ref().map(timestamp));
        line("reason", self.reason.as_deref().map(one_line));
        line(
            "rollback-reason",
            self.rollback_reason.as_deref().map(one_line),
        );
        line("tools", self.tools.as_ref().map(ToolVersions::encode));
        out
    }

    pub fn decode(text: &str) -> Result<UpdateResult, ResultError> {
        if text.len() > MAX_RESULT_BYTES {
            return Err(ResultError::TooLarge { len: text.len() });
        }
        let mut lines = text.split_inclusive('\n').enumerate();
        let mut next_line = || {
            lines.next().map(|(i, piece)| {
                piece
                    .strip_suffix('\n')
                    .filter(|l| !l.is_empty() && !l.contains('\r'))
                    .ok_or(ResultError::Malformed { line: i + 1 })
            })
        };
        if next_line().transpose()? != Some(RESULT_HEADER) {
            return Err(ResultError::Malformed { line: 1 });
        }

        let mut fields: Vec<(&str, &str)> = Vec::new();
        let mut number = 1;
        while let Some(line) = next_line() {
            number += 1;
            let line = line?;
            let (key, value) = line
                .split_once(' ')
                .ok_or(ResultError::Malformed { line: number })?;
            if fields.iter().any(|(k, _)| *k == key) {
                return Err(ResultError::Malformed { line: number });
            }
            fields.push((key, value));
        }
        let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let required = |key: &'static str| get(key).ok_or(ResultError::Missing(key));
        let time = |key: &'static str, value: &str| {
            DateTime::parse_from_rfc3339(value)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| ResultError::Field(key))
        };
        let version = |key: &'static str| {
            get(key)
                .map(|v| ReleaseVersion::parse(v).map_err(|_| ResultError::Field(key)))
                .transpose()
        };
        let optional_time = |key: &'static str| get(key).map(|v| time(key, v)).transpose();

        Ok(UpdateResult {
            request_id: RequestId::parse(required("request")?)
                .map_err(|_| ResultError::Field("request"))?,
            status: UpdateStatus::from_code(required("status")?)
                .ok_or(ResultError::Field("status"))?,
            received_at: time("received", required("received")?)?,
            updated_at: time("updated", required("updated")?)?,
            offered: version("offered")?,
            running: version("running")?,
            downtime_from: optional_time("downtime-from")?,
            downtime_to: optional_time("downtime-to")?,
            reason: get("reason").map(str::to_string),
            rollback_reason: get("rollback-reason").map(str::to_string),
            tools: get("tools")
                .map(|v| ToolVersions::decode(v).ok_or(ResultError::Field("tools")))
                .transpose()?,
        })
    }
}

/// The podman and podman-compose a run used, as they report themselves.
/// Recorded in every result and never checked after `install`: a host whose
/// tools were upgraded later must still be able to update, and only a newer
/// updater could accept a newer tool, which is the updater it could not
/// install if this one refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolVersions {
    pub podman: String,
    pub podman_compose: String,
}

impl ToolVersions {
    /// What was reported, kept to printable ASCII without spaces or `=` and
    /// at most [`MAX_TOOL_VERSION_CHARS`]; nothing left is `unknown`.
    pub fn new(podman: &str, podman_compose: &str) -> ToolVersions {
        ToolVersions {
            podman: tool_version(podman),
            podman_compose: tool_version(podman_compose),
        }
    }

    fn encode(&self) -> String {
        format!(
            "podman={} podman-compose={}",
            tool_version(&self.podman),
            tool_version(&self.podman_compose)
        )
    }

    /// `key=value` pairs; a key it does not know is skipped, so a later
    /// updater can record another tool.
    fn decode(text: &str) -> Option<ToolVersions> {
        let mut podman = None;
        let mut podman_compose = None;
        for pair in text.split(' ') {
            let (key, value) = pair.split_once('=')?;
            if value.is_empty() || value.contains('=') {
                return None;
            }
            let slot = match key {
                "podman" => &mut podman,
                "podman-compose" => &mut podman_compose,
                _ => continue,
            };
            if slot.replace(value.to_string()).is_some() {
                return None;
            }
        }
        Some(ToolVersions {
            podman: podman?,
            podman_compose: podman_compose?,
        })
    }
}

fn tool_version(text: &str) -> String {
    let kept: String = text
        .trim()
        .chars()
        .filter(|c| c.is_ascii_graphic() && *c != '=')
        .take(MAX_TOOL_VERSION_CHARS)
        .collect();
    if kept.is_empty() {
        "unknown".to_string()
    } else {
        kept
    }
}

/// A result file that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultError {
    TooLarge { len: usize },
    Malformed { line: usize },
    Missing(&'static str),
    Field(&'static str),
}

impl fmt::Display for ResultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResultError::TooLarge { len } => write!(f, "更新結果有 {len} 位元組，超過上限"),
            ResultError::Malformed { line } => write!(f, "更新結果第 {line} 行格式不符"),
            ResultError::Missing(key) => write!(f, "更新結果缺少 {key}"),
            ResultError::Field(key) => write!(f, "更新結果的 {key} 格式不符"),
        }
    }
}

impl std::error::Error for ResultError {}

fn timestamp(at: &DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// One line, at most [`MAX_REASON_CHARS`] characters: line breaks and other
/// control characters become spaces.
fn one_line(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_REASON_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "-".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const MANIFEST: &str = include_str!("../testdata/release-manifest.txt");

    fn id() -> RequestId {
        RequestId::parse("4d1c2a8e-0b7f-4c55-9a51-0f3c8e7b2a10").unwrap()
    }

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 12, 9, minute, 0).unwrap()
    }

    #[test]
    fn a_request_carries_the_manifest_byte_for_byte() {
        let request = UpdateRequest::new(id(), MANIFEST).unwrap();
        let text = request.encode();
        assert!(text.starts_with("laysi-update-request 1\nrequest 4d1c2a8e-"));
        let back = UpdateRequest::decode(&text).unwrap();
        assert_eq!(back, request);
        assert_eq!(back.manifest.as_bytes(), MANIFEST.as_bytes());
        assert!(text.len() <= MAX_REQUEST_BYTES);
    }

    #[test]
    fn the_largest_manifest_fits_in_a_request() {
        let manifest = "x".repeat(MAX_MANIFEST_BYTES);
        let id = RequestId::parse(&"a".repeat(MAX_REQUEST_ID_LEN)).unwrap();
        let text = UpdateRequest::new(id, &manifest).unwrap().encode();
        assert_eq!(text.len(), MAX_REQUEST_BYTES);
        assert!(UpdateRequest::decode(&text).is_ok());
        assert_eq!(
            UpdateRequest::decode(&format!("{text}x")),
            Err(RequestError::TooLarge {
                len: MAX_REQUEST_BYTES + 1
            })
        );
    }

    #[test]
    fn malformed_requests_are_refused() {
        let good = UpdateRequest::new(id(), MANIFEST).unwrap().encode();
        for text in [
            String::new(),
            good.replace("laysi-update-request 1", "laysi-update-request 2"),
            good.replace("\nrequest ", "\nrequest  "),
            good.replace("\nrequest ", "\nid "),
            "laysi-update-request 1\nrequest 4d1c2a8e-0b7f".to_string(),
            good.replacen('\n', "\r\n", 1),
        ] {
            assert!(UpdateRequest::decode(&text).is_err(), "{text:?}");
        }
        assert_eq!(
            UpdateRequest::new(id(), &"x".repeat(MAX_MANIFEST_BYTES + 1)),
            Err(RequestError::TooLarge {
                len: MAX_MANIFEST_BYTES + 1
            })
        );
    }

    #[test]
    fn request_ids_cannot_name_a_path() {
        for bad in [
            "",
            "short",
            "../../../etc/passwd",
            "abcdefgh/ij",
            "abcdefgh.ij",
            "abcdefgh ij",
            "abcdefgh\nij",
            "請求編號一二三四五六七八",
            &"a".repeat(MAX_REQUEST_ID_LEN + 1),
        ] {
            assert_eq!(
                RequestId::parse(bad),
                Err(RequestError::RequestId),
                "{bad:?}"
            );
        }
        for good in [
            "abcdefgh",
            "4d1c2a8e-0b7f-4c55-9a51-0f3c8e7b2a10",
            "ABC-123-xyz",
        ] {
            assert_eq!(RequestId::parse(good).unwrap().as_str(), good);
        }
    }

    #[test]
    fn a_result_round_trips() {
        let mut result = UpdateResult::new(id(), UpdateStatus::RollbackFailed, at(0));
        result.updated_at = at(9);
        result.offered = Some(ReleaseVersion::parse("v0.1.14").unwrap());
        result.running = Some(ReleaseVersion::parse("v0.1.13").unwrap());
        result.downtime_from = Some(at(2));
        result.downtime_to = Some(at(8));
        result.reason = Some("api 在期限內沒有通過健康檢查".into());
        result.rollback_reason = Some("回退後 web 沒有啟動".into());
        result.tools = Some(ToolVersions::new("4.9.3", "1.6.0"));
        let text = result.encode();
        assert_eq!(UpdateResult::decode(&text).unwrap(), result);
        assert!(text.contains("\nstatus rollback-failed\n"));
        assert!(text.ends_with("\ntools podman=4.9.3 podman-compose=1.6.0\n"));
        assert!(text.contains("\ndowntime-from 2026-10-12T09:02:00Z\n"));

        let bare = UpdateResult::new(id(), UpdateStatus::Accepted, at(0));
        assert_eq!(UpdateResult::decode(&bare.encode()).unwrap(), bare);
    }

    #[test]
    fn reasons_stay_on_one_line_and_within_the_limit() {
        let mut result = UpdateResult::new(id(), UpdateStatus::RolledBack, at(0));
        result.reason = Some(format!("第一行\n第二行\r\t{}", "長".repeat(5000)));
        let text = result.encode();
        assert!(text.len() <= MAX_RESULT_BYTES);
        let back = UpdateResult::decode(&text).unwrap();
        let reason = back.reason.unwrap();
        assert!(reason.starts_with("第一行 第二行"));
        assert_eq!(reason.chars().count(), MAX_REASON_CHARS);

        result.reason = Some("\n".into());
        assert_eq!(
            UpdateResult::decode(&result.encode())
                .unwrap()
                .reason
                .as_deref(),
            Some("-")
        );
    }

    #[test]
    fn unknown_keys_are_skipped_and_known_ones_checked() {
        let text = UpdateResult::new(id(), UpdateStatus::Succeeded, at(0)).encode();
        let later = format!("{text}phase extracting\n");
        assert!(UpdateResult::decode(&later).is_ok());

        let twice = format!("{text}status failed-unchanged\n");
        assert!(matches!(
            UpdateResult::decode(&twice),
            Err(ResultError::Malformed { .. })
        ));
        assert_eq!(
            UpdateResult::decode(&text.replace("succeeded", "done")),
            Err(ResultError::Field("status"))
        );
        assert_eq!(
            UpdateResult::decode(&text.replace("\nupdated ", "\nmodified ")),
            Err(ResultError::Missing("updated"))
        );
        assert_eq!(
            UpdateResult::decode(&format!("{text}running 0.1.13\n")),
            Err(ResultError::Field("running"))
        );
        assert!(UpdateResult::decode(text.trim_end()).is_err());
        assert!(UpdateResult::decode(&text.replace("laysi-update-result 1", "x")).is_err());
    }

    #[test]
    fn status_codes_are_distinct_and_round_trip() {
        let mut codes: Vec<_> = UpdateStatus::ALL.iter().map(|s| s.code()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), UpdateStatus::ALL.len());
        for status in UpdateStatus::ALL {
            assert_eq!(UpdateStatus::from_code(status.code()), Some(status));
        }
        let running: Vec<_> = UpdateStatus::ALL
            .into_iter()
            .filter(|s| !s.is_final())
            .collect();
        assert_eq!(
            running,
            [
                UpdateStatus::Accepted,
                UpdateStatus::Preparing,
                UpdateStatus::Switching,
                UpdateStatus::RollingBack
            ]
        );
    }

    #[test]
    fn tool_versions_round_trip_and_stay_one_token_each() {
        let mut result = UpdateResult::new(id(), UpdateStatus::Succeeded, at(0));
        result.tools = Some(ToolVersions::new(
            " 5.4.0-dev (rc 1)\n",
            &format!("1.6.0={}", "9".repeat(100)),
        ));
        let tools = result.tools.clone().unwrap();
        assert_eq!(tools.podman, "5.4.0-dev(rc1)");
        assert_eq!(tools.podman_compose.len(), MAX_TOOL_VERSION_CHARS);
        assert!(tools.podman_compose.starts_with("1.6.0999"));
        assert_eq!(ToolVersions::new("", "\t").podman, "unknown");
        let back = UpdateResult::decode(&result.encode()).unwrap();
        assert_eq!(back, result);

        let text = UpdateResult::new(id(), UpdateStatus::Succeeded, at(0)).encode();
        let decoded = |line: &str| UpdateResult::decode(&format!("{text}tools {line}\n"));
        let later = decoded("podman=4.9.3 buildah=1.33 podman-compose=1.6.0").unwrap();
        assert_eq!(later.tools.unwrap().podman_compose, "1.6.0");
        for bad in [
            "podman=4.9.3",
            "podman=4.9.3 podman-compose=",
            "podman=4.9.3 podman=5.0 podman-compose=1.6.0",
            "podman 4.9.3 podman-compose 1.6.0",
            "podman=4.9.3  podman-compose=1.6.0",
        ] {
            assert_eq!(decoded(bad), Err(ResultError::Field("tools")), "{bad}");
        }
        assert_eq!(UpdateResult::decode(&text).unwrap().tools, None);
    }

    #[test]
    fn the_largest_result_fits() {
        let mut result = UpdateResult::new(
            RequestId::parse(&"a".repeat(MAX_REQUEST_ID_LEN)).unwrap(),
            UpdateStatus::SucceededUpdaterKept,
            at(0),
        );
        result.offered = Some(ReleaseVersion::parse("v999.999.999").unwrap());
        result.running = Some(ReleaseVersion::parse("v999.999.998").unwrap());
        result.downtime_from = Some(at(1));
        result.downtime_to = Some(at(2));
        // Four bytes each, the most a char takes.
        result.reason = Some("\u{20000}".repeat(MAX_REASON_CHARS + 10));
        result.rollback_reason = result.reason.clone();
        let long = "x".repeat(MAX_TOOL_VERSION_CHARS + 10);
        result.tools = Some(ToolVersions::new(&long, &long));
        let text = result.encode();
        assert!(text.len() <= MAX_RESULT_BYTES, "{} bytes", text.len());
        assert!(UpdateResult::decode(&text).is_ok());
    }

    #[test]
    fn switching_is_the_marker_in_the_results_directory() {
        let dir =
            std::env::temp_dir().join(format!("laysi-update-switching-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // No results directory at all: an install without the updater.
        assert!(!switching_in(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!switching_in(&dir));
        let marker = dir.join(SWITCHING_FILE);
        std::fs::write(&marker, "request abcdefgh\n").unwrap();
        assert!(switching_in(&dir));
        // Whatever the entry is, its presence is the answer.
        std::fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink("/nonexistent", &marker).unwrap();
        assert!(switching_in(&dir));
        std::fs::remove_file(&marker).unwrap();
        assert!(!switching_in(&dir));
        // A file where the directory should be is a broken mount, not an
        // install without the updater: writes stop.
        let file = dir.join("plain");
        std::fs::write(&file, "").unwrap();
        assert!(switching_in(&file));
        std::fs::remove_dir_all(&dir).unwrap();

        // Never a result's name, so never pruned or overwritten as one.
        assert!(RequestId::parse(SWITCHING_FILE).is_err());
        assert!(!switching());
    }

    #[test]
    fn a_fallback_boot_is_a_marker_file_in_the_results_directory() {
        let dir =
            std::env::temp_dir().join(format!("laysi-update-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!fallback_boot_in(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!fallback_boot_in(&dir));
        let marker = dir.join(FALLBACK_BOOT_FILE);
        std::fs::write(&marker, "at 2026-10-15T06:00:00Z\n").unwrap();
        assert!(fallback_boot_in(&dir));
        // Only a regular file counts.
        std::fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", &marker).unwrap();
        assert!(!fallback_boot_in(&dir));
        std::fs::remove_file(&marker).unwrap();
        std::fs::create_dir(&marker).unwrap();
        assert!(!fallback_boot_in(&dir));
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(RequestId::parse(FALLBACK_BOOT_FILE).is_err());
    }
}
