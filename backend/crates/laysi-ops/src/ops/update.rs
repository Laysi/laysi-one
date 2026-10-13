//! Updating this installation from the screen: asking the host-side updater
//! for the released version, and reading how it went.
//!
//! The host-side updater (`laysi-updater`) owns the update itself. This module
//! is the product's half of the contract in `laysi_license_token::update`: a
//! request goes into a directory only the api container can write, and the
//! answer comes back through a directory both api and mcp can read.
//!
//! What may be installed is whatever the licence server released to this
//! installation: a signed manifest that arrives with the licence check and is
//! kept in `license_state.release_manifest`. Nothing here trusts that column.
//! The manifest is verified again every time a version is read out of it and
//! again when a request is written, and the updater verifies it a third time
//! before acting, so a hand-edited row offers nothing and installs nothing.
//!
//! The update takes the system down, so the screen cannot wait for its own
//! response. [`GetUpdateTicket`] returns as soon as the request is on disk,
//! with the id the screen then asks [`GetUpdateResult`] about.

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use laysi_license_token::release::{self, Decision, ReleaseVersion, VerifyingKey};
use laysi_license_token::update::{
    switching_in, RequestId, UpdateRequest, UpdateResult, UpdateStatus, CONTAINER_REQUEST_DIR,
    CONTAINER_RESULT_DIR, MAX_RESULT_BYTES, REQUEST_FILE,
};
use laysi_store::LicenseState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

/// Where the screen says an update is run from. Named in the refusal an AI
/// client gets, so it has to be the title of the page.
pub const UPDATE_PLACE: &str = "安全與營運";

/// The two directories the contract uses, as this process sees them.
///
/// A value rather than constants so a test can point the registry at
/// directories of its own: the container paths do not exist on a development
/// machine, and a test that needed them would be reading the real ones.
#[derive(Debug, Clone)]
pub struct UpdateDirs {
    pub requests: PathBuf,
    pub results: PathBuf,
}

impl UpdateDirs {
    /// The container paths. Where every real installation looks.
    pub fn container() -> Self {
        Self {
            requests: PathBuf::from(CONTAINER_REQUEST_DIR),
            results: PathBuf::from(CONTAINER_RESULT_DIR),
        }
    }

    /// Whether an update is switching, which is when writes are refused. See
    /// [`switching_in`] for what happens when the directory is not there.
    pub fn switching(&self) -> bool {
        switching_in(&self.results)
    }

    /// Whether this installation has a host-side updater to ask. The requests
    /// directory is mounted only on installations that were set up with one:
    /// a development machine or a compose deployment has none, and a button
    /// there would write nowhere.
    pub fn updater_present(&self) -> bool {
        self.requests.is_dir()
    }
}

impl Default for UpdateDirs {
    fn default() -> Self {
        Self::container()
    }
}

/// Operations provided by this module, looking at the container directories.
pub fn ops() -> Vec<Box<dyn Operation>> {
    ops_in(&UpdateDirs::container())
}

pub fn ops_in(dirs: &UpdateDirs) -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetUpdateTicket { dirs: dirs.clone() }),
        Box::new(GetUpdateResult { dirs: dirs.clone() }),
    ]
}

// What can be installed

/// The version the licence server released to this installation, when
/// installing it would move this build forward.
///
/// `None` for every other case: nothing released, a manifest that does not
/// verify against `trusted`, the same version, and an older one. The screen
/// only knows whether something was released to this machine, never what the
/// newest version is, so none of those is reported as "latest".
pub fn installable_with(
    manifest: &str,
    running: &str,
    trusted: &[VerifyingKey],
) -> Option<ReleaseVersion> {
    if manifest.is_empty() {
        return None;
    }
    let verified = release::verify_with(manifest, trusted).ok()?;
    match verified.decide(running) {
        Decision::Install => Some(verified.version().clone()),
        Decision::UpToDate | Decision::Ignore(_) => None,
    }
}

/// [`installable_with`] against the keys compiled into this build.
pub fn installable(state: &LicenseState) -> Option<ReleaseVersion> {
    installable_with(
        &state.release_manifest,
        crate::ops::license::build_version(),
        &release::trusted_keys(),
    )
}

// The request

/// Write `request` into `dir` so the updater sees a complete file or none.
///
/// The contract: a temporary name other than [`REQUEST_FILE`], fsync, mode
/// 0644 so the host user can read what the container wrote, then a rename,
/// which is what starts the updater. The directory is synced afterwards so the
/// rename itself survives a power cut.
pub fn write_request(dir: &Path, request: &UpdateRequest) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let temporary = dir.join(format!(".request-{}.tmp", request.id));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(request.encode().as_bytes())?;
        file.sync_all()?;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        std::fs::rename(&temporary, dir.join(REQUEST_FILE))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    std::fs::File::open(dir)?.sync_all()
}

/// Why an update could not be asked for, in the words the screen prints.
fn request_failed(error: &std::io::Error) -> OpError {
    tracing::error!(%error, "writing the update request failed");
    OpError::BadInput("無法送出更新要求，請稍後再試；仍然失敗時請洽經銷商。".into())
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct GetUpdateTicketInput {}

struct GetUpdateTicket {
    dirs: UpdateDirs,
}

#[async_trait]
impl Operation for GetUpdateTicket {
    fn name(&self) -> &'static str {
        "get_update_ticket"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    /// Writes a file for the host, not the books, so it is not a write in the
    /// sense the licence floor and the switching floor mean. The first matters
    /// because an installation whose licence lapsed is read-only and may be
    /// exactly the one an update repairs. The name starts with a read verb for
    /// the same reason `get_download_link` does: it is what
    /// `reads_are_not_writes_whatever_floor_they_carry` goes by.
    fn writes(&self) -> bool {
        false
    }
    fn ui_only(&self) -> Option<&'static str> {
        Some(UPDATE_PLACE)
    }
    fn summary(&self) -> &'static str {
        "更新系統：要求主機把本安裝更新為授權伺服器放行的版本，回傳請求編號供查詢結果。更新期間系統暫時無法使用。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<GetUpdateTicketInput>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        if self.dirs.switching() {
            return Err(OpError::BadInput(
                "系統正在更新中，請等更新完成後再查看結果。".into(),
            ));
        }
        if !self.dirs.updater_present() {
            return Err(OpError::BadInput(
                "本安裝的更新由維運人員處理，請洽經銷商。".into(),
            ));
        }
        let state = ctx.store.license_state().await?;
        let Some(version) = installable(&state) else {
            return Err(OpError::BadInput(
                "目前沒有可安裝的版本，請重新整理本頁後再確認。".into(),
            ));
        };
        let id = RequestId::parse(&uuid::Uuid::new_v4().to_string())
            .map_err(|e| OpError::BadInput(e.to_string()))?;
        let request = UpdateRequest::new(id.clone(), &state.release_manifest)
            .map_err(|e| OpError::BadInput(e.to_string()))?;
        write_request(&self.dirs.requests, &request).map_err(|e| request_failed(&e))?;

        Ok(OpOutcome::new(
            json!({ "request_id": id.as_str(), "offered_version": version.to_string() }),
            format!("要求更新至 {version} · 請求編號 {id}"),
        ))
    }
}

// The result

/// How an update stands, in the five answers the screen tells apart.
///
/// Decided here from [`UpdateStatus`] with no wildcard arm, so a status added
/// to the contract fails to compile instead of falling into one of these by
/// accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Still running; the system may be down.
    InProgress,
    /// The released version is running.
    Succeeded,
    /// The update did not complete and the previous version is running again.
    Restored,
    /// The update did not complete and the previous version could not be
    /// brought back. Somebody has to look at the machine.
    Unrestored,
    /// The updater looked at the request and declined to start.
    Refused,
}

impl Outcome {
    pub fn of(status: UpdateStatus) -> Outcome {
        match status {
            UpdateStatus::Accepted
            | UpdateStatus::Preparing
            | UpdateStatus::Switching
            | UpdateStatus::RollingBack => Outcome::InProgress,
            UpdateStatus::Succeeded | UpdateStatus::SucceededUpdaterKept => Outcome::Succeeded,
            UpdateStatus::FailedUnchanged | UpdateStatus::RolledBack => Outcome::Restored,
            UpdateStatus::RollbackFailed => Outcome::Unrestored,
            UpdateStatus::RefusedSignature
            | UpdateStatus::RefusedDowngrade
            | UpdateStatus::RefusedSameVersion
            | UpdateStatus::RefusedInstalledUnknown
            | UpdateStatus::RefusedDiskSpace
            | UpdateStatus::RefusedBusy
            | UpdateStatus::RefusedBlocked => Outcome::Refused,
        }
    }
}

/// The result file for `id`, or `None` when there is none.
///
/// The file is named after the request id, so a request never reads another's
/// result, and the id inside is compared too: a file that says it answers
/// some other request is an error rather than an answer. No file, no
/// directory, and a directory that is not one all mean the updater has not
/// answered, which is the same to the screen and, on a development machine,
/// always true.
pub fn read_result(dir: &Path, id: &RequestId) -> Result<Option<UpdateResult>, OpError> {
    let unreadable = || OpError::BadInput("暫時讀不到更新結果，請稍後再試。".into());
    let path = dir.join(id.as_str());
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None)
        }
        Err(e) => {
            tracing::warn!(error = %e, "the update result cannot be read");
            return Err(unreadable());
        }
    };
    if !meta.is_file() || meta.len() > MAX_RESULT_BYTES as u64 {
        tracing::warn!(path = %path.display(), "the update result is not a regular file of a sane size");
        return Err(unreadable());
    }
    let mut text = String::new();
    std::fs::File::open(&path)
        .and_then(|file| {
            file.take(MAX_RESULT_BYTES as u64 + 1)
                .read_to_string(&mut text)
        })
        .map_err(|e| {
            tracing::warn!(error = %e, "reading the update result failed");
            unreadable()
        })?;
    let result = UpdateResult::decode(&text).map_err(|e| {
        tracing::warn!(error = %e, "the update result does not decode");
        unreadable()
    })?;
    if result.request_id != *id {
        tracing::warn!(file = %id, inside = %result.request_id, "the update result answers another request");
        return Err(unreadable());
    }
    Ok(Some(result))
}

/// The result as the screen reads it.
pub fn result_json(id: &RequestId, result: Option<&UpdateResult>) -> Value {
    let Some(result) = result else {
        return json!({
            "request_id": id.as_str(),
            "found": false,
            "status": null,
            "outcome": null,
            "running_version": null,
            "offered_version": null,
            "reason": null,
        });
    };
    json!({
        "request_id": id.as_str(),
        "found": true,
        "status": result.status.code(),
        "outcome": Outcome::of(result.status),
        "running_version": result.running.as_ref().map(|v| v.to_string()),
        "offered_version": result.offered.as_ref().map(|v| v.to_string()),
        "reason": result.reason,
    })
}

#[derive(Deserialize, JsonSchema)]
pub struct GetUpdateResultInput {
    /// The request id `get_update_ticket` returned. A result is only ever
    /// given for the request that asked for it.
    pub request_id: String,
}

struct GetUpdateResult {
    dirs: UpdateDirs,
}

#[async_trait]
impl Operation for GetUpdateResult {
    fn name(&self) -> &'static str {
        "get_update_result"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn writes(&self) -> bool {
        false
    }
    fn summary(&self) -> &'static str {
        "讀更新結果：依請求編號回報進行中、成功、已回復或沒有執行。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<GetUpdateResultInput>()
    }

    async fn call(&self, _ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: GetUpdateResultInput = parse(input)?;
        let id =
            RequestId::parse(i.request_id.trim()).map_err(|e| OpError::BadInput(e.to_string()))?;
        let found = read_result(&self.dirs.results, &id)?;
        let summary = match &found {
            Some(result) => format!("讀更新結果 · {} · {}", id, result.status),
            None => format!("讀更新結果 · {id} · 尚無結果"),
        };
        Ok(OpOutcome::new(result_json(&id, found.as_ref()), summary))
    }
}

/// A signed manifest for tests that need one, here and in the licence check.
#[cfg(test)]
pub(crate) mod test_support {
    use ed25519_dalek::{Signer, SigningKey};
    use laysi_license_token::release::KeyId;

    const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    pub fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn manifest(version: &str, signer: &SigningKey) -> String {
        let body = format!(
            "laysi-release-manifest 1\nversion {version}\nimage api {DIGEST}\nimage web {DIGEST}\nimage backup {DIGEST}\n"
        );
        format!(
            "{body}signature {} {}\n",
            KeyId::of(&signer.verifying_key()),
            hex(&signer.sign(body.as_bytes()).to_bytes())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{SubsecRound, Utc};

    use crate::ops::update::test_support::{key, manifest};

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("laysi-update-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("應可建立暫存目錄");
        dir
    }

    fn id(text: &str) -> RequestId {
        RequestId::parse(text).expect("測試用請求編號")
    }

    fn result_file(dir: &Path, name: &str, result: &UpdateResult) {
        std::fs::write(dir.join(name), result.encode()).expect("應可寫入結果檔");
    }

    #[test]
    fn only_a_verified_manifest_that_moves_the_build_forward_is_offered() {
        let trusted = key(7);
        let keys = [trusted.verifying_key()];

        let newer = manifest("v0.2.0", &trusted);
        let offered = installable_with(&newer, "v0.1.12", &keys).expect("較新的版本應放行");
        assert_eq!(offered.to_string(), "v0.2.0");

        assert!(installable_with("", "v0.1.12", &keys).is_none(), "沒放行");
        assert!(
            installable_with(&newer, "v0.2.0", &keys).is_none(),
            "同版不該提供安裝"
        );
        assert!(
            installable_with(&newer, "v0.3.0", &keys).is_none(),
            "較舊的清單不該提供安裝"
        );
        assert!(
            installable_with(&newer, "dev", &keys).is_none(),
            "讀不出執行中的版本時不提供安裝"
        );
        assert!(
            installable_with(&manifest("v0.2.0", &key(8)), "v0.1.12", &keys).is_none(),
            "不信任的金鑰簽的清單不該放行"
        );
        let edited = newer.replace("v0.2.0", "v9.9.9");
        assert!(
            installable_with(&edited, "v0.1.12", &keys).is_none(),
            "內容被改過的清單不該放行"
        );
    }

    #[test]
    fn a_request_appears_whole_under_the_name_that_starts_the_updater() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("request");
        let signer = key(7);
        let request = UpdateRequest::new(id("11111111-aaaa"), &manifest("v0.2.0", &signer))
            .expect("應可建立請求");
        write_request(&dir, &request).expect("應寫得進去");

        let path = dir.join(REQUEST_FILE);
        let text = std::fs::read_to_string(&path).expect("應讀得到");
        assert_eq!(UpdateRequest::decode(&text).expect("應解得開"), request);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "主機上的使用者要讀得到"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, [REQUEST_FILE], "暫存檔不該留下");

        // A second request replaces the first rather than failing: the
        // updater discards what arrives while it runs.
        let second = UpdateRequest::new(id("22222222-bbbb"), &manifest("v0.2.0", &signer)).unwrap();
        write_request(&dir, &second).expect("後一份應取代前一份");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(UpdateRequest::decode(&text).unwrap().id, second.id);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_request_into_a_missing_directory_fails_and_leaves_nothing() {
        let dir = scratch("missing").join("gone");
        let request = UpdateRequest::new(id("11111111-aaaa"), "x\n").unwrap();
        assert!(write_request(&dir, &request).is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn a_result_is_read_only_for_the_request_it_answers() {
        let dir = scratch("result");
        let now = Utc::now().trunc_subsecs(0);
        let older = UpdateResult::new(id("aaaaaaaa-0001"), UpdateStatus::Succeeded, now);
        let mut newer = UpdateResult::new(id("bbbbbbbb-0002"), UpdateStatus::RolledBack, now);
        newer.running = Some(ReleaseVersion::parse("v0.1.12").unwrap());
        newer.reason = Some("新版沒有在時限內就緒".into());
        result_file(&dir, "aaaaaaaa-0001", &older);
        result_file(&dir, "bbbbbbbb-0002", &newer);

        // The directory holds the previous run's answer. Asking about the new
        // request must not return it, whichever is newer on disk.
        assert_eq!(read_result(&dir, &id("cccccccc-0003")).unwrap(), None);
        assert_eq!(
            read_result(&dir, &id("aaaaaaaa-0001")).unwrap(),
            Some(older)
        );
        let got = read_result(&dir, &id("bbbbbbbb-0002")).unwrap().unwrap();
        assert_eq!(got.status, UpdateStatus::RolledBack);
        assert_eq!(got.request_id.as_str(), "bbbbbbbb-0002");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_directory_or_file_is_no_result_rather_than_an_error() {
        let dir = scratch("absent");
        assert_eq!(read_result(&dir, &id("aaaaaaaa-0001")).unwrap(), None);
        assert_eq!(
            read_result(&dir.join("not-there"), &id("aaaaaaaa-0001")).unwrap(),
            None
        );
        // A file where the directory should be.
        let file = dir.join("plain");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(read_result(&file, &id("aaaaaaaa-0001")).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_result_that_answers_another_request_or_is_not_a_result_is_an_error() {
        let dir = scratch("wrong");
        let now = Utc::now();

        // A file renamed to another id's name: its content says which request
        // it answered, and the name does not get to overrule that.
        let other = UpdateResult::new(id("aaaaaaaa-0001"), UpdateStatus::Succeeded, now);
        result_file(&dir, "bbbbbbbb-0002", &other);
        assert!(read_result(&dir, &id("bbbbbbbb-0002")).is_err());

        std::fs::write(dir.join("cccccccc-0003"), "not a result\n").unwrap();
        assert!(read_result(&dir, &id("cccccccc-0003")).is_err());

        std::fs::write(dir.join("dddddddd-0004"), vec![b'x'; MAX_RESULT_BYTES + 1]).unwrap();
        assert!(read_result(&dir, &id("dddddddd-0004")).is_err(), "超過上限");

        std::fs::create_dir(dir.join("eeeeeeee-0005")).unwrap();
        assert!(
            read_result(&dir, &id("eeeeeeee-0005")).is_err(),
            "不是一般檔案"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_status_lands_in_exactly_one_of_the_screens_answers() {
        use std::collections::BTreeMap;

        let mut seen: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for status in UpdateStatus::ALL {
            let outcome = serde_json::to_value(Outcome::of(status)).unwrap();
            seen.entry(outcome.as_str().unwrap().to_string())
                .or_default()
                .push(status.code());
        }
        assert_eq!(
            seen["in_progress"],
            ["accepted", "preparing", "switching", "rolling-back"]
        );
        assert_eq!(seen["succeeded"], ["succeeded", "succeeded-updater-kept"]);
        assert_eq!(seen["restored"], ["failed-unchanged", "rolled-back"]);
        assert_eq!(seen["unrestored"], ["rollback-failed"]);
        assert_eq!(seen["refused"].len(), 7);
        // The screen stops polling on anything that is not in progress.
        for status in UpdateStatus::ALL {
            assert_eq!(
                Outcome::of(status) == Outcome::InProgress,
                !status.is_final(),
                "{status} 與 is_final 不一致"
            );
        }
    }

    #[test]
    fn the_response_for_a_missing_result_says_so_without_a_status() {
        let json = result_json(&id("aaaaaaaa-0001"), None);
        assert_eq!(json["found"], false);
        assert!(json["status"].is_null() && json["outcome"].is_null());
    }

    #[test]
    fn the_response_carries_the_version_that_came_back_and_the_reason() {
        let mut result =
            UpdateResult::new(id("aaaaaaaa-0001"), UpdateStatus::RolledBack, Utc::now());
        result.running = Some(ReleaseVersion::parse("v0.1.12").unwrap());
        result.offered = Some(ReleaseVersion::parse("v0.2.0").unwrap());
        result.reason = Some("新版沒有在時限內就緒".into());
        let json = result_json(&result.request_id, Some(&result));
        assert_eq!(json["outcome"], "restored");
        assert_eq!(json["running_version"], "v0.1.12");
        assert_eq!(json["offered_version"], "v0.2.0");
        assert_eq!(json["reason"], "新版沒有在時限內就緒");
    }

    #[test]
    fn the_updater_is_present_only_where_the_requests_directory_is() {
        let dir = scratch("present");
        let here = UpdateDirs {
            requests: dir.clone(),
            results: dir.join("results"),
        };
        assert!(here.updater_present());
        let nowhere = UpdateDirs {
            requests: dir.join("requests"),
            results: dir.join("results"),
        };
        assert!(!nowhere.updater_present());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
