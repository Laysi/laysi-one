//! The installed release and the journal of the update in progress.
//!
//! Both are files the updater writes and only it reads, so they hold what was
//! verified rather than the signed text: the version and the three digests.
//! The release this host runs is decided by host-side state, never by anything
//! a container can write.
//!
//! The journal is the whole update. Each phase is written, atomically, before
//! the work it names begins or after the work it names has finished, so a run
//! that dies at any point leaves a phase that says what to do next; see
//! [`Phase`].

use std::fmt;

use chrono::{DateTime, SecondsFormat, Utc};
use laysi_license_token::release::{Image, ImageDigest, ReleaseManifest, ReleaseVersion};
use laysi_license_token::update::RequestId;

/// A release as this host records it: what a verified manifest said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: ReleaseVersion,
    pub digests: [ImageDigest; 3],
}

impl Release {
    pub fn of(manifest: &ReleaseManifest) -> Release {
        Release {
            version: manifest.version().clone(),
            digests: Image::ALL.map(|image| manifest.digest(image)),
        }
    }

    pub fn digest(&self, image: Image) -> ImageDigest {
        self.digests[image as usize]
    }

    fn lines(&self, prefix: &str, out: &mut String) {
        out.push_str(&format!("{prefix}version {}\n", self.version));
        for image in Image::ALL {
            out.push_str(&format!("{prefix}image {} {}\n", image, self.digest(image)));
        }
    }

    fn read(prefix: &str, fields: &Fields) -> Result<Release, String> {
        let version = fields.one(&format!("{prefix}version"))?;
        let version =
            ReleaseVersion::parse(version).map_err(|_| format!("bad {prefix}version {version}"))?;
        let mut digests = Vec::new();
        for image in Image::ALL {
            let key = format!("{prefix}image {image}");
            let digest = fields.one(&key)?;
            digests.push(ImageDigest::parse(digest).ok_or_else(|| format!("bad {key}"))?);
        }
        Ok(Release {
            version,
            digests: [digests[0], digests[1], digests[2]],
        })
    }
}

const INSTALLED_HEADER: &str = "laysi-updater-installed 1";
const JOURNAL_HEADER: &str = "laysi-updater-journal 1";

/// `state/installed`.
pub fn encode_installed(release: &Release) -> String {
    let mut out = format!("{INSTALLED_HEADER}\n");
    release.lines("", &mut out);
    out
}

pub fn decode_installed(text: &str) -> Result<Release, String> {
    let fields = Fields::parse(text, INSTALLED_HEADER)?;
    Release::read("", &fields)
}

/// Where an update stands. Written to disk before the next step starts.
///
/// What a run that finds the journal in each phase does, at boot or when the
/// next request arrives (see `Machine::recover`):
///
/// | phase | the step it names | after an interruption |
/// |---|---|---|
/// | `Preparing` | space checked, pulling and extracting | nothing changed: report and drop |
/// | `Prepared` | new images and files are on disk | same |
/// | `Stopping` | stopping everything but postgres, then the dump | return: start the old version |
/// | `BackedUp` | dump written and checked | same, and delete the dump |
/// | `FilesSaved` | `.previous` copies written | restore the files first, then the same |
/// | `FilesSwapped` | new compose files and `.env` in place | same |
/// | `Switching` | new containers being created | finish the switch, then judge it |
/// | `CoreUp` | everything but web and mcp passed | same as `Switching` |
/// | `Committed` | every service passed; new release installed | replace the updater, clean up |
/// | `Finishing` | updater replaced or kept | clean up and report |
/// | `RollingBack` | stopping, restoring files, old postgres | start the rollback over |
/// | `Restoring` | `pg_restore` into the old postgres; after one failure, the retry | restore again from the dump; a retry begun is resumed, never a second one begun |
/// | `Restored` | database back as it was | start the old version and judge it |
/// | `RollbackFailed` | final | only start the stack |
///
/// The line between returning and finishing is `Switching`: before it the
/// database has seen only the old version, after it the new version may have
/// migrated it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Preparing,
    Prepared,
    Stopping,
    BackedUp,
    FilesSaved,
    FilesSwapped,
    Switching,
    CoreUp,
    Committed,
    Finishing,
    RollingBack,
    Restoring,
    Restored,
    RollbackFailed,
}

impl Phase {
    const ALL: [Phase; 14] = [
        Phase::Preparing,
        Phase::Prepared,
        Phase::Stopping,
        Phase::BackedUp,
        Phase::FilesSaved,
        Phase::FilesSwapped,
        Phase::Switching,
        Phase::CoreUp,
        Phase::Committed,
        Phase::Finishing,
        Phase::RollingBack,
        Phase::Restoring,
        Phase::Restored,
        Phase::RollbackFailed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Phase::Preparing => "preparing",
            Phase::Prepared => "prepared",
            Phase::Stopping => "stopping",
            Phase::BackedUp => "backed-up",
            Phase::FilesSaved => "files-saved",
            Phase::FilesSwapped => "files-swapped",
            Phase::Switching => "switching",
            Phase::CoreUp => "core-up",
            Phase::Committed => "committed",
            Phase::Finishing => "finishing",
            Phase::RollingBack => "rolling-back",
            Phase::Restoring => "restoring",
            Phase::Restored => "restored",
            Phase::RollbackFailed => "rollback-failed",
        }
    }

    pub fn from_name(name: &str) -> Option<Phase> {
        Phase::ALL.into_iter().find(|p| p.name() == name)
    }

    #[cfg(test)]
    pub fn all() -> &'static [Phase] {
        &Phase::ALL
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journal {
    pub phase: Phase,
    pub request: RequestId,
    pub received_at: DateTime<Utc>,
    /// Installed when the update started; what a rollback returns to.
    pub from: Release,
    /// What the request offered.
    pub to: Release,
    pub downtime_from: Option<DateTime<Utc>>,
    pub downtime_to: Option<DateTime<Utc>>,
    /// When the containers being judged were created from. A container older
    /// than this is left over from before, whatever it reports.
    pub switch_started: Option<DateTime<Utc>>,
    /// When web and mcp were first started on the new release. From then on
    /// the new release could take writes, which a rollback restores away;
    /// kept across interruptions so the result says how long that was.
    pub entries_opened: Option<DateTime<Utc>>,
    /// The dump, a file name inside the backup directory.
    pub backup: Option<String>,
    /// Image references the stack ran before the switch, kept for a later
    /// rollback by hand and never deleted by the clean-up.
    pub keep: Vec<String>,
    /// Rolling back has to put the previous stack files back.
    pub restore_files: bool,
    /// Rolling back has to restore the database from `backup`.
    pub restore_db: bool,
    /// The current updater binary is being, or has been, replaced by the
    /// new release's.
    pub updater_swapped: bool,
    pub reason: Option<String>,
    pub rollback_reason: Option<String>,
    /// Why the first restore of the rollback failed. Written before the one
    /// retry starts, so a run that finds it set resumes that retry, and a
    /// retry that fails ends the rollback.
    pub restore_retry: Option<String>,
    /// Why the new updater was not installed, when it was not.
    pub updater_note: Option<String>,
}

impl Journal {
    pub fn new(
        request: RequestId,
        received_at: DateTime<Utc>,
        from: Release,
        to: Release,
    ) -> Journal {
        Journal {
            phase: Phase::Preparing,
            request,
            received_at,
            from,
            to,
            downtime_from: None,
            downtime_to: None,
            switch_started: None,
            entries_opened: None,
            backup: None,
            keep: Vec::new(),
            restore_files: false,
            restore_db: false,
            updater_swapped: false,
            reason: None,
            rollback_reason: None,
            restore_retry: None,
            updater_note: None,
        }
    }

    pub fn encode(&self) -> String {
        let mut out = format!(
            "{JOURNAL_HEADER}\nphase {}\nrequest {}\nreceived {}\n",
            self.phase,
            self.request,
            stamp(&self.received_at)
        );
        self.from.lines("from-", &mut out);
        self.to.lines("to-", &mut out);
        let mut line = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                out.push_str(&format!("{key} {value}\n"));
            }
        };
        line("downtime-from", self.downtime_from.as_ref().map(stamp));
        line("downtime-to", self.downtime_to.as_ref().map(stamp));
        line("switch-started", self.switch_started.as_ref().map(stamp));
        line("entries-opened", self.entries_opened.as_ref().map(stamp));
        line("backup", self.backup.clone());
        line("restore-files", Some(yes_no(self.restore_files)));
        line("restore-db", Some(yes_no(self.restore_db)));
        line("updater-swapped", Some(yes_no(self.updater_swapped)));
        line("reason", self.reason.as_deref().map(single_line));
        line(
            "rollback-reason",
            self.rollback_reason.as_deref().map(single_line),
        );
        line(
            "restore-retry",
            self.restore_retry.as_deref().map(single_line),
        );
        line(
            "updater-note",
            self.updater_note.as_deref().map(single_line),
        );
        for reference in &self.keep {
            out.push_str(&format!("keep {reference}\n"));
        }
        out
    }

    pub fn decode(text: &str) -> Result<Journal, String> {
        let fields = Fields::parse(text, JOURNAL_HEADER)?;
        let phase = fields.one("phase")?;
        let time = |key: &str| -> Result<Option<DateTime<Utc>>, String> {
            fields
                .optional(key)?
                .map(|v| {
                    DateTime::parse_from_rfc3339(v)
                        .map(|t| t.with_timezone(&Utc))
                        .map_err(|_| format!("bad {key}"))
                })
                .transpose()
        };
        // Always written, so a journal without one is damaged rather than a
        // journal that says no.
        let flag = |key: &str| -> Result<bool, String> {
            match fields.one(key)? {
                "no" => Ok(false),
                "yes" => Ok(true),
                other => Err(format!("bad {key} {other}")),
            }
        };
        Ok(Journal {
            phase: Phase::from_name(phase).ok_or_else(|| format!("unknown phase {phase}"))?,
            request: RequestId::parse(fields.one("request")?).map_err(|e| e.to_string())?,
            received_at: time("received")?.ok_or("missing received")?,
            from: Release::read("from-", &fields)?,
            to: Release::read("to-", &fields)?,
            downtime_from: time("downtime-from")?,
            downtime_to: time("downtime-to")?,
            switch_started: time("switch-started")?,
            entries_opened: time("entries-opened")?,
            backup: fields.optional("backup")?.map(str::to_string),
            keep: fields.all("keep").map(str::to_string).collect(),
            restore_files: flag("restore-files")?,
            restore_db: flag("restore-db")?,
            // Added after the first journals: a journal without it comes
            // from an updater that never replaced itself mid-step.
            updater_swapped: match fields.optional("updater-swapped")? {
                None => false,
                Some(_) => flag("updater-swapped")?,
            },
            reason: fields.optional("reason")?.map(str::to_string),
            rollback_reason: fields.optional("rollback-reason")?.map(str::to_string),
            // An updater from before the retry skips this line and restores
            // again as if the retry had not begun.
            restore_retry: fields.optional("restore-retry")?.map(str::to_string),
            updater_note: fields.optional("updater-note")?.map(str::to_string),
        })
    }
}

fn yes_no(flag: bool) -> String {
    if flag { "yes" } else { "no" }.to_string()
}

fn stamp(at: &DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn single_line(text: &str) -> String {
    let line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(1000)
        .collect();
    match line.trim() {
        "" => "-".to_string(),
        trimmed => trimmed.to_string(),
    }
}

/// `key value` lines after a header. A key is everything before the last
/// space-separated value, so `from-image api sha256:…` is the key
/// `from-image api`.
struct Fields<'a> {
    pairs: Vec<(&'a str, &'a str)>,
}

impl<'a> Fields<'a> {
    fn parse(text: &'a str, header: &str) -> Result<Fields<'a>, String> {
        let mut lines = text.split('\n');
        if lines.next() != Some(header) {
            return Err(format!("not a {header} file"));
        }
        if !text.ends_with('\n') {
            return Err("truncated".into());
        }
        let mut pairs = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let split = if line.starts_with("from-image ")
                || line.starts_with("to-image ")
                || line.starts_with("image ")
            {
                line.rsplit_once(' ')
            } else {
                line.split_once(' ')
            };
            pairs.push(split.ok_or_else(|| format!("malformed line {line:?}"))?);
        }
        Ok(Fields { pairs })
    }

    fn all<'k>(&'k self, key: &'k str) -> impl Iterator<Item = &'a str> + 'k {
        self.pairs
            .iter()
            .filter(move |(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    fn optional(&self, key: &str) -> Result<Option<&'a str>, String> {
        let mut values = self.all(key);
        let first = values.next();
        if values.next().is_some() {
            return Err(format!("{key} appears twice"));
        }
        Ok(first)
    }

    fn one(&self, key: &str) -> Result<&'a str, String> {
        self.optional(key)?.ok_or_else(|| format!("missing {key}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    pub fn release(version: &str, seed: u8) -> Release {
        let digest = |n: u8| {
            ImageDigest::parse(&format!("sha256:{}", format!("{n:02x}").repeat(32))).unwrap()
        };
        Release {
            version: ReleaseVersion::parse(version).unwrap(),
            digests: [digest(seed), digest(seed + 1), digest(seed + 2)],
        }
    }

    #[test]
    fn installed_round_trips() {
        let r = release("v0.1.13", 1);
        assert_eq!(decode_installed(&encode_installed(&r)).unwrap(), r);
        assert!(decode_installed("laysi-updater-installed 1\nversion v0.1.13\n").is_err());
        assert!(decode_installed(&encode_installed(&r).replace("v0.1.13", "0.1.13")).is_err());
        assert!(decode_installed(encode_installed(&r).trim_end()).is_err());
    }

    #[test]
    fn a_journal_round_trips_in_every_phase() {
        let at = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        for &phase in Phase::all() {
            let mut j = Journal::new(
                RequestId::parse("abcdefgh-1234").unwrap(),
                at,
                release("v0.1.13", 1),
                release("v0.1.14", 7),
            );
            j.phase = phase;
            j.downtime_from = Some(at + chrono::Duration::nanoseconds(123_456_789));
            j.switch_started = Some(at);
            j.entries_opened = Some(at + chrono::Duration::seconds(30));
            j.backup = Some("abcdefgh-1234.dump".into());
            j.keep = vec![
                "docker.io/library/postgres@sha256:aa".into(),
                "docker.io/rustfs/rustfs@sha256:bb".into(),
            ];
            j.restore_files = true;
            j.restore_db = phase >= Phase::Switching;
            j.updater_swapped = phase >= Phase::Committed;
            j.reason = Some("api 沒有通過\n健康檢查".into());
            if phase == Phase::Restoring {
                j.restore_retry = Some("還原資料庫失敗：磁碟已滿".into());
            }
            assert_eq!(Phase::from_name(phase.name()), Some(phase));
            let back = Journal::decode(&j.encode()).unwrap();
            assert_eq!(back.reason.as_deref(), Some("api 沒有通過 健康檢查"));
            j.reason = back.reason.clone();
            assert_eq!(back, j);
        }
    }

    #[test]
    fn a_damaged_journal_is_an_error_not_a_guess() {
        let at = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        let j = Journal::new(
            RequestId::parse("abcdefgh-1234").unwrap(),
            at,
            release("v0.1.13", 1),
            release("v0.1.14", 7),
        );
        let text = j.encode();
        assert!(Journal::decode(&text[..text.len() / 2]).is_err());
        assert!(Journal::decode(&text.replace("phase preparing", "phase dancing")).is_err());
        assert!(Journal::decode(&format!("{text}phase committed\n")).is_err());
        assert!(Journal::decode("").is_err());
        for key in ["restore-files ", "restore-db "] {
            let without: String = text
                .lines()
                .filter(|l| !l.starts_with(key))
                .map(|l| format!("{l}\n"))
                .collect();
            assert!(Journal::decode(&without).is_err(), "{key}");
        }
        let older: String = text
            .lines()
            .filter(|l| !l.starts_with("updater-swapped "))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(!Journal::decode(&older).unwrap().updater_swapped);
    }
}
