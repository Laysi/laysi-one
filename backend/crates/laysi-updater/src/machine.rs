//! The update: from a request to a final result, driven by the journal.
//!
//! The numbered steps are the ones in the issue that specified this (#61):
//!
//! 1. Accept the request, verify the manifest, refuse a downgrade or the
//!    same version.
//! 2. Check disk space.
//! 3. Pull by digest and take the compose files and the next updater out of
//!    the new api image. Nothing has changed for the running system up to
//!    here.
//! 4. Stop everything but postgres. Downtime starts.
//! 5. Dump the database.
//! 6. Swap the compose files and `.env`, keeping the previous copies.
//! 7. Run `secrets` and recreate every long-running service but web and mcp
//!    from the new files.
//! 8. Wait for them; only then start web and mcp, and wait again. Until web
//!    and mcp start nobody can reach the new version, and once they have,
//!    the switching marker tells api and mcp to refuse writes until the
//!    result is final.
//! 9. On failure: put the files back, recreate postgres from the old image,
//!    restore the dump, start the old version and judge it the same way. A
//!    restore that fails is tried once more in the same run, after the new
//!    release's containers and images are removed and the old postgres is
//!    recreated.
//! 10. On success: replace this updater with the new one when it passes its
//!     self-check, and delete images other than the current and the previous
//!     release.
//!
//! Every way out writes a result for the request id.
//!
//! From before step 4 until the final result is written, the results
//! directory also holds [`SWITCHING_FILE`], which tells api and mcp to refuse
//! writes: a rollback restores the database as it was at step 5, and would
//! take back anything written in between.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use laysi_license_token::release::{self, Decision, Image, ImageDigest, Refusal, VerifyingKey};
use laysi_license_token::update::{
    RequestId, ToolVersions, UpdateRequest, UpdateResult, UpdateStatus, KEEP_RESULTS,
    MAX_REQUEST_BYTES, MAX_RESULT_BYTES, REQUEST_FILE, SWITCHING_FILE,
};

use crate::envfile::{self, EnvFile};
use crate::fsx::{self, Lock};
use crate::host::{Container, Host, Service, StackFiles};
use crate::journal::{self, Journal, Phase, Release};
use crate::layout::{Layout, COMPOSE_FILES, IMAGE_SHARE, IMAGE_UPDATER};

/// The one-shot service that writes the secrets volume. Run to completion
/// before anything else starts.
pub const SECRETS: &str = "secrets";
/// The database, which stays up while the rest is stopped and is what gets
/// dumped and restored.
pub const DATABASE: &str = "postgres";
/// The service web proxies to, and the one that writes update requests.
pub const API: &str = "api";
/// The object store. Its data is not part of a rollback.
pub const OBJECT_STORE: &str = "rustfs";
/// Which of the release's images each of its services runs.
pub const ROLES: [(&str, Image); 4] = [
    (API, Image::Api),
    ("mcp", Image::Api),
    ("web", Image::Web),
    ("backup", Image::Backup),
];
/// The services anybody can reach the system through. They start only after
/// everything else has passed.
pub const ENTRIES: [&str; 2] = ["web", "mcp"];

const INTERRUPTED: &str = "更新途中主機重新啟動或更新程式中斷";
/// Ends every rollback failure from the restore on: the dump is not
/// deleted until a restore has succeeded.
const BACKUP_KEPT: &str = "備份保留在主機上";
const NOT_STARTED: &str = "更新程式在開始更新前中斷，系統沒有任何變更";
/// How many times the size of the running release's images a new release
/// is assumed to need before it has been pulled.
const RELEASE_GROWTH: u64 = 2;
/// Restarts after which a container counts as crashing rather than slow.
const CRASH_LOOP_RESTARTS: u32 = 5;
const MAX_COMPOSE_BYTES: usize = 256 * 1024;
const MAX_STATE_BYTES: usize = 64 * 1024;
const MAX_UPDATER_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    /// How long a version gets to come up, migrations included.
    pub health_timeout_secs: u64,
    /// How long web and mcp get once everything behind them has passed.
    /// They run images that have just passed (mcp is the api image) or that
    /// serve files, so this is short: from the moment they start, users
    /// reach the new release, which is to refuse writes while the switching
    /// marker is there, and a rollback restores the database regardless.
    pub entry_timeout_secs: u64,
    pub poll_secs: u64,
    /// Kept free on top of what the images and the dump need.
    pub disk_reserve_bytes: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            health_timeout_secs: 20 * 60,
            entry_timeout_secs: 3 * 60,
            poll_secs: 2,
            disk_reserve_bytes: 512 * 1024 * 1024,
        }
    }
}

impl Config {
    /// The defaults, overridden by `LAYSI_UPDATER_HEALTH_TIMEOUT_SECS`,
    /// `LAYSI_UPDATER_ENTRY_TIMEOUT_SECS` and
    /// `LAYSI_UPDATER_DISK_RESERVE_BYTES` from `state/updater.env`.
    pub fn from_env() -> Config {
        let mut config = Config::default();
        let number = |key: &str| {
            std::env::var(key)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        if let Some(secs) = number("LAYSI_UPDATER_HEALTH_TIMEOUT_SECS") {
            config.health_timeout_secs = secs;
        }
        if let Some(secs) = number("LAYSI_UPDATER_ENTRY_TIMEOUT_SECS") {
            config.entry_timeout_secs = secs;
        }
        if let Some(bytes) = number("LAYSI_UPDATER_DISK_RESERVE_BYTES") {
            config.disk_reserve_bytes = bytes;
        }
        config
    }
}

pub struct Machine<'h, H: Host> {
    pub host: &'h mut H,
    pub layout: Layout,
    pub keys: Vec<VerifyingKey>,
    pub config: Config,
    /// Asked once per run, the first time a result is written.
    tools: Option<ToolVersions>,
}

enum Space {
    Short(String),
    Unknown(String),
}

/// Why a preparation gave up, with the status the result reports.
struct Abandoned {
    status: UpdateStatus,
    reason: String,
}

impl From<String> for Abandoned {
    fn from(reason: String) -> Abandoned {
        Abandoned {
            status: UpdateStatus::FailedUnchanged,
            reason,
        }
    }
}

impl<'h, H: Host> Machine<'h, H> {
    pub fn new(host: &'h mut H, layout: Layout, keys: Vec<VerifyingKey>, config: Config) -> Self {
        Machine {
            host,
            layout,
            keys,
            config,
            tools: None,
        }
    }

    pub fn log(&self, message: impl AsRef<str>) {
        let line = format!(
            "{} {}\n",
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            message.as_ref()
        );
        eprint!("{line}");
        let path = self.layout.log();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
            let _ = std::fs::rename(&path, path.with_extension("log.1"));
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    // Results

    fn result_path(&self, id: &RequestId) -> PathBuf {
        self.layout.results_dir().join(id.as_str())
    }

    pub fn read_result(&self, id: &RequestId) -> Option<UpdateResult> {
        let bytes = fsx::read_own(&self.result_path(id), MAX_RESULT_BYTES).ok()??;
        UpdateResult::decode(std::str::from_utf8(&bytes).ok()?).ok()
    }

    /// A result that cannot be written is logged. Only a final one is worth
    /// failing over: the journal stays until it is written, so the run that
    /// finishes the journal writes it again.
    ///
    /// Every result records the tools this run used.
    fn write_result(&mut self, result: &UpdateResult) -> Result<(), String> {
        let path = self.result_path(&result.request_id);
        let tools = match &self.tools {
            Some(tools) => tools.clone(),
            None => self.tools.insert(self.host.tool_versions()).clone(),
        };
        let mut result = result.clone();
        result.tools = Some(tools);
        let written = fsx::write_atomic(&path, result.encode().as_bytes(), 0o644).map_err(|e| {
            let e = format!("could not write the result for {}: {e}", result.request_id);
            self.log(&e);
            e
        });
        self.prune_results();
        written
    }

    fn prune_results(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.layout.results_dir()) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name();
                name.to_str().is_some_and(|n| RequestId::parse(n).is_ok())
                    && e.file_type().is_ok_and(|t| t.is_file())
            })
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if files.len() <= KEEP_RESULTS {
            return;
        }
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        for (_, path) in files.into_iter().skip(KEEP_RESULTS) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// A step the update is in. The final result is what has to arrive.
    fn progress(&mut self, j: &Journal, status: UpdateStatus) {
        let _ = self.report(j, status);
    }

    fn report(&mut self, j: &Journal, status: UpdateStatus) -> Result<(), String> {
        let mut result = UpdateResult::new(j.request.clone(), status, j.received_at);
        result.updated_at = self.host.now();
        result.offered = Some(j.to.version.clone());
        result.running = match status {
            UpdateStatus::Succeeded | UpdateStatus::SucceededUpdaterKept => {
                Some(j.to.version.clone())
            }
            UpdateStatus::Switching | UpdateStatus::RollingBack => None,
            // The release the stack files name, which is what a boot starts;
            // it did not come up, and when the files could not be put back
            // there is no release to name.
            UpdateStatus::RollbackFailed => {
                let named = self
                    .env_of(&self.stack_files())
                    .get(envfile::RELEASE)
                    .map(str::to_string);
                (named.as_deref() == Some(j.from.version.to_string().as_str()))
                    .then(|| j.from.version.clone())
            }
            _ => Some(j.from.version.clone()),
        };
        result.downtime_from = j.downtime_from;
        result.downtime_to = j.downtime_to;
        result.reason = match status {
            UpdateStatus::SucceededUpdaterKept => j.updater_note.clone(),
            _ => j.reason.clone(),
        };
        result.rollback_reason = j.rollback_reason.clone();
        self.write_result(&result)
    }

    // The switching marker

    fn switching_marker(&self) -> PathBuf {
        self.layout.results_dir().join(SWITCHING_FILE)
    }

    /// Before step 4, and again by a run that resumes an update past it.
    fn mark_switching(&mut self, j: &Journal) -> Result<(), String> {
        fsx::write_atomic(
            &self.switching_marker(),
            format!("request {}\n", j.request).as_bytes(),
            0o644,
        )
        .map_err(|e| format!("無法寫入切換標記：{e}"))
    }

    /// The marker goes once no update is left that could still take back a
    /// write: no journal, or one ended in `RollbackFailed` whose final
    /// result is written. Every run that holds the lock asks after its
    /// recovery, so one that died between the final result and this, or
    /// that lost power, leaves the marker only until the boot or recovery
    /// unit runs.
    fn settle_switching(&mut self) {
        let done = match self.load_journal() {
            Ok(None) => true,
            Ok(Some(j)) if j.phase == Phase::RollbackFailed => self
                .read_result(&j.request)
                .is_some_and(|r| r.status.is_final()),
            _ => false,
        };
        if done {
            if let Err(e) = fsx::remove_durable(&self.switching_marker()) {
                self.log(format!("could not remove the switching marker: {e}"));
            }
        }
    }

    fn refuse(&mut self, mut result: UpdateResult, status: UpdateStatus, reason: String) {
        self.log(format!("request {}: {status}: {reason}", result.request_id));
        result.status = status;
        result.updated_at = self.host.now();
        result.reason = Some(reason);
        let _ = self.write_result(&result);
    }

    // State files

    pub fn load_journal(&self) -> Result<Option<Journal>, String> {
        match fsx::read_own(&self.layout.journal(), MAX_STATE_BYTES) {
            Ok(None) => Ok(None),
            Ok(Some(bytes)) => {
                let text = String::from_utf8(bytes).map_err(|_| "journal is not text")?;
                Journal::decode(&text).map(Some)
            }
            Err(e) => Err(format!("journal: {e}")),
        }
    }

    fn save(&mut self, j: &Journal) -> Result<(), String> {
        fsx::write_atomic(&self.layout.journal(), j.encode().as_bytes(), 0o600)
            .map_err(|e| format!("writing the journal: {e}"))?;
        self.log(format!("request {}: {}", j.request, j.phase));
        self.host.checkpoint(j.phase.name());
        Ok(())
    }

    fn drop_journal(&mut self) -> Result<(), String> {
        fsx::remove_durable(&self.layout.journal())
            .map_err(|e| format!("removing the journal: {e}"))
    }

    pub fn load_installed(&self) -> Result<Release, String> {
        let bytes = fsx::read_own(&self.layout.installed(), MAX_STATE_BYTES)
            .map_err(|e| e.to_string())?
            .ok_or("no installed release is recorded")?;
        journal::decode_installed(std::str::from_utf8(&bytes).map_err(|_| "not text")?)
    }

    fn save_installed(&mut self, release: &Release) -> Result<(), String> {
        fsx::write_atomic(
            &self.layout.installed(),
            journal::encode_installed(release).as_bytes(),
            0o600,
        )
        .map_err(|e| format!("writing the installed release: {e}"))
    }

    /// The `.env` of a set of stack files. Missing or unreadable is an error:
    /// what is built from it would otherwise carry defaults in place of the
    /// site's settings.
    fn read_env(&self, files: &StackFiles) -> Result<EnvFile, String> {
        let bytes = fsx::read_own(&files[2], MAX_STATE_BYTES)
            .map_err(|e| format!("{}: {e}", files[2].display()))?
            .ok_or_else(|| format!("{} is missing", files[2].display()))?;
        let text =
            String::from_utf8(bytes).map_err(|_| format!("{} is not text", files[2].display()))?;
        Ok(EnvFile::parse(&text))
    }

    /// The same, empty when it cannot be read: only for questions an empty
    /// file answers with "no", such as which release the files name.
    fn env_of(&self, files: &StackFiles) -> EnvFile {
        self.read_env(files).unwrap_or_default()
    }

    fn stack_files(&self) -> StackFiles {
        self.layout.stack_files()
    }

    // Entry points

    /// A request arrived: the path unit started this run.
    pub fn run_request(&mut self) -> Result<(), String> {
        let lock = Lock::acquire(&self.layout.lock(), false)
            .map_err(|e| format!("opening the lock: {e}"))?;
        let request = self.read_request();
        let Some(_lock) = lock else {
            // The run holding the lock may be answering this very request.
            if let Some(request) = request {
                if self.read_result(&request.id).is_none() {
                    self.answer_busy(&request.id);
                }
            }
            return Ok(());
        };

        // A run that was killed left its update behind: finish that first.
        match self.load_journal()? {
            Some(j) if j.phase != Phase::RollbackFailed => {
                self.recover(j)?;
                if let Err(e) = self.start_what_is_safe() {
                    self.log(e);
                }
            }
            Some(j) => self.answer_final(&j),
            None => {}
        }
        self.settle_switching();

        let Some(request) = request else {
            return Ok(());
        };
        if self
            .read_result(&request.id)
            .is_some_and(|r| r.status.is_final())
        {
            self.log(format!("request {} was already answered", request.id));
            return Ok(());
        }
        self.handle(request)
    }

    /// The machine started: finish whatever the journal says, then start the
    /// stack. This is the only thing that starts the stack at boot.
    pub fn boot(&mut self) -> Result<(), String> {
        let _lock = Lock::acquire(&self.layout.lock(), true)
            .map_err(|e| format!("opening the lock: {e}"))?;
        // An update this updater cannot read or finish is left as it is:
        // started on top of it, the stack could run swapped files on a
        // database that was never restored. The boot fails instead, and the
        // fallback unit has the previous updater try the same journal.
        match self.load_journal() {
            Ok(Some(j)) if j.phase != Phase::RollbackFailed => {
                if let Err(e) = self.recover(j) {
                    self.log(format!("recovery failed, nothing started: {e}"));
                    return Err(e);
                }
            }
            Ok(Some(j)) => self.answer_final(&j),
            Ok(None) => {}
            Err(e) => {
                self.log(format!("the journal cannot be read, nothing started: {e}"));
                return Err(e);
            }
        }
        self.settle_switching();
        let started = self.start_what_is_safe();
        if let Err(e) = &started {
            self.log(e);
        }
        started
    }

    /// A journal left in its final phase still owes its request a final
    /// result when the run that wrote it died before reporting; the request
    /// is then not taken for a new one.
    fn answer_final(&mut self, j: &Journal) {
        if !self
            .read_result(&j.request)
            .is_some_and(|r| r.status.is_final())
        {
            self.progress(j, UpdateStatus::RollbackFailed);
        }
    }

    /// The stack, unless a rollback that never restored the database left
    /// its dump as the only copy of the data. Then nothing that serves or
    /// writes starts on what is left of the database: postgres alone runs,
    /// so the dump can be restored by hand, and only when the stack files
    /// are back to the release the host records, so it is the old postgres
    /// image; otherwise nothing starts. Neither is a failure of the boot,
    /// which would only have the previous updater start everything.
    fn start_what_is_safe(&mut self) -> Result<(), String> {
        let failed = matches!(self.load_journal(), Ok(Some(j)) if j.phase == Phase::RollbackFailed);
        if !(failed && self.retained_backup().is_some()) {
            return self.start_stack();
        }
        let release = match self.load_installed() {
            Ok(release) => release,
            Err(e) => {
                self.log(format!("a rollback left its dump and the installed release is unknown, nothing started: {e}"));
                return Ok(());
            }
        };
        let named = self
            .env_of(&self.stack_files())
            .get(envfile::RELEASE)
            .map(str::to_string);
        if named.as_deref() != Some(release.version.to_string().as_str()) {
            self.log("a rollback left its dump and the stack files are not the installed release's: nothing started");
            return Ok(());
        }
        // Whatever the rollback could not stop must not be writing when the
        // old postgres comes up under it.
        if let Err(e) = self.ensure_stopped() {
            self.log(format!(
                "a rollback left its dump and {e}: the database is left as it is"
            ));
            return Ok(());
        }
        self.log("a rollback left its dump: starting postgres only");
        if let Err(e) = self.database_up(false, &release, None) {
            self.log(format!("starting postgres: {e}"));
        }
        Ok(())
    }

    /// After every run: a request that arrived while it ran is discarded and
    /// told so, then the directory is emptied, or the path unit stops
    /// watching it.
    ///
    /// A request this run accepted but never wrote a journal for, because it
    /// died in between, is answered as failed here: nothing will pick it up
    /// again once it is cleared. One whose journal exists, or might, is left
    /// to the recovery.
    ///
    /// `run_failed` is whether the run itself ended in failure.
    pub fn discard_requests(&mut self, run_failed: bool) -> Result<(), String> {
        if let Some(request) = self.read_request() {
            // A journal is kept until its final result is written: the
            // request it names is the recovery's to answer. Otherwise a
            // request with no result arrived during the run, unless the run
            // failed with no update under way: then it may never have been
            // read, as when the current updater cannot start at all, and
            // "busy" would have the administrator retry an updater that fails
            // every time. One left without a final result never got a journal
            // at all.
            match (self.load_journal(), self.read_result(&request.id)) {
                (Err(_), _) => {}
                (Ok(Some(j)), _) if j.request == request.id => {
                    if j.phase == Phase::RollbackFailed {
                        self.answer_final(&j);
                    }
                }
                (Ok(None), None) if run_failed => self.answer_not_started(&request.id),
                (Ok(_), None) => self.answer_busy(&request.id),
                (Ok(_), Some(result)) if !result.status.is_final() => {
                    self.refuse(result, UpdateStatus::FailedUnchanged, NOT_STARTED.into())
                }
                (Ok(_), Some(_)) => {}
            }
        }
        let dir = self.layout.requests_dir();
        self.host.clear_requests(&dir)
    }

    fn answer_not_started(&mut self, id: &RequestId) {
        let now = self.host.now();
        let result = UpdateResult::new(id.clone(), UpdateStatus::Accepted, now);
        self.refuse(result, UpdateStatus::FailedUnchanged, NOT_STARTED.into());
    }

    fn answer_busy(&mut self, id: &RequestId) {
        let now = self.host.now();
        let result = UpdateResult::new(id.clone(), UpdateStatus::Accepted, now);
        self.refuse(
            result,
            UpdateStatus::RefusedBusy,
            "另一次更新或開機復原正在進行，本次更新指示未執行".into(),
        );
    }

    fn read_request(&mut self) -> Option<UpdateRequest> {
        let dir = self.layout.requests_dir();
        match fsx::read_untrusted(&dir, REQUEST_FILE, MAX_REQUEST_BYTES) {
            Ok(Some(text)) => match UpdateRequest::decode(&text) {
                Ok(request) => Some(request),
                Err(e) => {
                    self.log(format!("ignoring an unreadable request: {e}"));
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                self.log(format!("ignoring the request file: {e}"));
                None
            }
        }
    }

    fn handle(&mut self, request: UpdateRequest) -> Result<(), String> {
        let now = self.host.now();
        let mut result = UpdateResult::new(request.id.clone(), UpdateStatus::Accepted, now);
        self.log(format!("request {}: accepted", request.id));
        let _ = self.write_result(&result);

        let installed = match self.load_installed() {
            Ok(installed) => installed,
            Err(e) => {
                self.refuse(
                    result,
                    UpdateStatus::RefusedInstalledUnknown,
                    format!("無法讀取已安裝的版本：{e}"),
                );
                return Ok(());
            }
        };
        result.running = Some(installed.version.clone());

        let manifest = match release::verify_with(&request.manifest, &self.keys) {
            Ok(manifest) => manifest,
            Err(e) => {
                self.refuse(result, UpdateStatus::RefusedSignature, e.to_string());
                return Ok(());
            }
        };
        result.offered = Some(manifest.version().clone());
        match manifest.decide(&installed.version.to_string()) {
            Decision::Install => {}
            Decision::UpToDate => {
                let reason = format!("已安裝 {}", installed.version);
                self.refuse(result, UpdateStatus::RefusedSameVersion, reason);
                return Ok(());
            }
            Decision::Ignore(refusal @ Refusal::Downgrade { .. }) => {
                self.refuse(result, UpdateStatus::RefusedDowngrade, refusal.to_string());
                return Ok(());
            }
            Decision::Ignore(refusal) => {
                self.refuse(
                    result,
                    UpdateStatus::RefusedInstalledUnknown,
                    refusal.to_string(),
                );
                return Ok(());
            }
        }

        if let Some(name) = self.retained_backup() {
            self.refuse(
                result,
                UpdateStatus::RefusedBlocked,
                format!("上一次回退沒有完成，資料庫備份 {name} 保留在主機上，處理之前不再更新"),
            );
            return Ok(());
        }

        // Step 2, before anything is written.
        match self.check_space(false) {
            Ok(()) => {}
            Err(Space::Short(reason)) => {
                self.refuse(result, UpdateStatus::RefusedDiskSpace, reason);
                return Ok(());
            }
            Err(Space::Unknown(reason)) => {
                self.refuse(result, UpdateStatus::FailedUnchanged, reason);
                return Ok(());
            }
        }

        let j = Journal::new(request.id, now, installed, Release::of(&manifest));
        if let Err(e) = self.save(&j) {
            self.refuse(
                result,
                UpdateStatus::FailedUnchanged,
                format!("{NOT_STARTED}：{e}"),
            );
            return Err(e);
        }
        self.progress(&j, UpdateStatus::Preparing);
        self.advance(j)
    }

    fn retained_backup(&self) -> Option<String> {
        std::fs::read_dir(self.layout.backup_dir())
            .ok()?
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().ends_with(".dump"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
    }

    /// Continue an update a run left behind. Before `Switching` that means
    /// returning to the old version; from `Switching` on, finishing the
    /// switch and judging it.
    pub fn recover(&mut self, mut j: Journal) -> Result<(), String> {
        self.log(format!(
            "request {}: recovering from {}",
            j.request, j.phase
        ));
        // Past step 4 the marker is already there; written again for a
        // journal left by an updater that did not write one. Without it
        // the recovery still has to run.
        if j.phase >= Phase::Stopping && j.phase != Phase::RollbackFailed {
            if let Err(e) = self.mark_switching(&j) {
                self.log(e);
            }
        }
        match j.phase {
            Phase::Preparing | Phase::Prepared => {
                return self.abandon(j, UpdateStatus::FailedUnchanged, INTERRUPTED.into());
            }
            Phase::Stopping | Phase::BackedUp => {
                self.roll_back(&mut j, false, false, INTERRUPTED.into())?;
            }
            Phase::FilesSaved | Phase::FilesSwapped => {
                self.roll_back(&mut j, true, false, INTERRUPTED.into())?;
            }
            Phase::Switching | Phase::CoreUp => {
                // Only the updater died, perhaps: what it started may still
                // run, web and mcp included. Nothing runs into the redone
                // switch, and what cannot be stopped is rolled back from.
                if let Err(reason) = self.ensure_stopped() {
                    self.roll_back(&mut j, true, true, reason)?;
                } else {
                    j.phase = Phase::Switching;
                    j.switch_started = Some(self.host.now());
                    self.save(&j)?;
                }
            }
            Phase::Committed
            | Phase::Finishing
            | Phase::RollingBack
            | Phase::Restoring
            | Phase::Restored => {}
            Phase::RollbackFailed => return Ok(()),
        }
        self.advance(j)
    }

    /// Run the journal from its phase to a final result.
    fn advance(&mut self, mut j: Journal) -> Result<(), String> {
        loop {
            match j.phase {
                Phase::Preparing => {
                    if let Err(Abandoned { status, reason }) = self.prepare(&j) {
                        return self.abandon(j, status, reason);
                    }
                    j.phase = Phase::Prepared;
                    self.save(&j)?;
                }
                Phase::Prepared => {
                    // Step 4 starts here: the downtime and what was running.
                    // The marker first, so api and mcp refuse writes before
                    // anything stops; without it nothing stops.
                    if let Err(reason) = self.mark_switching(&j) {
                        return self.abandon(j, UpdateStatus::FailedUnchanged, reason);
                    }
                    let containers = self.host.containers().unwrap_or_default();
                    j.keep = containers
                        .iter()
                        .filter(|c| {
                            split_reference(&c.image_name).is_some()
                                && !c.image_name.contains(char::is_whitespace)
                        })
                        .map(|c| c.image_name.clone())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    j.downtime_from = Some(self.host.now());
                    j.phase = Phase::Stopping;
                    self.save(&j)?;
                    self.progress(&j, UpdateStatus::Switching);
                }
                Phase::Stopping => {
                    if let Err(reason) = self.ensure_stopped() {
                        self.roll_back(&mut j, false, false, reason)?;
                        continue;
                    }
                    // Step 5.
                    match self.dump(&j) {
                        Ok(name) => {
                            j.backup = Some(name);
                            j.phase = Phase::BackedUp;
                            self.save(&j)?;
                        }
                        Err(reason) => self.roll_back(&mut j, false, false, reason)?,
                    }
                }
                Phase::BackedUp => {
                    // Step 6: the previous files beside the current ones.
                    let saved = self.stack_files().iter().try_for_each(|file| {
                        fsx::copy_atomic(file, &Layout::previous_of(file), 0o600, MAX_COMPOSE_BYTES)
                            .map_err(|e| format!("無法保存 {}：{e}", file.display()))
                    });
                    match saved {
                        Ok(()) => {
                            j.phase = Phase::FilesSaved;
                            self.save(&j)?;
                        }
                        Err(reason) => self.roll_back(&mut j, false, false, reason)?,
                    }
                }
                Phase::FilesSaved => {
                    let incoming = self.layout.incoming_files();
                    let mut swapped = Ok(());
                    for (from, to) in incoming.iter().zip(self.stack_files().iter()) {
                        swapped = fsx::copy_atomic(from, to, 0o600, MAX_COMPOSE_BYTES)
                            .map_err(|e| format!("無法寫入 {}：{e}", to.display()));
                        if swapped.is_err() {
                            break;
                        }
                        self.host.checkpoint("stack file swapped");
                    }
                    match swapped {
                        Ok(()) => {
                            j.phase = Phase::FilesSwapped;
                            self.save(&j)?;
                        }
                        Err(reason) => self.roll_back(&mut j, true, false, reason)?,
                    }
                }
                Phase::FilesSwapped => {
                    j.switch_started = Some(self.host.now());
                    j.phase = Phase::Switching;
                    self.save(&j)?;
                }
                Phase::Switching => {
                    // Steps 7 and 8, the part with no entry point open.
                    let files = self.stack_files();
                    match self.start_core(&files, Some(&j.to.clone()), true, j.switch_started) {
                        Ok(()) => {
                            j.phase = Phase::CoreUp;
                            self.save(&j)?;
                        }
                        Err(reason) => self.roll_back(&mut j, true, true, reason)?,
                    }
                }
                Phase::CoreUp => {
                    // The old release cannot run on a database the new one
                    // has migrated, so a rollback from here on restores the
                    // dump even though web and mcp were reachable for a
                    // moment. The switching marker tells them to refuse
                    // writes meanwhile; entry_timeout_secs bounds that
                    // span, and roll_back says how long it was, counted
                    // from the first time they started.
                    if j.entries_opened.is_none() {
                        j.entries_opened = Some(self.host.now());
                        self.save(&j)?;
                    }
                    let files = self.stack_files();
                    match self.start_entries(&files, Some(&j.to.clone()), true, j.switch_started) {
                        Ok(()) => {
                            j.downtime_to = Some(self.host.now());
                            j.phase = Phase::Committed;
                            self.save(&j)?;
                        }
                        Err(reason) => self.roll_back(&mut j, true, true, reason)?,
                    }
                }
                Phase::Committed => {
                    // Step 10.
                    let to = j.to.clone();
                    self.save_installed(&to)?;
                    j.updater_note = self.replace_updater(&mut j)?;
                    j.phase = Phase::Finishing;
                    self.save(&j)?;
                }
                Phase::Finishing => {
                    self.prune_images(&j, &[&j.from, &j.to], &[]);
                    self.remove_backup(&j)?;
                    self.empty_incoming();
                    let status = if j.updater_note.is_some() {
                        UpdateStatus::SucceededUpdaterKept
                    } else {
                        UpdateStatus::Succeeded
                    };
                    self.report(&j, status)?;
                    self.drop_journal()?;
                    self.settle_switching();
                    self.log(format!(
                        "request {}: {status}, running {}",
                        j.request, j.to.version
                    ));
                    return Ok(());
                }
                Phase::RollingBack => {
                    self.progress(&j, UpdateStatus::RollingBack);
                    // The files first: whatever happens below, a boot starts
                    // the release they name, and from here that is the old
                    // one.
                    if j.restore_files {
                        let restored = self.stack_files().iter().try_for_each(|file| {
                            fsx::copy_atomic(
                                &Layout::previous_of(file),
                                file,
                                0o600,
                                MAX_COMPOSE_BYTES,
                            )
                            .map_err(|e| format!("無法還原 {}：{e}", file.display()))
                        });
                        if let Err(reason) = restored {
                            return self.fail_rollback(j, reason);
                        }
                        self.host.checkpoint("stack files restored");
                    }
                    let from = j.from.clone();
                    self.save_installed(&from)?;
                    if j.restore_db {
                        // Nothing may write while the database is replaced.
                        if let Err(reason) = self.ensure_stopped() {
                            return self.fail_rollback(j, reason);
                        }
                        // The old postgres image, whatever the new one was.
                        let since = self.host.now();
                        if let Err(reason) = self.database_up(true, &from, Some(since)) {
                            return self
                                .fail_rollback(j, format!("舊版資料庫服務沒有啟動：{reason}"));
                        }
                        j.phase = Phase::Restoring;
                    } else {
                        // Nothing to protect: start_core recreates whatever
                        // is still running.
                        if let Err(e) = self.stop_all_but_database() {
                            self.log(format!("stopping before the rollback: {e}"));
                        }
                        j.phase = Phase::Restored;
                    }
                    self.save(&j)?;
                }
                Phase::Restoring => {
                    if let Err(reason) = self.ensure_stopped() {
                        return self.fail_rollback(j, format!("{reason}，{BACKUP_KEPT}"));
                    }
                    let from = j.from.clone();
                    // The one retry, begun or resumed: room first, since a
                    // full disk is the likely cause, then a fresh old
                    // postgres. Everything removed here is recreated from
                    // the old files when the old release starts.
                    let retry = j.restore_retry.clone();
                    if retry.is_some() {
                        self.clear_for_retry(&j);
                    }
                    let since = retry.is_some().then(|| self.host.now());
                    if let Err(reason) = self.database_up(retry.is_some(), &from, since) {
                        return self.fail_rollback(
                            j,
                            format!("舊版資料庫服務沒有啟動：{reason}，{BACKUP_KEPT}"),
                        );
                    }
                    match (self.restore(&j), retry) {
                        (Ok(()), retry) => {
                            if let Some(first) = retry {
                                j.rollback_reason = Some(format!("第一次{first}，重試後成功"));
                            }
                            j.phase = Phase::Restored;
                            self.save(&j)?;
                        }
                        (Err(reason), None) => {
                            self.log(format!(
                                "request {}: {reason}; retrying the restore once",
                                j.request
                            ));
                            j.restore_retry = Some(reason);
                            self.save(&j)?;
                        }
                        (Err(reason), Some(first)) => {
                            return self.fail_rollback(
                                j,
                                format!("重試後{reason}（第一次{first}），{BACKUP_KEPT}"),
                            );
                        }
                    }
                }
                Phase::Restored => {
                    self.remove_backup(&j)?;
                    let files = self.stack_files();
                    let from = j.from.clone();
                    let since = Some(self.host.now());
                    let started = self
                        .start_core(&files, Some(&from), true, since)
                        .and_then(|()| self.start_entries(&files, Some(&from), true, since));
                    if let Err(reason) = started {
                        return self.fail_rollback(j, reason);
                    }
                    j.downtime_to = Some(self.host.now());
                    self.drop_offered(&j);
                    self.report(&j, UpdateStatus::RolledBack)?;
                    self.drop_journal()?;
                    self.settle_switching();
                    self.log(format!(
                        "request {}: rolled back to {}",
                        j.request, j.from.version
                    ));
                    return Ok(());
                }
                Phase::RollbackFailed => return Ok(()),
            }
        }
    }

    fn roll_back(
        &mut self,
        j: &mut Journal,
        files: bool,
        db: bool,
        reason: String,
    ) -> Result<(), String> {
        self.log(format!("request {}: rolling back: {reason}", j.request));
        j.restore_files = files;
        j.restore_db = db && j.backup.is_some();
        let reason = match j.entries_opened {
            Some(opened) if j.restore_db => {
                let secs = (self.host.now() - opened).num_seconds().max(0);
                format!("{reason}；新版開放使用後 {secs} 秒內寫入的資料已隨資料庫一併還原")
            }
            _ => reason,
        };
        if j.reason.is_none() {
            j.reason = Some(reason);
        }
        j.phase = Phase::RollingBack;
        self.save(j)
    }

    /// Final: the old version did not come back. Services other than postgres
    /// stay as they are; the next boot only starts the stack.
    fn fail_rollback(&mut self, mut j: Journal, reason: String) -> Result<(), String> {
        self.log(format!("request {}: rollback failed: {reason}", j.request));
        // A retried restore that succeeded said so here already.
        j.rollback_reason = Some(match j.rollback_reason.take() {
            Some(earlier) => format!("{reason}；{earlier}"),
            None => reason,
        });
        j.phase = Phase::RollbackFailed;
        self.save(&j)?;
        self.empty_incoming();
        self.progress(&j, UpdateStatus::RollbackFailed);
        self.settle_switching();
        Ok(())
    }

    /// Before step 4: nothing the running system uses has changed.
    fn abandon(
        &mut self,
        mut j: Journal,
        status: UpdateStatus,
        reason: String,
    ) -> Result<(), String> {
        self.log(format!("request {}: not installed: {reason}", j.request));
        j.reason = Some(reason);
        self.drop_offered(&j);
        self.report(&j, status)?;
        self.drop_journal()?;
        self.settle_switching();
        Ok(())
    }

    /// What was pulled for a release that is not staying goes again, or every
    /// failed attempt keeps a release's worth of images that check_space
    /// never counts. Then the files taken from it.
    fn drop_offered(&mut self, j: &Journal) {
        let incoming = self.layout.incoming_files();
        let pulled: Vec<String> = self
            .host
            .compose_config(&incoming)
            .unwrap_or_default()
            .iter()
            .filter_map(|s| split_reference(&s.image).map(|(repo, _)| repo.to_string()))
            .collect();
        self.prune_images(j, &[&j.from], &pulled);
        self.empty_incoming();
    }

    fn empty_incoming(&mut self) {
        if let Err(e) = fsx::empty_own_dir(&self.layout.incoming_dir()) {
            self.log(format!("could not empty incoming/: {e}"));
        }
    }

    // Step 2

    /// Room for the new images and a dump, beside what runs now.
    ///
    /// Before anything is pulled, the size of the new images is not known:
    /// the updater cannot ask the registry without podman pulling, so the
    /// release is taken to be at most `RELEASE_GROWTH` times the size of the
    /// one running, whose images stay. With `pulled`, the images are on disk
    /// and only the dump and the reserve still have to fit; that check runs
    /// before anything stops.
    fn check_space(&mut self, pulled: bool) -> Result<(), Space> {
        let containers = self.host.containers().map_err(Space::Unknown)?;
        let postgres = running_container(&containers, DATABASE)
            .ok_or_else(|| Space::Unknown("資料庫服務沒有在執行，無法備份".into()))?;
        let database = self
            .host
            .database_size(&postgres)
            .map_err(|e| Space::Unknown(format!("無法取得資料庫大小：{e}")))?;
        let digests: HashSet<String> = containers.iter().map(|c| c.image_digest.clone()).collect();
        let images: u64 = self
            .host
            .images()
            .map_err(Space::Unknown)?
            .iter()
            .filter(|i| image_digests(i).any(|d| digests.contains(d)))
            .map(|i| i.size)
            .sum();
        let images = if pulled {
            0
        } else {
            images.saturating_mul(RELEASE_GROWTH)
        };
        let store = self.host.image_store().map_err(Space::Unknown)?;
        let backups = self.layout.backup_dir();
        let reserve = self.config.disk_reserve_bytes;
        let mut needs = vec![(store.clone(), images + reserve, "映像")];
        if self
            .host
            .same_device(&store, &backups)
            .map_err(Space::Unknown)?
        {
            needs[0].1 += database;
        } else {
            needs.push((backups, database + reserve, "資料庫備份"));
        }
        for (path, need, what) in needs {
            let free = self.host.free_bytes(&path).map_err(Space::Unknown)?;
            if free < need {
                return Err(Space::Short(format!(
                    "{what}所在的磁碟剩 {}，更新需要 {}（新映像約 {}、資料庫備份約 {}）",
                    size(free),
                    size(need),
                    size(images),
                    size(database)
                )));
            }
        }
        Ok(())
    }

    // Step 3

    fn prepare(&mut self, j: &Journal) -> Result<(), Abandoned> {
        let incoming = self.layout.incoming_dir();
        fsx::ensure_dir(&incoming, 0o700).map_err(|e| e.to_string())?;
        fsx::empty_own_dir(&incoming).map_err(|e| e.to_string())?;

        let current = self.read_env(&self.stack_files())?;
        let registry = current.registry();
        for image in Image::ALL {
            let reference = format!(
                "{}@{}",
                envfile::repository(&registry, image),
                j.to.digest(image)
            );
            if let Err(e) = self.host.pull(&reference) {
                self.log(format!("pulling {reference}: {e}"));
                return Err(pull_failure(&format!(" {image} 映像"), &e).into());
            }
        }

        let api = format!(
            "{}@{}",
            envfile::repository(&registry, Image::Api),
            j.to.digest(Image::Api)
        );
        let [prod, ship, env] = self.layout.incoming_files();
        let inside = |name: &str| format!("{IMAGE_SHARE}/{name}");
        let (prod_src, ship_src) = (inside(COMPOSE_FILES[0]), inside(COMPOSE_FILES[1]));
        self.host
            .extract(
                &api,
                &[(prod_src.as_str(), &prod), (ship_src.as_str(), &ship)],
            )
            .map_err(|e| format!("新版 api 映像裡沒有 compose 檔：{e}"))?;
        for file in [&prod, &ship] {
            check_extracted(file, MAX_COMPOSE_BYTES)?;
        }
        let updater = self.layout.incoming_updater();
        match self.host.extract(&api, &[(IMAGE_UPDATER, &updater)]) {
            Ok(()) => {
                check_extracted(&updater, MAX_UPDATER_BYTES)?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&updater, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => self.log(format!("the new api image carries no updater: {e}")),
        }

        let host_dir = self.layout.root.to_string_lossy().into_owned();
        let next = current.for_release(&j.to, &host_dir);
        fsx::write_atomic(&env, next.render().as_bytes(), 0o600).map_err(|e| e.to_string())?;

        let files = self.layout.incoming_files();
        let services = self
            .host
            .compose_config(&files)
            .map_err(|e| format!("無法讀取新版 compose 檔：{e}"))?;
        check_release_files(&services, &j.to, &registry)?;
        if let Err(e) = self.host.compose_pull(&files) {
            self.log(format!("pulling the new release's images: {e}"));
            return Err(pull_failure("新版的映像", &e).into());
        }
        match self.check_space(true) {
            Ok(()) => Ok(()),
            Err(Space::Short(reason)) => Err(Abandoned {
                status: UpdateStatus::RefusedDiskSpace,
                reason,
            }),
            Err(Space::Unknown(reason)) => Err(reason.into()),
        }
    }

    // Steps 4 and 5

    /// Stop everything but postgres and confirm that nothing else runs: the
    /// dump and the restore must not race a writer.
    fn ensure_stopped(&mut self) -> Result<(), String> {
        let mut still = Vec::new();
        for _ in 0..3 {
            let stopped = self.stop_all_but_database();
            still = self
                .host
                .containers()?
                .into_iter()
                .filter(|c| c.running && c.service != DATABASE)
                .map(|c| c.service)
                .collect();
            if still.is_empty() {
                return Ok(());
            }
            if let Err(e) = stopped {
                self.log(format!("stopping: {e}"));
            }
        }
        Err(format!("無法停止 {}", still.join("、")))
    }

    /// Entry points first and on their own, so nothing is written while the
    /// rest stops.
    fn stop_all_but_database(&mut self) -> Result<(), String> {
        let containers = self.host.containers()?;
        let running = |entry: bool| -> Vec<String> {
            containers
                .iter()
                .filter(|c| c.running && c.service != DATABASE)
                .filter(|c| ENTRIES.contains(&c.service.as_str()) == entry)
                .map(|c| c.name.clone())
                .collect()
        };
        for group in [running(true), running(false)] {
            if !group.is_empty() {
                self.host.stop(&group)?;
            }
        }
        Ok(())
    }

    fn backup_path(&self, j: &Journal) -> PathBuf {
        self.layout.backup_dir().join(format!("{}.dump", j.request))
    }

    fn dump(&mut self, j: &Journal) -> Result<String, String> {
        let path = self.backup_path(j);
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let containers = self.host.containers()?;
        let postgres =
            running_container(&containers, DATABASE).ok_or("資料庫服務沒有在執行，無法備份")?;
        fsx::remove_durable(&path).map_err(|e| e.to_string())?;
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .read(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .map_err(|e| format!("無法建立資料庫備份：{e}"))?;
            self.host
                .dump_database(&postgres, &mut file)
                .map_err(|e| format!("資料庫備份失敗：{e}"))?;
            file.sync_all().map_err(|e| e.to_string())?;
            let mut check = File::open(&path).map_err(|e| e.to_string())?;
            self.host
                .check_dump(&postgres, &mut check)
                .map_err(|e| format!("資料庫備份不完整：{e}"))?;
            fsx::fsync_parent(&path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fsx::remove_durable(&path);
        }
        result.map(|()| name)
    }

    fn remove_backup(&mut self, j: &Journal) -> Result<(), String> {
        fsx::remove_durable(&self.backup_path(j)).map_err(|e| format!("removing the backup: {e}"))
    }

    // Step 9

    /// Postgres from the current stack files, up and judged.
    fn database_up(
        &mut self,
        recreate: bool,
        release: &Release,
        since: Option<DateTime<Utc>>,
    ) -> Result<(), String> {
        let files = self.stack_files();
        let database = only(self.host.compose_config(&files)?, |s| s == DATABASE);
        if database.is_empty() {
            return Err(format!("compose 檔沒有 {DATABASE} 服務"));
        }
        self.host.compose_up(&files, &names(&database), recreate)?;
        let timeout = self.config.health_timeout_secs;
        self.wait_ready(&files, &database, Some(release), since, timeout)
    }

    /// The dump into the running postgres. Never writes to the dump.
    fn restore(&mut self, j: &Journal) -> Result<(), String> {
        let path = self.backup_path(j);
        let mut file = File::open(&path)
            .map_err(|e| format!("資料庫備份 {} 無法開啟：{e}", path.display()))?;
        let containers = self.host.containers()?;
        let postgres = running_container(&containers, DATABASE).ok_or("資料庫服務沒有在執行")?;
        if let Err(e) = self.host.restore_database(&postgres, &mut file) {
            // A database half restored is worse than none: the old api would
            // run its migrations over it. The dump stays for the retry and,
            // after that, for whoever repairs it.
            if let Err(drop) = self.host.drop_database(&postgres) {
                self.log(format!(
                    "dropping the partly restored database failed: {drop}"
                ));
            }
            return Err(format!("還原資料庫失敗：{e}"));
        }
        Ok(())
    }

    /// Room for the retried restore: the new release's containers, stopped
    /// by now, and then its images, which those containers held. postgres
    /// stays; it is recreated from the old files next. Whatever cannot be
    /// removed is logged and the retry goes ahead.
    fn clear_for_retry(&mut self, j: &Journal) {
        match self.host.containers() {
            Ok(containers) => {
                let stopped: Vec<String> = containers
                    .iter()
                    .filter(|c| !c.running && c.service != DATABASE)
                    .map(|c| c.name.clone())
                    .collect();
                if !stopped.is_empty() {
                    if let Err(e) = self.host.remove_containers(&stopped) {
                        self.log(format!("removing containers before the retry: {e}"));
                    }
                }
            }
            Err(e) => self.log(format!("listing containers before the retry: {e}")),
        }
        self.drop_offered(j);
    }

    // Steps 7 and 8, and starting at boot

    /// `secrets` to completion, postgres, then every other long-running
    /// service but the entry points, judged.
    ///
    /// podman-compose is always called with `--no-deps`: it then neither
    /// starts a service the list leaves out (web and mcp) nor gives the
    /// containers `--requires`, which would stop a container from being
    /// removed and recreated while a stopped dependent still exists. The
    /// price is that it no longer waits on `depends_on`, so the database is
    /// started and judged before anything that connects to it.
    fn start_core(
        &mut self,
        files: &StackFiles,
        release: Option<&Release>,
        recreate: bool,
        since: Option<DateTime<Utc>>,
    ) -> Result<(), String> {
        let services = self.host.compose_config(files)?;
        if services.iter().any(|s| s.name == SECRETS) {
            self.host
                .compose_up(files, &[SECRETS.to_string()], recreate)?;
            let containers = self.host.containers()?;
            let name = containers
                .iter()
                .filter(|c| c.service == SECRETS)
                .max_by_key(|c| c.created)
                .map(|c| c.name.clone())
                .ok_or("secrets 沒有容器")?;
            let code = self.host.wait_exit(&name)?;
            if code != 0 {
                return Err(format!("secrets 結束碼為 {code}"));
            }
        }
        let database = only(services.clone(), |s| s == DATABASE);
        if database.is_empty() {
            return Err(format!("compose 檔沒有 {DATABASE} 服務"));
        }
        self.host.compose_up(files, &names(&database), recreate)?;
        let timeout = self.config.health_timeout_secs;
        self.wait_ready(files, &database, release, since, timeout)?;

        let core = only(services, |s| s != SECRETS && !ENTRIES.contains(&s));
        let rest: Vec<Service> = only(core.clone(), |s| s != DATABASE);
        self.host.compose_up(files, &names(&rest), recreate)?;
        self.wait_ready(files, &core, release, since, timeout)
    }

    /// web and mcp, then everything judged together.
    fn start_entries(
        &mut self,
        files: &StackFiles,
        release: Option<&Release>,
        recreate: bool,
        since: Option<DateTime<Utc>>,
    ) -> Result<(), String> {
        let services = self.host.compose_config(files)?;
        let entries = only(services.clone(), |s| ENTRIES.contains(&s));
        self.host.compose_up(files, &names(&entries), recreate)?;
        let running = only(services, |s| s != SECRETS);
        let timeout = self.config.entry_timeout_secs;
        self.wait_ready(files, &running, release, since, timeout)
    }

    /// Start the stack as the files describe it, in the same order an update
    /// does: web and mcp only once everything behind them has passed.
    pub fn start_stack(&mut self) -> Result<(), String> {
        let files = self.stack_files();
        let release = match self.load_installed() {
            Ok(r) => Some(r),
            Err(e) => {
                self.log(format!(
                    "starting without an installed release on record: {e}"
                ));
                None
            }
        };
        self.start_core(&files, release.as_ref(), false, None)
            .and_then(|()| self.start_entries(&files, release.as_ref(), false, None))
            .map_err(|e| format!("starting the stack: {e}"))
    }

    fn wait_ready(
        &mut self,
        files: &StackFiles,
        services: &[Service],
        release: Option<&Release>,
        since: Option<DateTime<Utc>>,
        timeout_secs: u64,
    ) -> Result<(), String> {
        let registry = self.read_env(files)?.registry();
        let deadline = self.host.now() + chrono::Duration::seconds(timeout_secs as i64);
        loop {
            let verdict = match self.host.containers() {
                Ok(containers) => {
                    // Only for containers this run created: a restart count
                    // left from before would fail every later boot at once.
                    let crashing = since.and_then(|_| crash_loop(&containers, services));
                    if let Some(reason) = crashing {
                        return Err(reason);
                    }
                    judge(&containers, services, release, &registry, since)
                }
                Err(e) => Err(e),
            };
            let reason = match verdict {
                Ok(()) => return Ok(()),
                Err(reason) => reason,
            };
            if self.host.now() >= deadline {
                return Err(format!("{timeout_secs} 秒內沒有全部就緒：{reason}"));
            }
            self.host.sleep(self.config.poll_secs);
        }
    }

    // Step 10

    /// Put the new updater in place when it passes its self-check. `Some`
    /// says why the old one stays; the update itself has succeeded either
    /// way, so nothing here fails it.
    ///
    /// `j.updater_swapped` is written before the current binary is touched,
    /// so a run that resumes this step knows whether `current` is still the
    /// one it started with or has already been replaced, also when the two
    /// releases carry the same binary.
    fn replace_updater(&mut self, j: &mut Journal) -> Result<Option<String>, String> {
        let incoming = self.layout.incoming_updater();
        let current = self.layout.current_updater();
        let previous = self.layout.previous_updater();
        let same = |a: &Path, b: &Path| {
            matches!(
                (fsx::read_own(a, MAX_UPDATER_BYTES), fsx::read_own(b, MAX_UPDATER_BYTES)),
                (Ok(Some(x)), Ok(Some(y))) if x == y
            )
        };

        let refused = if !incoming.exists() {
            Some("新版映像沒有附更新程式，保留目前的更新程式".to_string())
        } else {
            self.host
                .self_check(&incoming)
                .err()
                .map(|e| format!("新版更新程式自我檢查不通過，保留目前的更新程式：{e}"))
        };
        // The flag says a swap was begun; only a current binary that is now
        // the incoming one says it got that far. Before that, `previous` may
        // still hold the release before last, which must not come back.
        let replaced = |j: &Journal| j.updater_swapped && same(&current, &incoming);
        if let Some(note) = refused {
            if replaced(j) {
                self.put_previous_back();
            }
            return Ok(Some(note));
        }

        if !same(&current, &incoming) {
            j.updater_swapped = true;
            self.save(j)?;
            if let Err(e) = fsx::copy_atomic(&current, &previous, 0o755, MAX_UPDATER_BYTES) {
                return Ok(Some(format!("無法保留目前的更新程式，未換上新版：{e}")));
            }
            self.host.checkpoint("previous updater kept");
            if let Err(e) = fsx::copy_atomic(&incoming, &current, 0o755, MAX_UPDATER_BYTES) {
                return Ok(Some(format!("無法換上新版更新程式：{e}")));
            }
            self.host.checkpoint("new updater installed");
        }
        match self.host.install_units(&current) {
            Ok(()) => Ok(None),
            Err(e) if replaced(j) => {
                self.put_previous_back();
                Ok(Some(format!(
                    "新版更新程式無法寫出 systemd 設定，保留目前的更新程式：{e}"
                )))
            }
            Err(e) => Ok(Some(format!("無法寫出 systemd 設定：{e}"))),
        }
    }

    /// The updater this run replaced goes back, with its own units.
    fn put_previous_back(&mut self) {
        let current = self.layout.current_updater();
        let previous = self.layout.previous_updater();
        if let Err(e) = fsx::copy_atomic(&previous, &current, 0o755, MAX_UPDATER_BYTES) {
            self.log(format!("putting the previous updater back: {e}"));
            return;
        }
        if let Err(e) = self.host.install_units(&current) {
            self.log(format!(
                "the previous updater could not write its units either: {e}"
            ));
        }
    }

    /// Remove the images of the repositories this stack uses, or of
    /// `also_repos`, that neither a container, the releases in `releases`
    /// nor the references in `j.keep` use.
    fn prune_images(&mut self, j: &Journal, releases: &[&Release], also_repos: &[String]) {
        let registry = match self.read_env(&self.stack_files()) {
            Ok(env) => env.registry(),
            Err(e) => {
                self.log(format!("images left as they are: {e}"));
                return;
            }
        };
        let containers = self.host.containers().unwrap_or_default();
        let mut keep: HashSet<String> = HashSet::new();
        let mut repos: HashSet<String> = also_repos.iter().cloned().collect();
        for image in Image::ALL {
            repos.insert(envfile::repository(&registry, image));
        }
        for release in releases {
            for image in Image::ALL {
                keep.insert(release.digest(image).to_string());
            }
        }
        for reference in j
            .keep
            .iter()
            .chain(containers.iter().map(|c| &c.image_name))
        {
            if let Some((repo, digest)) = split_reference(reference) {
                keep.insert(digest.to_string());
                repos.insert(repo.to_string());
            }
        }
        keep.extend(containers.iter().map(|c| c.image_digest.clone()));

        let images = match self.host.images() {
            Ok(images) => images,
            Err(e) => {
                self.log(format!("listing images: {e}"));
                return;
            }
        };
        for image in images {
            if image_digests(&image).any(|d| keep.contains(d)) {
                continue;
            }
            let ours = image
                .repo_digests
                .iter()
                .filter_map(|r| r.split_once('@').map(|(repo, _)| repo))
                .chain(image.names.iter().map(|n| repo_of_tagged(n)))
                .any(|repo| repos.contains(repo));
            if !ours {
                continue;
            }
            if let Err(e) = self.host.remove_image(&image.id) {
                self.log(format!("removing image {}: {e}", image.id));
            }
        }
    }
}

/// Why a pull failed, short enough for the screen. A registry that refuses
/// an anonymous pull or does not have the image gets one sentence: shipped
/// machines carry no registry credentials, so either the image is not public
/// or it is gone, and neither is anything the machine can fix. Anything
/// else, such as no network, keeps podman's own words. The updater never
/// logs in to a registry.
pub fn pull_failure(what: &str, error: &str) -> String {
    const REFUSED: [&str; 6] = [
        "unauthorized",
        "authentication required",
        "denied",
        "manifest unknown",
        "name unknown",
        "not found",
    ];
    // A local file podman cannot open is not the registry refusing.
    let lower = error.to_ascii_lowercase().replace("permission denied", "");
    if REFUSED.iter().any(|word| lower.contains(word)) {
        format!("無法取得{what}：映像庫拒絕提供或沒有這個映像")
    } else {
        format!("無法下載{what}：{error}")
    }
}

fn running_container(containers: &[Container], service: &str) -> Option<String> {
    containers
        .iter()
        .filter(|c| c.service == service && c.running)
        .max_by_key(|c| c.created)
        .map(|c| c.name.clone())
}

fn image_digests(image: &crate::host::LocalImage) -> impl Iterator<Item = &str> {
    std::iter::once(image.digest.as_str()).chain(
        image
            .repo_digests
            .iter()
            .filter_map(|r| r.split_once('@').map(|(_, d)| d)),
    )
}

/// `repo[:tag]@sha256:…` into the repository, without its tag, and the
/// digest. podman pulls and runs the digest alone when both are given.
pub fn split_reference(reference: &str) -> Option<(&str, &str)> {
    let (name, digest) = reference.split_once('@')?;
    ImageDigest::parse(digest)?;
    Some((repo_of_tagged(name), digest))
}

/// `repo:tag` or `repo@digest` to `repo`, minding a registry port.
fn repo_of_tagged(name: &str) -> &str {
    if let Some((repo, _)) = name.split_once('@') {
        return repo;
    }
    match name.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => repo,
        _ => name,
    }
}

fn check_extracted(path: &Path, limit: usize) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} 不是一般檔案", path.display()));
    }
    if meta.len() as usize > limit {
        return Err(format!("{} 超過 {} 位元組", path.display(), limit));
    }
    Ok(())
}

/// A service whose newest container its restart policy has had to start
/// again and again. It will not come up however long the wait, and every
/// minute of waiting is downtime.
fn crash_loop(containers: &[Container], services: &[Service]) -> Option<String> {
    services.iter().find_map(|service| {
        let c = containers
            .iter()
            .filter(|c| c.service == service.name)
            .max_by_key(|c| c.created)?;
        (c.restarts >= CRASH_LOOP_RESTARTS).then(|| {
            format!(
                "{} 啟動後反覆結束，已重新啟動 {} 次",
                service.name, c.restarts
            )
        })
    })
}

/// The services whose names pass `keep`.
fn only(services: Vec<Service>, keep: impl Fn(&str) -> bool) -> Vec<Service> {
    services.into_iter().filter(|s| keep(&s.name)).collect()
}

fn names(services: &[Service]) -> Vec<String> {
    services.iter().map(|s| s.name.clone()).collect()
}

/// Which of this release's images a repository is, if any.
fn own_image(registry: &str, repo: &str) -> Option<Image> {
    Image::ALL
        .into_iter()
        .find(|&image| envfile::repository(registry, image) == repo)
}

/// A release's compose files define postgres, api, web and mcp, run every
/// image of the release, name every image by digest, and name the images of
/// the release itself by the manifest's digests. Checked before anything
/// stops, so a release that cannot pass [`judge`] or would leave a service
/// out is refused while the system is still serving, and by `install`.
pub fn check_release_files(
    services: &[Service],
    release: &Release,
    registry: &str,
) -> Result<(), String> {
    let required = [DATABASE, OBJECT_STORE]
        .into_iter()
        .chain(ROLES.iter().map(|(name, _)| *name));
    for required in required {
        if !services.iter().any(|s| s.name == required) {
            return Err(format!("compose 檔沒有 {required} 服務"));
        }
    }
    for (name, image) in ROLES {
        let runs = services
            .iter()
            .find(|s| s.name == name)
            .and_then(|s| split_reference(&s.image))
            .and_then(|(repo, _)| own_image(registry, repo));
        if runs != Some(image) {
            return Err(format!(
                "compose 檔的 {name} 沒有使用版本清單上的 {image} 映像"
            ));
        }
    }
    for service in services {
        let Some((repo, digest)) = split_reference(&service.image) else {
            return Err(format!(
                "新版 compose 檔的 {} 沒有以 digest 指定映像：{}",
                service.name, service.image
            ));
        };
        if let Some(image) = own_image(registry, repo) {
            if release.digest(image).to_string() != digest {
                return Err(format!(
                    "新版 compose 檔的 {} 不是版本清單上的 {image} 映像",
                    service.name
                ));
            }
        }
    }
    Ok(())
}

/// Every service running the image its compose file names, healthy where it
/// has a healthcheck, and created after `since` when that is given.
///
/// The compose file has to name every image by digest. The container has to
/// have been created from that same digest and has to run it: podman reports
/// the index digest a multi-platform image was pulled by. The images of the
/// release itself must also carry the manifest's digest. A tag, a container
/// created from another reference, or one left over from before the switch,
/// never passes.
pub fn judge(
    containers: &[Container],
    services: &[Service],
    release: Option<&Release>,
    registry: &str,
    since: Option<DateTime<Utc>>,
) -> Result<(), String> {
    for service in services {
        let name = &service.name;
        let Some(c) = containers
            .iter()
            .filter(|c| &c.service == name)
            .max_by_key(|c| c.created)
        else {
            return Err(format!("{name} 沒有容器"));
        };
        if !c.running {
            return Err(format!("{name} 沒有在執行"));
        }
        if let Some(since) = since {
            if c.created < since - chrono::Duration::seconds(2) {
                return Err(format!("{name} 仍是切換前建立的容器"));
            }
        }
        let Some((_, wanted)) = split_reference(&service.image) else {
            return Err(format!(
                "compose 檔的 {name} 沒有以 digest 指定映像：{}",
                service.image
            ));
        };
        let Some((repo, digest)) = split_reference(&c.image_name) else {
            return Err(format!("{name} 的映像沒有以 digest 指定：{}", c.image_name));
        };
        if digest != wanted {
            return Err(format!(
                "{name} 的容器以 {digest} 建立，compose 檔指定的是 {wanted}"
            ));
        }
        let pinned = ImageDigest::parse(digest).expect("split_reference parsed it");
        if !pinned.matches(&c.image_digest) {
            return Err(format!(
                "{name} 執行的映像 {} 與指定的 {digest} 不符",
                c.image_digest
            ));
        }
        let role = ROLES
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, image)| *image);
        if let (Some(_), Some(image)) = (release, role) {
            if own_image(registry, repo) != Some(image) {
                return Err(format!("{name} 執行的不是本系統的 {image} 映像"));
            }
        }
        if let (Some(release), Some(image)) = (release, own_image(registry, repo)) {
            if release.digest(image) != pinned {
                return Err(format!(
                    "{name} 執行的 {image} 映像不是 {} 的版本",
                    release.version
                ));
            }
        }
        if c.has_healthcheck && c.health.as_deref() != Some("healthy") {
            return Err(format!(
                "{name} 尚未通過健康檢查（{}）",
                c.health.as_deref().unwrap_or("unknown")
            ));
        }
    }
    Ok(())
}

fn size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= 1024 * MB {
        format!("{:.1} GB", bytes as f64 / (1024 * MB) as f64)
    } else {
        format!("{} MB", bytes.div_ceil(MB))
    }
}
