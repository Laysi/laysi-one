//! A host made of memory, for the state machine's tests.
//!
//! Containers, images, the database and the secrets volume are fields of a
//! [`World`]; the updater's own files are real files in a temporary
//! directory. Every call is a point where the power can go: with `crash_at`
//! set, the call with that number panics, after doing whatever part of its
//! work a real host could have done by then. The world outlives the panic,
//! the way the disks outlive a power cut, and [`World::reboot`] stops every
//! container the way a power cut does.
//!
//! The fake compose format is one line per service, later files overriding
//! earlier ones:
//!
//! ```text
//! service api ${LAYSI_REGISTRY}/laysi-one-api@${LAYSI_API_DIGEST}
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::envfile::EnvFile;
use crate::host::{Container, Host, HostResult, LocalImage, Service, StackFiles};
use crate::machine::{DATABASE, ENTRIES, SECRETS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    /// Runs and passes its healthcheck.
    Healthy,
    /// Runs and has no healthcheck.
    Plain,
    /// Exits as soon as it starts.
    ExitsAtStart,
    /// Runs, never passes its healthcheck.
    NeverHealthy,
    /// Runs to completion with exit code 0.
    Oneshot,
}

#[derive(Debug, Clone)]
pub struct ImageSpec {
    pub repo: String,
    /// Which release it belongs to, for the mixing check.
    pub release: &'static str,
    pub behaviour: Behaviour,
    /// A migration it applies to the database when it starts.
    pub migrates: Option<&'static str>,
    /// A secret it adds to the volume when it runs.
    pub adds_secret: Option<&'static str>,
    pub files: BTreeMap<String, Vec<u8>>,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Db {
    pub migrations: BTreeSet<String>,
    pub rows: Vec<String>,
}

impl Db {
    fn encode(&self) -> String {
        let m: Vec<_> = self.migrations.iter().cloned().collect();
        format!("fake-dump\n{}\n{}\n", m.join(","), self.rows.join(","))
    }

    fn decode(text: &str) -> Option<Db> {
        let mut lines = text.lines();
        (lines.next()? == "fake-dump").then_some(())?;
        let split = |l: &str| -> Vec<String> {
            l.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        Some(Db {
            migrations: split(lines.next()?).into_iter().collect(),
            rows: split(lines.next()?),
        })
    }
}

#[derive(Debug, Clone)]
pub struct FakeContainer {
    pub reference: String,
    pub digest: String,
    pub running: bool,
    pub exit_code: Option<i64>,
    pub created: DateTime<Utc>,
}

#[derive(Debug)]
pub struct World {
    pub clock: DateTime<Utc>,
    pub calls: usize,
    pub crash_at: Option<usize>,
    /// Cut the power at the first call with this label, e.g. a journal phase.
    pub crash_on: Option<&'static str>,
    /// What a pull can fetch, by digest.
    pub registry: BTreeMap<String, ImageSpec>,
    pub local: BTreeSet<String>,
    pub containers: BTreeMap<String, FakeContainer>,
    pub db: Option<Db>,
    pub secrets: BTreeSet<String>,
    pub free: u64,
    pub fail_restore: bool,
    /// An image whose containers ignore `podman stop`.
    pub unstoppable: Option<String>,
    pub restores: usize,
    pub dumps: usize,
    pub pulls: usize,
    /// Moments an entry point ran while the rest of the stack was not proven.
    pub violations: Vec<String>,
}

impl World {
    pub fn new() -> World {
        World {
            clock: Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap(),
            calls: 0,
            crash_at: None,
            crash_on: None,
            registry: BTreeMap::new(),
            local: BTreeSet::new(),
            containers: BTreeMap::new(),
            db: Some(Db {
                migrations: ["m1".to_string()].into(),
                rows: vec!["written-before-the-update".into()],
            }),
            secrets: ["pg_password".to_string(), "setup_token".to_string()].into(),
            free: 100 * 1024 * 1024 * 1024,
            fail_restore: false,
            unstoppable: None,
            restores: 0,
            dumps: 0,
            pulls: 0,
            violations: Vec::new(),
        }
    }

    /// Power cut: nothing runs, time moves on.
    pub fn reboot(&mut self) {
        for c in self.containers.values_mut() {
            c.running = false;
        }
        self.clock += Duration::seconds(60);
        self.crash_at = None;
    }

    /// Only the updater died: containers keep running, time moves on.
    pub fn restart_updater(&mut self) {
        self.clock += Duration::seconds(60);
        self.crash_at = None;
    }

    pub fn spec_of(&self, service: &str) -> Option<&ImageSpec> {
        self.containers
            .get(service)
            .and_then(|c| self.registry.get(&c.digest))
    }

    fn healthy(&self, service: &str) -> bool {
        let Some(c) = self.containers.get(service) else {
            return false;
        };
        let Some(spec) = self.registry.get(&c.digest) else {
            return false;
        };
        c.running && matches!(spec.behaviour, Behaviour::Healthy | Behaviour::Plain)
    }
}

pub struct FakeHost {
    pub world: Rc<RefCell<World>>,
}

impl FakeHost {
    fn tick(&mut self, what: &str) {
        let mut w = self.world.borrow_mut();
        w.calls += 1;
        w.clock += Duration::seconds(1);
        if w.crash_at == Some(w.calls) {
            panic!("power cut at call {} ({what})", w.calls);
        }
        if w.crash_on == Some(what) {
            w.crash_on = None;
            panic!("power cut at {what}");
        }
    }

    fn services(files: &StackFiles) -> HostResult<Vec<(String, String)>> {
        let env = EnvFile::parse(&std::fs::read_to_string(&files[2]).map_err(|e| e.to_string())?);
        let mut services: Vec<(String, String)> = Vec::new();
        for file in &files[..2] {
            let text =
                std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
            for line in text.lines() {
                let mut parts = line.split_whitespace();
                if parts.next() != Some("service") {
                    continue;
                }
                let (Some(name), Some(template)) = (parts.next(), parts.next()) else {
                    return Err(format!("bad line {line}"));
                };
                let mut reference = template.to_string();
                while let Some(start) = reference.find("${") {
                    let end = reference[start..].find('}').ok_or("unclosed")? + start;
                    let key = &reference[start + 2..end];
                    let value = env
                        .get(key)
                        .filter(|v| !v.is_empty())
                        .ok_or_else(|| format!("required variable {key} is missing a value"))?;
                    reference.replace_range(start..=end, value);
                }
                match services.iter_mut().find(|(n, _)| n == name) {
                    Some(entry) => entry.1 = reference,
                    None => services.push((name.to_string(), reference)),
                }
            }
        }
        Ok(services)
    }

    fn digest_of(reference: &str) -> HostResult<String> {
        reference
            .split_once('@')
            .map(|(_, d)| d.to_string())
            .ok_or_else(|| format!("{reference} is not pinned"))
    }

    fn check_entries(&mut self) {
        let mut w = self.world.borrow_mut();
        let entry_running = ENTRIES
            .iter()
            .any(|e| w.containers.get(*e).is_some_and(|c| c.running));
        if !entry_running {
            return;
        }
        let mut problems = Vec::new();
        let names: Vec<String> = w.containers.keys().cloned().collect();
        let mut releases = BTreeSet::new();
        for name in names.iter().filter(|n| n.as_str() != SECRETS) {
            let running = w.containers.get(name).is_some_and(|c| c.running);
            if let Some(spec) = w.spec_of(name) {
                if running && !spec.release.is_empty() {
                    releases.insert(spec.release);
                }
            }
            if !ENTRIES.contains(&name.as_str()) && !w.healthy(name) {
                problems.push(format!("{name} not healthy"));
            }
        }
        if releases.len() > 1 {
            problems.push(format!("mixed releases {releases:?}"));
        }
        if !problems.is_empty() {
            let at = w.calls;
            w.violations
                .push(format!("call {at}: entry running while {problems:?}"));
        }
    }
}

impl Host for FakeHost {
    fn now(&mut self) -> DateTime<Utc> {
        self.world.borrow().clock
    }

    fn sleep(&mut self, seconds: u64) {
        self.world.borrow_mut().clock += Duration::seconds(seconds as i64);
    }

    fn checkpoint(&mut self, label: &str) {
        self.tick(label);
    }

    fn pull(&mut self, reference: &str) -> HostResult<()> {
        self.tick("pull");
        let digest = FakeHost::digest_of(reference)?;
        let mut w = self.world.borrow_mut();
        match w.registry.get(&digest) {
            Some(spec) if reference.starts_with(&spec.repo) => {
                w.local.insert(digest);
                w.pulls += 1;
                Ok(())
            }
            _ => Err(format!("manifest unknown: {reference}")),
        }
    }

    fn extract(&mut self, image: &str, files: &[(&str, &Path)]) -> HostResult<()> {
        self.tick("extract");
        let digest = FakeHost::digest_of(image)?;
        let w = self.world.borrow();
        if !w.local.contains(&digest) {
            return Err(format!("{image} is not pulled"));
        }
        let spec = w.registry.get(&digest).ok_or("no such image")?;
        for (inside, dest) in files {
            let bytes = spec
                .files
                .get(*inside)
                .ok_or_else(|| format!("{inside}: no such file in {image}"))?;
            std::fs::write(dest, bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn compose_pull(&mut self, files: &StackFiles) -> HostResult<()> {
        for (_, reference) in FakeHost::services(files)? {
            self.pull(&reference)?;
        }
        Ok(())
    }

    fn compose_config(&mut self, files: &StackFiles) -> HostResult<Vec<Service>> {
        self.tick("config");
        Ok(FakeHost::services(files)?
            .into_iter()
            .map(|(name, image)| Service { name, image })
            .collect())
    }

    fn compose_up(
        &mut self,
        files: &StackFiles,
        services: &[String],
        recreate: bool,
    ) -> HostResult<()> {
        let defined = FakeHost::services(files)?;
        for service in services {
            self.tick("up");
            let reference = defined
                .iter()
                .find(|(n, _)| n == service)
                .map(|(_, r)| r.clone())
                .ok_or_else(|| format!("no such service {service}"))?;
            let digest = FakeHost::digest_of(&reference)?;
            {
                let mut w = self.world.borrow_mut();
                let spec = w.registry.get(&digest).cloned().ok_or("image not found")?;
                w.local.insert(digest.clone());
                let now = w.clock;
                let fresh = recreate
                    || w.containers
                        .get(service)
                        .is_none_or(|c| c.reference != reference);
                if fresh {
                    w.containers.insert(
                        service.clone(),
                        FakeContainer {
                            reference: reference.clone(),
                            digest: digest.clone(),
                            running: false,
                            exit_code: None,
                            created: now,
                        },
                    );
                }
                let running = matches!(
                    spec.behaviour,
                    Behaviour::Healthy | Behaviour::Plain | Behaviour::NeverHealthy
                );
                let c = w.containers.get_mut(service).unwrap();
                c.running = running;
                c.exit_code = (!running).then_some(match spec.behaviour {
                    Behaviour::Oneshot => 0,
                    _ => 1,
                });
                if let Some(m) = spec.migrates {
                    if let Some(db) = w.db.as_mut() {
                        if service != DATABASE {
                            db.migrations.insert(m.to_string());
                        }
                    }
                }
                if let Some(s) = spec.adds_secret {
                    w.secrets.insert(s.to_string());
                }
            }
            self.check_entries();
        }
        Ok(())
    }

    fn containers(&mut self) -> HostResult<Vec<Container>> {
        self.tick("containers");
        let w = self.world.borrow();
        Ok(w.containers
            .iter()
            .map(|(service, c)| {
                let spec = w.registry.get(&c.digest);
                let behaviour = spec.map(|s| s.behaviour).unwrap_or(Behaviour::Plain);
                let has_healthcheck =
                    matches!(behaviour, Behaviour::Healthy | Behaviour::NeverHealthy);
                Container {
                    id: format!("id-{service}"),
                    name: format!("laysi-one-{service}"),
                    service: service.clone(),
                    running: c.running,
                    exit_code: c.exit_code,
                    // An image that exits at start is in a restart loop by
                    // the time anyone looks.
                    restarts: if behaviour == Behaviour::ExitsAtStart {
                        10
                    } else {
                        0
                    },
                    has_healthcheck,
                    health: has_healthcheck.then(|| {
                        if c.running && behaviour == Behaviour::Healthy {
                            "healthy".to_string()
                        } else {
                            "unhealthy".to_string()
                        }
                    }),
                    image_name: c.reference.clone(),
                    image_digest: c.digest.clone(),
                    created: c.created,
                }
            })
            .collect())
    }

    fn stop(&mut self, names: &[String]) -> HostResult<()> {
        for name in names {
            self.tick("stop");
            let service = name.trim_start_matches("laysi-one-");
            let mut w = self.world.borrow_mut();
            let stuck = w.unstoppable.clone();
            if let Some(c) = w.containers.get_mut(service) {
                if stuck.as_ref() == Some(&c.digest) {
                    return Err(format!("{name} did not stop"));
                }
                c.running = false;
            }
        }
        Ok(())
    }

    fn wait_exit(&mut self, name: &str) -> HostResult<i64> {
        self.tick("wait");
        let service = name.trim_start_matches("laysi-one-");
        let w = self.world.borrow();
        let c = w.containers.get(service).ok_or("no such container")?;
        c.exit_code
            .ok_or_else(|| format!("{name} is still running"))
    }

    fn database_size(&mut self, _postgres: &str) -> HostResult<u64> {
        self.tick("database size");
        let w = self.world.borrow();
        if !w.containers.get(DATABASE).is_some_and(|c| c.running) {
            return Err("postgres is not running".into());
        }
        w.db.as_ref()
            .map(|_| 1024 * 1024)
            .ok_or("no database".into())
    }

    fn dump_database(&mut self, _postgres: &str, to: &mut File) -> HostResult<()> {
        self.tick("pg_dump");
        let text = {
            let mut w = self.world.borrow_mut();
            w.dumps += 1;
            w.db.as_ref().ok_or("database does not exist")?.encode()
        };
        // Half the dump, then the power can go.
        let half = text.len() / 2;
        to.write_all(&text.as_bytes()[..half])
            .map_err(|e| e.to_string())?;
        self.tick("pg_dump half");
        to.write_all(&text.as_bytes()[half..])
            .map_err(|e| e.to_string())
    }

    fn check_dump(&mut self, _postgres: &str, dump: &mut File) -> HostResult<()> {
        self.tick("pg_restore --list");
        let mut text = String::new();
        dump.read_to_string(&mut text).map_err(|e| e.to_string())?;
        Db::decode(&text)
            .map(drop)
            .ok_or("not a complete dump".into())
    }

    fn restore_database(&mut self, _postgres: &str, from: &mut File) -> HostResult<()> {
        self.tick("drop database");
        {
            let mut w = self.world.borrow_mut();
            if !w.containers.get(DATABASE).is_some_and(|c| c.running) {
                return Err("postgres is not running".into());
            }
            w.db = None;
            w.restores += 1;
        }
        self.tick("pg_restore");
        if self.world.borrow().fail_restore {
            return Err("pg_restore: error: could not execute query".into());
        }
        let mut text = String::new();
        from.read_to_string(&mut text).map_err(|e| e.to_string())?;
        let db = Db::decode(&text).ok_or("archive is not complete")?;
        self.world.borrow_mut().db = Some(db);
        Ok(())
    }

    fn drop_database(&mut self, _postgres: &str) -> HostResult<()> {
        self.tick("drop database");
        self.world.borrow_mut().db = None;
        Ok(())
    }

    /// What is left of `free` once the pulled images are on disk.
    fn free_bytes(&mut self, _path: &Path) -> HostResult<u64> {
        self.tick("statvfs");
        let w = self.world.borrow();
        let used: u64 = w
            .local
            .iter()
            .filter_map(|d| w.registry.get(d))
            .map(|s| s.size)
            .sum();
        Ok(w.free.saturating_sub(used))
    }

    fn same_device(&mut self, _a: &Path, _b: &Path) -> HostResult<bool> {
        Ok(true)
    }

    fn image_store(&mut self) -> HostResult<PathBuf> {
        Ok(PathBuf::from("/fake/storage"))
    }

    fn images(&mut self) -> HostResult<Vec<LocalImage>> {
        self.tick("images");
        let w = self.world.borrow();
        Ok(w.local
            .iter()
            .map(|digest| {
                let spec = w.registry.get(digest);
                LocalImage {
                    id: format!("id-{digest}"),
                    repo_digests: spec
                        .map(|s| vec![format!("{}@{digest}", s.repo)])
                        .unwrap_or_default(),
                    names: Vec::new(),
                    digest: digest.clone(),
                    size: spec.map(|s| s.size).unwrap_or(0),
                }
            })
            .collect())
    }

    fn remove_image(&mut self, id: &str) -> HostResult<()> {
        self.tick("rmi");
        let digest = id.trim_start_matches("id-").to_string();
        let mut w = self.world.borrow_mut();
        if w.containers.values().any(|c| c.digest == digest) {
            return Err("image is in use by a container".into());
        }
        w.local.remove(&digest);
        Ok(())
    }

    fn self_check(&mut self, binary: &Path) -> HostResult<()> {
        self.tick("self-check");
        let bytes = std::fs::read(binary).map_err(|e| e.to_string())?;
        if bytes.starts_with(b"broken") {
            Err("exited with 1: cannot read the journal".into())
        } else {
            Ok(())
        }
    }

    fn install_units(&mut self, binary: &Path) -> HostResult<()> {
        self.tick("write-units");
        let bytes = std::fs::read(binary).map_err(|e| e.to_string())?;
        if bytes.starts_with(b"no-units") {
            Err("cannot write units".into())
        } else {
            Ok(())
        }
    }

    fn clear_requests(&mut self, dir: &Path) -> HostResult<()> {
        self.tick("clear requests");
        crate::fsx::clear_untrusted(dir)
            .map(drop)
            .map_err(|e| e.to_string())
    }
}
