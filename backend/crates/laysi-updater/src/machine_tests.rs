//! The state machine against the fake host, including a power cut at every
//! call it makes.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use ed25519_dalek::{Signer, SigningKey};
use laysi_license_token::release::{self, Image, ImageDigest, KeyId, ReleaseVersion};
use laysi_license_token::update::{
    RequestId, ToolVersions, UpdateRequest, UpdateResult, UpdateStatus, REQUEST_FILE,
    SWITCHING_FILE,
};

use crate::envfile::EnvFile;
use crate::fake::{Behaviour, FakeContainer, FakeHost, ImageSpec, World};
use crate::fsx;
use crate::journal::{self, Phase, Release};
use crate::layout::{Layout, COMPOSE_FILES, IMAGE_SHARE, IMAGE_UPDATER};
use crate::machine::{Config, Machine};

const REGISTRY: &str = "ghcr.io/laysi";
const SERVICES: [&str; 6] = ["postgres", "rustfs", "api", "mcp", "backup", "web"];

fn digest(tag: &str) -> String {
    let hex: String = tag.bytes().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex:0>64}")
}

/// What a release is made of, for the fake registry.
#[derive(Clone)]
struct Spec {
    version: &'static str,
    api: Behaviour,
    web: Behaviour,
    migrates: &'static str,
    secrets: &'static str,
    adds_secret: Option<&'static str>,
    rustfs: &'static str,
    /// The repository the rustfs image is pulled from.
    rustfs_repo: &'static str,
    postgres: &'static str,
    updater: &'static [u8],
}

impl Spec {
    fn good(version: &'static str, migrates: &'static str) -> Spec {
        Spec {
            version,
            api: Behaviour::Healthy,
            web: Behaviour::Plain,
            migrates,
            secrets: "alpine-1",
            adds_secret: None,
            rustfs: "rustfs-1",
            rustfs_repo: "docker.io/rustfs/rustfs",
            postgres: "postgres-1",
            updater: b"updater ok",
        }
    }

    fn own(&self, image: Image) -> String {
        digest(&format!("{}-{}", image.name(), self.version))
    }

    fn release(&self) -> Release {
        Release {
            version: ReleaseVersion::parse(self.version).unwrap(),
            digests: Image::ALL.map(|i| ImageDigest::parse(&self.own(i)).unwrap()),
        }
    }

    fn compose(&self) -> String {
        format!(
            "service secrets docker.io/library/alpine@{}\n\
             service postgres docker.io/library/postgres@{}\n\
             service rustfs {}@{}\n\
             service api ${{LAYSI_REGISTRY}}/laysi-one-api@${{LAYSI_API_DIGEST}}\n\
             service mcp ${{LAYSI_REGISTRY}}/laysi-one-api@${{LAYSI_API_DIGEST}}\n\
             service backup ${{LAYSI_REGISTRY}}/laysi-one-backup@${{LAYSI_BACKUP_DIGEST}}\n\
             service web ${{LAYSI_REGISTRY}}/laysi-one-web@${{LAYSI_WEB_DIGEST}}\n",
            digest(self.secrets),
            digest(self.postgres),
            self.rustfs_repo,
            digest(self.rustfs)
        )
    }

    fn publish(&self, world: &mut World) {
        let image = |repo: &str, release: &'static str, behaviour| ImageSpec {
            repo: repo.to_string(),
            release,
            behaviour,
            migrates: None,
            adds_secret: None,
            files: BTreeMap::new(),
            size: 100 * 1024 * 1024,
        };
        let mut api = image(&format!("{REGISTRY}/laysi-one-api"), self.version, self.api);
        api.migrates = Some(self.migrates);
        api.files.insert(
            format!("{IMAGE_SHARE}/{}", COMPOSE_FILES[0]),
            self.compose().into_bytes(),
        );
        api.files.insert(
            format!("{IMAGE_SHARE}/{}", COMPOSE_FILES[1]),
            b"overlay\n".to_vec(),
        );
        api.files
            .insert(IMAGE_UPDATER.to_string(), self.updater.to_vec());
        world.registry.insert(self.own(Image::Api), api);
        world.registry.insert(
            self.own(Image::Web),
            image(&format!("{REGISTRY}/laysi-one-web"), self.version, self.web),
        );
        world.registry.insert(
            self.own(Image::Backup),
            image(
                &format!("{REGISTRY}/laysi-one-backup"),
                self.version,
                Behaviour::Plain,
            ),
        );
        let mut secrets = image("docker.io/library/alpine", "", Behaviour::Oneshot);
        secrets.adds_secret = self.adds_secret;
        world
            .registry
            .entry(digest(self.secrets))
            .or_insert(secrets);
        world
            .registry
            .entry(digest(self.postgres))
            .or_insert_with(|| image("docker.io/library/postgres", "", Behaviour::Healthy));
        world
            .registry
            .entry(digest(self.rustfs))
            .or_insert_with(|| image(self.rustfs_repo, "", Behaviour::Healthy));
    }
}

struct Rig {
    layout: Layout,
    world: Rc<RefCell<World>>,
    key: SigningKey,
    config: Config,
    specs: Vec<Spec>,
}

impl Rig {
    /// A host that runs `installed`, booted, with `offered` published.
    fn new(installed: Spec, offered: &[Spec]) -> Rig {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("laysi-updater-rig-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = Layout::new(root);
        for (dir, mode) in layout.directories() {
            fsx::ensure_dir(&dir, mode).unwrap();
        }
        let mut world = World::new();
        world.marker = Some(layout.results_dir().join(SWITCHING_FILE));
        installed.publish(&mut world);
        for spec in offered {
            spec.publish(&mut world);
        }
        // An image from a release before the installed one, which the
        // clean-up after a successful update should remove.
        let ancient = Spec::good("v0.0.9", "m0");
        ancient.publish(&mut world);
        world.local.insert(ancient.own(Image::Api));

        let stack = layout.stack_files();
        std::fs::write(&stack[0], installed.compose()).unwrap();
        std::fs::write(&stack[1], "overlay\n").unwrap();
        let env =
            EnvFile::default().for_release(&installed.release(), &layout.root.to_string_lossy());
        std::fs::write(&stack[2], env.render()).unwrap();
        std::fs::write(
            layout.installed(),
            journal::encode_installed(&installed.release()),
        )
        .unwrap();
        std::fs::write(layout.current_updater(), b"updater installed").unwrap();
        for i in Image::ALL {
            world.local.insert(installed.own(i));
        }

        let rig = Rig {
            layout,
            world: Rc::new(RefCell::new(world)),
            key: SigningKey::from_bytes(&[7; 32]),
            config: Config {
                health_timeout_secs: 60,
                entry_timeout_secs: 30,
                poll_secs: 5,
                disk_reserve_bytes: 1024,
            },
            specs: std::iter::once(installed)
                .chain(offered.iter().cloned())
                .collect(),
        };
        rig.machine(|m| m.boot())
            .expect("the installed release boots");
        rig.world.borrow_mut().violations.clear();
        rig.world.borrow_mut().marked.clear();
        rig
    }

    fn machine<T>(&self, f: impl FnOnce(&mut Machine<FakeHost>) -> T) -> T {
        let mut host = FakeHost {
            world: self.world.clone(),
        };
        let mut m = Machine::new(
            &mut host,
            self.layout.clone(),
            vec![self.key.verifying_key()],
            self.config.clone(),
        );
        f(&mut m)
    }

    fn manifest(&self, version: &str) -> String {
        let spec = self
            .specs
            .iter()
            .find(|s| s.version == version)
            .expect("published");
        signed_manifest(&self.key, spec)
    }

    fn request(&self, id: &str, manifest: &str) -> RequestId {
        let id = RequestId::parse(id).unwrap();
        let text = UpdateRequest::new(id.clone(), manifest).unwrap().encode();
        std::fs::write(self.layout.requests_dir().join(REQUEST_FILE), text).unwrap();
        id
    }

    /// What systemd does when a request appears: the run, then
    /// `ExecStopPost=`.
    fn run(&self) -> Result<(), String> {
        let outcome = self.machine(|m| m.run_request());
        let failed = outcome.is_err();
        self.machine(|m| m.discard_requests(failed)).unwrap();
        outcome
    }

    fn result(&self, id: &RequestId) -> UpdateResult {
        self.machine(|m| m.read_result(id))
            .expect("a result was written")
    }

    fn installed(&self) -> String {
        self.machine(|m| m.load_installed())
            .unwrap()
            .version
            .to_string()
    }

    fn spec(&self, version: &str) -> &Spec {
        self.specs.iter().find(|s| s.version == version).unwrap()
    }

    fn backups(&self) -> Vec<String> {
        std::fs::read_dir(self.layout.backup_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// Whether a container of `service` exists and runs.
    fn running(&self, service: &str) -> bool {
        self.world
            .borrow()
            .containers
            .get(service)
            .is_some_and(|c| c.running)
    }

    /// What api and mcp ask before every write.
    fn switching(&self) -> bool {
        laysi_license_token::update::switching_in(&self.layout.results_dir())
    }

    fn requests_left(&self) -> usize {
        std::fs::read_dir(self.layout.requests_dir())
            .unwrap()
            .count()
    }

    /// Every service runs, all of one release, the database matches it, and
    /// nothing was left behind.
    fn assert_settled(&self, id: &RequestId) {
        let installed = self.installed();
        let spec = self.spec(&installed).clone();
        let w = self.world.borrow();
        for service in SERVICES {
            let c = w
                .containers
                .get(service)
                .unwrap_or_else(|| panic!("{service} has no container"));
            assert!(c.running, "{service} is not running ({installed})");
            let image = w.registry.get(&c.digest).unwrap();
            if !image.release.is_empty() {
                assert_eq!(image.release, installed, "{service} runs another release");
            }
        }
        assert_eq!(w.containers["rustfs"].digest, digest(spec.rustfs));
        assert_eq!(w.containers["postgres"].digest, digest(spec.postgres));
        let db = w.db.as_ref().expect("the database exists");
        assert!(db.rows.contains(&"written-before-the-update".to_string()));
        let expected: Vec<&str> = self
            .specs
            .iter()
            .filter(|s| {
                ReleaseVersion::parse(s.version).unwrap()
                    <= ReleaseVersion::parse(&installed).unwrap()
            })
            .map(|s| s.migrates)
            .collect();
        let mut have: Vec<&str> = db.migrations.iter().map(String::as_str).collect();
        have.sort();
        let mut want = expected.clone();
        want.sort();
        want.dedup();
        assert_eq!(have, want, "the database matches {installed}");
        assert!(w.violations.is_empty(), "{:?}", w.violations);
        drop(w);
        assert!(
            self.backups().is_empty(),
            "backups left: {:?}",
            self.backups()
        );
        assert!(self.machine(|m| m.load_journal()).unwrap().is_none());
        let result = self.result(id);
        assert!(result.status.is_final(), "{:?}", result.status);
        let running = result.running.as_ref().map(|v| v.to_string());
        assert_eq!(running.as_deref(), Some(installed.as_str()), "{:?}", result);
        assert_eq!(result.tools, Some(ToolVersions::new("4.9.3", "1.6.0")));
        assert_eq!(self.requests_left(), 0);
        assert!(!self.switching(), "the switching marker is left");
    }

    /// Whether every stop and every start of web or mcp since the rig was
    /// built happened with the marker in place, and there was at least one.
    fn all_marked(&self) -> bool {
        let w = self.world.borrow();
        !w.marked.is_empty() && w.marked.iter().all(|(_, present)| *present)
    }

    /// How often this request's log says a retry was begun.
    fn retries_begun(&self, id: &str) -> usize {
        std::fs::read_to_string(self.layout.log())
            .unwrap_or_default()
            .lines()
            .filter(|l| {
                l.contains(&format!("request {id}:")) && l.contains("retrying the restore once")
            })
            .count()
    }
}

fn signed_manifest(key: &SigningKey, spec: &Spec) -> String {
    let body = format!(
        "laysi-release-manifest 1\nversion {}\nimage api {}\nimage web {}\nimage backup {}\n",
        spec.version,
        spec.own(Image::Api),
        spec.own(Image::Web),
        spec.own(Image::Backup)
    );
    let sig: String = key
        .sign(body.as_bytes())
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "{body}signature {} {sig}\n",
        KeyId::of(&key.verifying_key())
    )
}

fn v1() -> Spec {
    Spec::good("v0.1.0", "m1")
}

fn v2() -> Spec {
    Spec::good("v0.2.0", "m2")
}

#[test]
fn an_update_installs_the_new_release() {
    let rig = Rig::new(v1(), &[v2()]);
    let id = rig.request("req-success-1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();

    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::Succeeded, "{result:?}");
    assert_eq!(rig.installed(), "v0.2.0");
    rig.assert_settled(&id);
    // Every service stopped, and web and mcp started on the new release,
    // while api and mcp would refuse writes.
    assert!(rig.all_marked(), "{:?}", rig.world.borrow().marked);
    let (from, to) = (result.downtime_from.unwrap(), result.downtime_to.unwrap());
    assert!(from < to);
    assert_eq!(result.offered.unwrap().to_string(), "v0.2.0");

    // The updater was replaced and the previous one kept.
    assert_eq!(
        std::fs::read(rig.layout.current_updater()).unwrap(),
        b"updater ok"
    );
    assert_eq!(
        std::fs::read(rig.layout.previous_updater()).unwrap(),
        b"updater installed"
    );
    // The previous stack files are kept beside the current ones.
    let prod = rig.layout.stack_files()[0].clone();
    assert_eq!(
        std::fs::read_to_string(Layout::previous_of(&prod)).unwrap(),
        v1().compose()
    );

    // Only this release's images and the previous release's remain.
    let w = rig.world.borrow();
    for digest in &w.local {
        let release = w.registry[digest].release;
        assert!(
            release.is_empty() || release == "v0.1.0" || release == "v0.2.0",
            "{digest} from {release} survived"
        );
    }
    assert!(w.local.contains(&v1().own(Image::Api)));
    assert!(std::fs::read_dir(rig.layout.incoming_dir())
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn a_new_release_that_never_passes_is_rolled_back_with_the_database() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), &[broken]);
    let id = rig.request("req-unhealthy-1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();

    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RolledBack, "{result:?}");
    assert!(result.reason.unwrap().contains("api"));
    assert_eq!(rig.installed(), "v0.1.0");
    // The new api ran its migration; the restore took it out again, and the
    // row written before step 4 is still there.
    assert_eq!(rig.world.borrow().restores, 1);
    rig.assert_settled(&id);
    assert!(rig.all_marked(), "{:?}", rig.world.borrow().marked);
    // Neither entry point ever ran on the new release.
    let w = rig.world.borrow();
    assert!(w.violations.is_empty());
    assert_eq!(w.containers["web"].digest, v1().own(Image::Web));
}

#[test]
fn an_api_that_exits_at_start_is_rolled_back() {
    let mut broken = v2();
    broken.api = Behaviour::ExitsAtStart;
    let rig = Rig::new(v1(), &[broken]);
    let id = rig.request("req-exits-01", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RolledBack);
    rig.assert_settled(&id);
}

#[test]
fn entry_points_never_start_before_the_rest_has_passed() {
    // web is the one that fails, so the core of the new release did pass and
    // web and mcp were started on it; the rollback then has to undo them.
    let mut broken = v2();
    broken.web = Behaviour::ExitsAtStart;
    let rig = Rig::new(v1(), &[broken]);
    let id = rig.request("req-webfail-1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RolledBack);
    // web and mcp were reachable on the new release for a while, and the
    // result says that what they took in went back with the database. They
    // were refusing writes all along.
    assert!(rig.result(&id).reason.unwrap().contains("一併還原"));
    assert!(rig.all_marked(), "{:?}", rig.world.borrow().marked);
    rig.assert_settled(&id);
}

#[test]
fn when_the_old_release_does_not_come_back_either_it_stays_and_is_not_restored_again() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), &[broken]);
    // The old web image no longer starts either.
    let old_web = v1().own(Image::Web);
    rig.world
        .borrow_mut()
        .registry
        .get_mut(&old_web)
        .unwrap()
        .behaviour = Behaviour::ExitsAtStart;

    let id = rig.request("req-rbfail-1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RollbackFailed, "{result:?}");
    assert!(result.reason.is_some() && result.rollback_reason.is_some());
    assert_eq!(result.running.unwrap().to_string(), "v0.1.0");
    assert_eq!(rig.installed(), "v0.1.0");
    // The restore succeeded, so its dump is gone.
    assert!(rig.backups().is_empty());
    assert_eq!(rig.world.borrow().restores, 1);
    let journal = rig.machine(|m| m.load_journal()).unwrap().unwrap();
    assert_eq!(journal.phase, Phase::RollbackFailed);

    // Every later boot starts the stack and never restores again.
    for _ in 0..3 {
        rig.world.borrow_mut().reboot();
        let booted = rig.machine(|m| m.boot());
        assert!(booted.is_err(), "web still does not start");
        assert_eq!(rig.world.borrow().restores, 1);
        assert!(rig.world.borrow().containers["api"].running);
        assert_eq!(
            rig.world.borrow().containers["api"].digest,
            v1().own(Image::Api)
        );
    }
    assert!(rig.world.borrow().db.as_ref().unwrap().migrations.len() == 1);
}

#[test]
fn a_failed_restore_keeps_the_dump_and_blocks_further_updates() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let mut v3 = Spec::good("v0.3.0", "m3");
    v3.api = Behaviour::Healthy;
    let rig = Rig::new(v1(), &[broken, v3]);
    rig.world.borrow_mut().fail_restore = true;
    let id = rig.request("req-restorefail", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RollbackFailed);
    assert!(result.rollback_reason.unwrap().contains("備份保留"));
    assert_eq!(rig.backups(), vec!["req-restorefail.dump".to_string()]);
    assert!(rig.world.borrow().db.is_none(), "a half restore is dropped");
    // The services other than postgres stay stopped.
    assert!(!rig.running("web"));

    let next = rig.request("req-after-fail", &rig.manifest("v0.3.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&next).status, UpdateStatus::RefusedBlocked);
    assert_eq!(rig.backups(), vec!["req-restorefail.dump".to_string()]);

    // Every later boot starts postgres alone, and restores nothing.
    for _ in 0..2 {
        rig.world.borrow_mut().reboot();
        assert!(rig.machine(|m| m.boot()).is_ok());
        assert!(rig.running("postgres"));
        for service in ["api", "mcp", "web", "backup", "rustfs"] {
            assert!(!rig.running(service), "{service} started");
        }
        assert_eq!(
            rig.world.borrow().restores,
            2,
            "the first restore and its one retry"
        );
    }
}

/// A rig left in `RollbackFailed` by two failed restores, the dump kept and
/// the database gone, with v0.3.0 published for the update after.
fn failed_restore(id: &str) -> (Rig, RequestId) {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), &[broken, Spec::good("v0.3.0", "m3")]);
    rig.world.borrow_mut().fail_restore = true;
    let id = rig.request(id, &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RollbackFailed);
    assert!(rig.world.borrow().db.is_none());
    (rig, id)
}

#[test]
fn a_failed_rollback_with_its_dump_kept_is_retried_by_hand() {
    let (rig, id) = failed_restore("req-byhand-1");
    // Whatever made the restore fail is dealt with, then the command, and
    // the recovery unit it starts.
    rig.world.borrow_mut().fail_restore = false;
    assert_eq!(rig.machine(|m| m.retry_rollback()), Ok(id.clone()));
    let journal = rig.machine(|m| m.load_journal()).unwrap().unwrap();
    assert_eq!(journal.phase, Phase::Restoring);
    rig.machine(|m| m.boot()).unwrap();

    rig.assert_settled(&id);
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RolledBack);
    assert_eq!(result.running.unwrap().to_string(), "v0.1.0");
    assert!(result.rollback_reason.unwrap().contains("人工指令重新還原"));
    assert_eq!(rig.world.borrow().restores, 3);

    // The next update goes ahead. v0.2.0 never stayed, so its migration is
    // not in the database, which assert_settled would expect.
    let next = rig.request("req-byhand-2", &rig.manifest("v0.3.0"));
    rig.run().unwrap();
    let result = rig.result(&next);
    assert_eq!(result.status, UpdateStatus::Succeeded, "{result:?}");
    assert_eq!(rig.installed(), "v0.3.0");
    let w = rig.world.borrow();
    let db = w.db.as_ref().unwrap();
    assert_eq!(db.migrations.iter().collect::<Vec<_>>(), ["m1", "m3"]);
    assert!(db.rows.contains(&"written-before-the-update".to_string()));
    drop(w);
    assert!(rig.backups().is_empty());
    assert!(rig.machine(|m| m.load_journal()).unwrap().is_none());
}

#[test]
fn a_rollback_retried_by_hand_that_fails_again_keeps_the_dump() {
    let (rig, id) = failed_restore("req-byhand-3");
    rig.machine(|m| m.retry_rollback()).unwrap();
    rig.machine(|m| m.boot()).unwrap();

    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RollbackFailed, "{result:?}");
    assert!(result.rollback_reason.unwrap().contains("人工指令重新還原"));
    assert_eq!(rig.backups(), vec!["req-byhand-3.dump".to_string()]);
    assert_eq!(rig.world.borrow().restores, 4, "the restore and its retry");
    assert!(rig.running("postgres"));
    for service in ["api", "mcp", "web", "backup", "rustfs"] {
        assert!(!rig.running(service), "{service} started");
    }
    assert!(!rig.switching());
    // And it can be tried again. This time the first restore fails and its
    // retry succeeds, and the result still says the rollback was retried by
    // hand.
    rig.world.borrow_mut().fail_restore = false;
    rig.world.borrow_mut().fail_next_restores = 1;
    rig.machine(|m| m.retry_rollback()).unwrap();
    rig.machine(|m| m.boot()).unwrap();
    rig.assert_settled(&id);
    let reason = rig.result(&id).rollback_reason.unwrap();
    assert!(reason.starts_with("第一次"), "{reason}");
    assert!(reason.contains("重試後成功"), "{reason}");
    assert!(reason.contains("人工指令重新還原"), "{reason}");
}

#[test]
fn retry_rollback_refuses_all_but_a_failed_rollback_with_its_dump() {
    let phase = |rig: &Rig| rig.machine(|m| m.load_journal()).unwrap().map(|j| j.phase);

    // No update at all.
    let rig = Rig::new(v1(), &[]);
    assert!(rig.machine(|m| m.retry_rollback()).is_err());
    assert_eq!(phase(&rig), None);

    // The restore succeeded and the old release did not start: no dump.
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), &[broken]);
    rig.world
        .borrow_mut()
        .registry
        .get_mut(&v1().own(Image::Web))
        .unwrap()
        .behaviour = Behaviour::ExitsAtStart;
    let id = rig.request("req-byhand-4", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RollbackFailed);
    let refused = rig.machine(|m| m.retry_rollback()).unwrap_err();
    assert!(refused.contains("沒有留下資料庫備份"), "{refused}");
    assert_eq!(phase(&rig), Some(Phase::RollbackFailed));

    // Another run holds the lock.
    let (rig, _) = failed_restore("req-byhand-5");
    let held = fsx::Lock::acquire(&rig.layout.lock(), false)
        .unwrap()
        .unwrap();
    assert!(rig.machine(|m| m.retry_rollback()).is_err());
    drop(held);
    assert_eq!(phase(&rig), Some(Phase::RollbackFailed));

    // The stack files name another release.
    let env = EnvFile::default().for_release(&v2().release(), &rig.layout.root.to_string_lossy());
    std::fs::write(&rig.layout.stack_files()[2], env.render()).unwrap();
    let refused = rig.machine(|m| m.retry_rollback()).unwrap_err();
    assert!(refused.contains("v0.1.0"), "{refused}");
    assert_eq!(phase(&rig), Some(Phase::RollbackFailed));
    assert_eq!(rig.backups(), vec!["req-byhand-5.dump".to_string()]);
}

#[test]
fn too_little_disk_stops_before_anything_changes() {
    let rig = Rig::new(v1(), &[v2()]);
    let before = rig.world.borrow().containers.clone();
    let pulls = rig.world.borrow().pulls;
    rig.world.borrow_mut().free = 10 * 1024 * 1024;
    let id = rig.request("req-diskspace", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RefusedDiskSpace, "{result:?}");
    assert!(result.reason.unwrap().contains("磁碟"));
    let w = rig.world.borrow();
    assert_eq!(w.pulls, pulls, "nothing was pulled");
    assert_eq!(w.dumps, 0);
    for (service, c) in &w.containers {
        assert_eq!(
            c.created, before[service].created,
            "{service} was not touched"
        );
        assert_eq!(c.running, before[service].running);
    }
    drop(w);
    assert!(rig.machine(|m| m.load_journal()).unwrap().is_none());
    assert_eq!(rig.installed(), "v0.1.0");
    assert_eq!(rig.requests_left(), 0);
}

#[test]
fn refusals_are_answered_with_the_request_id() {
    let rig = Rig::new(v2(), &[v1()]);

    // Same version.
    let id = rig.request("req-same-0001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RefusedSameVersion);

    // An older release.
    let id = rig.request("req-down-0001", &rig.manifest("v0.1.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RefusedDowngrade);
    assert!(result.reason.unwrap().contains("低於"));

    // Signed by a key this build does not trust.
    let other = SigningKey::from_bytes(&[9; 32]);
    let id = rig.request("req-badsig-001", &signed_manifest(&other, &v1()));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RefusedSignature);

    // Edited after signing.
    let edited = rig.manifest("v0.1.0").replace("v0.1.0", "v0.9.0");
    let id = rig.request("req-edited-001", &edited);
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RefusedSignature);

    assert_eq!(rig.installed(), "v0.2.0");
    assert_eq!(rig.world.borrow().dumps, 0);
    assert_eq!(rig.requests_left(), 0);
}

#[test]
fn a_request_while_another_run_holds_the_lock_is_refused_as_busy() {
    let rig = Rig::new(v1(), &[v2()]);
    let held = fsx::Lock::acquire(&rig.layout.lock(), false)
        .unwrap()
        .unwrap();
    let id = rig.request("req-busy-00001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RefusedBusy);
    assert_eq!(rig.installed(), "v0.1.0");
    assert_eq!(rig.requests_left(), 0);
    drop(held);
}

#[test]
fn a_request_written_during_a_run_is_discarded_and_told_so() {
    let rig = Rig::new(v1(), &[v2()]);
    let first = rig.request("req-first-0001", &rig.manifest("v0.2.0"));
    rig.machine(|m| m.run_request()).unwrap();
    // Arrives while the run is ending; the next ExecStopPost sees it.
    let second = rig.request("req-second-001", &rig.manifest("v0.2.0"));
    rig.machine(|m| m.discard_requests(false)).unwrap();
    assert_eq!(rig.result(&first).status, UpdateStatus::Succeeded);
    assert_eq!(rig.result(&second).status, UpdateStatus::RefusedBusy);
    assert_eq!(rig.requests_left(), 0);
    assert_eq!(rig.installed(), "v0.2.0");
}

#[test]
fn a_request_a_failed_run_never_read_is_answered_as_not_started() {
    let rig = Rig::new(v1(), &[v2()]);
    // The current updater cannot start at all: the run fails without
    // touching the request, and the previous updater's ExecStopPost sees it.
    let id = rig.request("req-unread-0001", &rig.manifest("v0.2.0"));
    rig.machine(|m| m.discard_requests(true)).unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged);
    assert!(result.reason.is_some_and(|r| r.contains("開始更新前")));
    assert_eq!(rig.requests_left(), 0);
    assert_eq!(rig.installed(), "v0.1.0");
}

#[test]
fn a_request_written_during_a_failed_run_with_an_update_pending_is_busy() {
    let rig = Rig::new(v1(), &[v2()]);
    let first = rig.request("req-pending-001", &rig.manifest("v0.2.0"));
    // The final result cannot be written, so the run fails with the
    // journal kept for the recovery.
    let blocker = rig.layout.results_dir().join(first.as_str());
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "").unwrap();
    assert!(rig.machine(|m| m.run_request()).is_err());
    std::fs::remove_dir_all(&blocker).unwrap();
    let second = rig.request("req-pending-002", &rig.manifest("v0.2.0"));
    rig.machine(|m| m.discard_requests(true)).unwrap();
    assert_eq!(rig.result(&second).status, UpdateStatus::RefusedBusy);
    rig.machine(|m| m.boot()).unwrap();
    assert_eq!(rig.result(&first).status, UpdateStatus::Succeeded);
}

#[test]
fn a_request_that_is_not_a_regular_file_is_ignored_and_cleared() {
    let rig = Rig::new(v1(), &[v2()]);
    let outside = rig.layout.root.join("state/installed");
    std::os::unix::fs::symlink(&outside, rig.layout.requests_dir().join(REQUEST_FILE)).unwrap();
    rig.run().unwrap();
    assert_eq!(rig.installed(), "v0.1.0");
    assert_eq!(rig.requests_left(), 0);
    assert!(outside.exists(), "the symlink target was not touched");

    std::fs::write(
        rig.layout.requests_dir().join(REQUEST_FILE),
        "not a request",
    )
    .unwrap();
    rig.run().unwrap();
    assert_eq!(rig.requests_left(), 0);
    assert_eq!(rig.installed(), "v0.1.0");
}

#[test]
fn an_updater_that_fails_its_self_check_is_not_installed() {
    let mut next = v2();
    next.updater = b"broken updater";
    let rig = Rig::new(v1(), &[next]);
    let id = rig.request("req-selfcheck1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::SucceededUpdaterKept);
    assert!(result.reason.unwrap().contains("自我檢查"));
    assert_eq!(
        std::fs::read(rig.layout.current_updater()).unwrap(),
        b"updater installed"
    );
    assert!(!rig.layout.previous_updater().exists());
    rig.assert_settled(&id);
}

#[test]
fn an_updater_that_cannot_write_its_units_is_put_back() {
    let mut next = v2();
    next.updater = b"no-units updater";
    let rig = Rig::new(v1(), &[next]);
    let id = rig.request("req-units-0001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::SucceededUpdaterKept);
    assert_eq!(
        std::fs::read(rig.layout.current_updater()).unwrap(),
        b"updater installed"
    );
}

#[test]
fn a_new_secret_is_generated_and_the_old_ones_kept() {
    let mut next = v2();
    next.secrets = "alpine-2";
    next.adds_secret = Some("mcp_signing_key");
    let rig = Rig::new(v1(), &[next]);
    let id = rig.request("req-secret-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::Succeeded);
    let secrets = rig.world.borrow().secrets.clone();
    for s in ["pg_password", "setup_token", "mcp_signing_key"] {
        assert!(secrets.contains(s), "{s}");
    }
    rig.assert_settled(&id);
}

#[test]
fn a_release_that_only_changes_rustfs_runs_the_new_rustfs() {
    let mut next = v2();
    next.rustfs = "rustfs-2";
    let rig = Rig::new(v1(), &[next]);
    let id = rig.request("req-rustfs-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::Succeeded);
    assert_eq!(
        rig.world.borrow().containers["rustfs"].digest,
        digest("rustfs-2")
    );
    rig.assert_settled(&id);
}

#[test]
fn ten_updates_in_a_row_keep_two_releases_of_images() {
    let versions: Vec<&'static str> = vec![
        "v0.2.0", "v0.3.0", "v0.4.0", "v0.5.0", "v0.6.0", "v0.7.0", "v0.8.0", "v0.9.0", "v0.10.0",
        "v0.11.0",
    ];
    let migrations: Vec<&'static str> =
        vec!["m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9", "m10", "m11"];
    let specs: Vec<Spec> = versions
        .iter()
        .zip(&migrations)
        .map(|(v, m)| Spec::good(v, m))
        .collect();
    let rig = Rig::new(v1(), &specs);
    for (n, version) in versions.iter().enumerate() {
        let id = rig.request(&format!("req-tenfold-{n:02}"), &rig.manifest(version));
        rig.run().unwrap();
        assert_eq!(rig.result(&id).status, UpdateStatus::Succeeded, "{version}");
        assert_eq!(&rig.installed(), version);
        rig.assert_settled(&id);
        let w = rig.world.borrow();
        let releases: std::collections::BTreeSet<&str> = w
            .local
            .iter()
            .map(|d| w.registry[d].release)
            .filter(|r| !r.is_empty())
            .collect();
        assert!(releases.len() <= 2, "{releases:?}");
    }
}

/// Every image the compose files of `spec` name, Laysi's own and the
/// third-party ones.
fn images_of(spec: &Spec) -> std::collections::BTreeSet<String> {
    Image::ALL
        .into_iter()
        .map(|image| spec.own(image))
        .chain([spec.secrets, spec.postgres, spec.rustfs].map(digest))
        .collect()
}

/// Installs `releases` one after another from `installed`, and after each
/// success expects exactly the images of the running and the previous
/// release on the host: the previous one's, third-party ones included, so a
/// rollback can still start it.
fn update_through(installed: Spec, releases: &[Spec]) -> Rig {
    let rig = Rig::new(installed.clone(), releases);
    let mut previous = installed;
    for (n, spec) in releases.iter().enumerate() {
        let id = rig.request(&format!("req-images-{n:04}"), &rig.manifest(spec.version));
        rig.run().unwrap();
        assert_eq!(
            rig.result(&id).status,
            UpdateStatus::Succeeded,
            "{}",
            spec.version
        );
        rig.assert_settled(&id);
        let left = rig.world.borrow().local.clone();
        let wanted: std::collections::BTreeSet<String> = images_of(&previous)
            .union(&images_of(spec))
            .cloned()
            .collect();
        assert_eq!(left, wanted, "images after {}", spec.version);
        previous = spec.clone();
    }
    rig
}

#[test]
fn third_party_images_older_than_the_previous_release_are_removed() {
    let mut second = v2();
    second.secrets = "alpine-2";
    second.postgres = "postgres-2";
    second.rustfs = "rustfs-2";
    let mut third = Spec::good("v0.3.0", "m3");
    third.secrets = "alpine-3";
    third.postgres = "postgres-3";
    third.rustfs = "rustfs-3";
    let rig = update_through(v1(), &[second, third]);
    let w = rig.world.borrow();
    for gone in ["alpine-1", "postgres-1", "rustfs-1"] {
        assert!(!w.local.contains(&digest(gone)), "{gone} was kept");
    }
}

#[test]
fn a_third_party_image_whose_repository_changed_is_removed_two_releases_later() {
    let mut second = v2();
    second.rustfs = "rustfs-2";
    second.rustfs_repo = "ghcr.io/rustfs/rustfs";
    let mut third = Spec::good("v0.3.0", "m3");
    third.rustfs = "rustfs-2";
    third.rustfs_repo = "ghcr.io/rustfs/rustfs";
    let rig = update_through(v1(), &[second, third]);
    assert!(!rig.world.borrow().local.contains(&digest("rustfs-1")));
}

#[test]
fn a_power_cut_after_the_commit_keeps_the_third_party_images_the_new_files_name() {
    let mut next = v2();
    next.postgres = "postgres-2";
    next.rustfs = "rustfs-2";
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    rig.world.borrow_mut().crash_on = Some("committed");
    let id = rig.request("req-cutcommit-1", &rig.manifest("v0.2.0"));
    let cut = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.run_request())));
    assert!(cut.is_err(), "the power went once the switch was committed");
    {
        let mut w = rig.world.borrow_mut();
        w.reboot();
        // podman removes the containers whose layers the cut left unreadable.
        w.containers.clear();
    }
    let pulls = rig.world.borrow().pulls;

    rig.machine(|m| m.boot()).unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::Succeeded);
    {
        let w = rig.world.borrow();
        assert_eq!(w.pulls, pulls, "the stack started without pulling again");
        for image in images_of(&next) {
            assert!(w.local.contains(&image), "{image} was removed");
        }
        let ancient = Spec::good("v0.0.9", "m0").own(Image::Api);
        assert!(!w.local.contains(&ancient), "the clean-up did not run");
    }
    rig.run().unwrap();
    rig.assert_settled(&id);
}

#[test]
fn an_unreadable_journal_starts_nothing_and_fails_the_boot() {
    let rig = Rig::new(v1(), &[]);
    std::fs::write(
        rig.layout.journal(),
        "laysi-updater-journal 1\nphase dancing\n",
    )
    .unwrap();
    rig.world.borrow_mut().reboot();
    // The boot unit's OnFailure= then runs the previous updater.
    assert!(rig.machine(|m| m.boot()).is_err());
    let w = rig.world.borrow();
    for service in SERVICES {
        assert!(!w.containers[service].running, "{service} started");
    }
    drop(w);
    // An updater that reads it, here the same one once the file is whole,
    // finishes and starts.
    std::fs::remove_file(rig.layout.journal()).unwrap();
    assert!(rig.machine(|m| m.boot()).is_ok());
    assert!(rig.world.borrow().containers["web"].running);
}

/// Run `scenario` once to count its calls, then again with the power cut at
/// each of them, reboot, boot and let the path unit see the request again.
/// Wherever it was cut, the machine ends on one release with everything
/// running, the database matching it and the request answered.
#[derive(Clone, Copy, PartialEq)]
enum Cut {
    /// The power goes: every container stops with the updater.
    Power,
    /// The power goes again during the recovery at boot.
    PowerTwice,
    /// Only the updater dies; what it started keeps running, and the
    /// recovery unit (`OnFailure=`) runs next.
    Process,
}

/// Wherever a run stopped: while the result is not final and the journal is
/// past step 4, api and mcp see the marker, and every stop so far happened
/// with it in place.
fn assert_marked_while_switching(rig: &Rig, id: &RequestId, k: usize) {
    let journal = rig.machine(|m| m.load_journal()).unwrap();
    let result = rig.machine(|m| m.read_result(id));
    let unfinished = result.as_ref().is_none_or(|r| !r.status.is_final());
    let past_step_4 = journal.is_some_and(|j| j.phase >= Phase::Stopping);
    if unfinished && past_step_4 {
        assert!(
            rig.switching(),
            "cut at call {k}: no marker while switching"
        );
    }
    if result.is_some_and(|r| {
        matches!(
            r.status,
            UpdateStatus::Switching | UpdateStatus::RollingBack
        )
    }) {
        assert!(rig.switching(), "cut at call {k}: no marker beside {id}");
    }
    let w = rig.world.borrow();
    assert!(
        w.marked.iter().all(|(_, present)| *present),
        "cut at call {k}: {:?}",
        w.marked
    );
}

fn sweep(installed: Spec, offered: Spec, expect_final: &[UpdateStatus], cut: Cut) {
    sweep_with(installed, offered, expect_final, cut, |_| {});
}

/// [`sweep`], with `setup` applied to every fresh world first.
fn sweep_with(
    installed: Spec,
    offered: Spec,
    expect_final: &[UpdateStatus],
    cut: Cut,
    setup: impl Fn(&mut World),
) {
    let double = cut == Cut::PowerTwice;
    let calls = {
        let rig = Rig::new(installed.clone(), std::slice::from_ref(&offered));
        setup(&mut rig.world.borrow_mut());
        rig.request("req-sweep-0000", &rig.manifest(offered.version));
        let start = rig.world.borrow().calls;
        rig.machine(|m| m.run_request()).unwrap();
        let end = rig.world.borrow().calls;
        end - start
    };
    assert!(calls > 20, "only {calls} calls");
    let step = if double { 3 } else { 1 };
    for k in (1..=calls).step_by(step) {
        let rig = Rig::new(installed.clone(), std::slice::from_ref(&offered));
        setup(&mut rig.world.borrow_mut());
        let id = rig.request("req-sweep-0001", &rig.manifest(offered.version));
        let at = rig.world.borrow().calls + k;
        rig.world.borrow_mut().crash_at = Some(at);
        let first = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.run_request())));
        assert!(first.is_err(), "the cut at call {k} of {calls} happened");
        assert_marked_while_switching(&rig, &id, k);
        if cut == Cut::Process {
            rig.world.borrow_mut().restart_updater();
        } else {
            rig.world.borrow_mut().reboot();
        }

        if double {
            // A second cut, during the recovery at boot.
            let j = (k % 11) + 1;
            let at = rig.world.borrow().calls + j;
            rig.world.borrow_mut().crash_at = Some(at);
            let _ = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.boot())));
            rig.world.borrow_mut().reboot();
        }

        let booted = rig.machine(|m| m.boot());
        assert!(booted.is_ok(), "boot after a cut at call {k}: {booted:?}");
        // The request is still in its directory: no ExecStopPost ran.
        rig.run()
            .unwrap_or_else(|e| panic!("run after a cut at call {k}: {e}"));

        let result = rig.result(&id);
        assert!(
            expect_final.contains(&result.status),
            "cut at call {k}: {:?} {:?}",
            result.status,
            result.reason
        );
        rig.assert_settled(&id);
        assert!(rig.retries_begun(id.as_str()) <= 1, "cut at call {k}");
    }
}

#[test]
fn a_power_cut_at_any_point_of_a_successful_update_ends_on_one_release() {
    sweep(
        v1(),
        v2(),
        &[
            UpdateStatus::Succeeded,
            UpdateStatus::RolledBack,
            UpdateStatus::FailedUnchanged,
        ],
        Cut::Power,
    );
}

#[test]
fn a_power_cut_at_any_point_of_a_rollback_ends_on_the_old_release() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    sweep(
        v1(),
        broken,
        &[UpdateStatus::RolledBack, UpdateStatus::FailedUnchanged],
        Cut::Power,
    );
}

#[test]
fn a_second_power_cut_during_recovery_still_ends_on_one_release() {
    sweep(
        v1(),
        v2(),
        &[
            UpdateStatus::Succeeded,
            UpdateStatus::RolledBack,
            UpdateStatus::FailedUnchanged,
        ],
        Cut::PowerTwice,
    );
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    sweep(
        v1(),
        broken,
        &[UpdateStatus::RolledBack, UpdateStatus::FailedUnchanged],
        Cut::PowerTwice,
    );
}

#[test]
fn an_updater_killed_at_any_point_with_the_stack_running_ends_on_one_release() {
    sweep(
        v1(),
        v2(),
        &[
            UpdateStatus::Succeeded,
            UpdateStatus::RolledBack,
            UpdateStatus::FailedUnchanged,
        ],
        Cut::Process,
    );
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    sweep(
        v1(),
        broken,
        &[UpdateStatus::RolledBack, UpdateStatus::FailedUnchanged],
        Cut::Process,
    );
}

#[test]
fn every_journal_phase_is_reached_by_the_tests() {
    // The sweeps above cut the power after each phase is written, because a
    // checkpoint follows every journal write. This lists the phases a
    // success and a rollback pass through, so a phase nobody reaches shows up.
    let mut seen = std::collections::BTreeSet::new();
    for offered in [v2(), {
        let mut b = v2();
        b.api = Behaviour::NeverHealthy;
        b
    }] {
        let rig = Rig::new(v1(), std::slice::from_ref(&offered));
        rig.request("req-phases-001", &rig.manifest(offered.version));
        rig.run().unwrap();
        let log = std::fs::read_to_string(rig.layout.log()).unwrap();
        for phase in Phase::all() {
            if log.contains(&format!(": {phase}\n")) {
                seen.insert(phase.name());
            }
        }
    }
    let missing: Vec<_> = Phase::all()
        .iter()
        .filter(|p| !seen.contains(p.name()) && **p != Phase::RollbackFailed)
        .collect();
    assert!(missing.is_empty(), "never reached: {missing:?}");
}

#[test]
fn manifests_verify_against_the_release_module() {
    let rig = Rig::new(v1(), &[v2()]);
    let text = rig.manifest("v0.2.0");
    let m = release::verify_with(&text, &[rig.key.verifying_key()]).unwrap();
    assert_eq!(Release::of(&m), v2().release());
}

#[test]
fn a_release_whose_compose_file_names_a_tag_is_refused_before_anything_stops() {
    let mut next = v2();
    next.rustfs = "latest";
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    // Publish a compose file with the tag in place of the digest.
    let tagged = next
        .compose()
        .replace(&format!("@{}", digest("latest")), ":latest");
    rig.world
        .borrow_mut()
        .registry
        .get_mut(&next.own(Image::Api))
        .unwrap()
        .files
        .insert(
            format!("{IMAGE_SHARE}/{}", COMPOSE_FILES[0]),
            tagged.into_bytes(),
        );
    let before = rig.world.borrow().containers.clone();
    let id = rig.request("req-tagged-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert!(result.reason.unwrap().contains("digest"));
    assert!(result.downtime_from.is_none());
    let w = rig.world.borrow();
    // What the preparation pulled went again.
    for image in Image::ALL {
        assert!(
            !w.local.contains(&next.own(image)),
            "{image} was left behind"
        );
        assert!(
            w.local.contains(&v1().own(image)),
            "{image} of v1 was removed"
        );
    }
    for (service, c) in &w.containers {
        assert_eq!(c.created, before[service].created, "{service}");
        assert_eq!(c.running, before[service].running, "{service}");
    }
}

#[test]
fn a_container_not_created_from_the_compose_file_does_not_pass() {
    use crate::host::{Container, Service};
    use crate::machine::judge;
    let at = chrono::Utc::now();
    let container = |image: &str, digest: &str| Container {
        id: "x".into(),
        name: "laysi-one-rustfs".into(),
        service: "rustfs".into(),
        running: true,
        exit_code: None,
        restarts: 0,
        has_healthcheck: true,
        health: Some("healthy".into()),
        image_name: image.into(),
        image_digest: digest.into(),
        created: at,
    };
    let (old, new) = (digest("rustfs-1"), digest("rustfs-2"));
    let service = Service {
        name: "rustfs".into(),
        image: format!("docker.io/rustfs/rustfs:1.0.1@{new}"),
    };
    let services = std::slice::from_ref(&service);
    let ok = container(&format!("docker.io/rustfs/rustfs:1.0.1@{new}"), &new);
    assert_eq!(judge(&[ok], services, None, REGISTRY, None), Ok(()));
    // The container a failed recreate left in place.
    let stale = container(&format!("docker.io/rustfs/rustfs@{old}"), &old);
    assert!(judge(&[stale], services, None, REGISTRY, None).is_err());
    // Created from the right reference, running something else.
    let other = container(&format!("docker.io/rustfs/rustfs@{new}"), &old);
    assert!(judge(&[other], services, None, REGISTRY, None).is_err());
    // A tag in the compose file never passes.
    let tagged = Service {
        name: "rustfs".into(),
        image: "docker.io/rustfs/rustfs:1.0.1".into(),
    };
    let running = container("docker.io/rustfs/rustfs:1.0.1", &new);
    assert!(judge(&[running], &[tagged], None, REGISTRY, None).is_err());
    // Older than the switch.
    let early = container(&format!("docker.io/rustfs/rustfs@{new}"), &new);
    let since = Some(at + chrono::Duration::seconds(30));
    assert!(judge(&[early], services, None, REGISTRY, since).is_err());
}

#[test]
fn a_service_that_does_not_stop_is_never_dumped_around() {
    let rig = Rig::new(v1(), &[v2()]);
    rig.world.borrow_mut().unstoppable = Some(v1().own(Image::Backup));
    let id = rig.request("req-nostop-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    // Step 4 could not stop backup: no dump, nothing switched, back up.
    assert_eq!(result.status, UpdateStatus::RolledBack, "{result:?}");
    assert!(result.reason.unwrap().contains("backup"));
    assert_eq!(rig.world.borrow().dumps, 0);
    rig.world.borrow_mut().unstoppable = None;
    rig.assert_settled(&id);
}

#[test]
fn a_rollback_that_cannot_stop_the_new_release_keeps_the_dump() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), &[broken.clone()]);
    rig.world.borrow_mut().unstoppable = Some(broken.own(Image::Api));
    let id = rig.request("req-nostop-002", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RollbackFailed, "{result:?}");
    assert!(result.rollback_reason.unwrap().contains("api"));
    assert_eq!(rig.world.borrow().restores, 0, "no restore under a writer");
    assert_eq!(rig.backups(), vec!["req-nostop-002.dump".to_string()]);
    // The files name the old release, so that is what a boot starts.
    let env = std::fs::read_to_string(&rig.layout.stack_files()[2]).unwrap();
    assert!(env.contains(&v1().own(Image::Api)));
    assert_eq!(rig.installed(), "v0.1.0");
}

#[test]
fn the_same_updater_whose_units_fail_is_kept_and_the_update_finishes() {
    let mut next = v2();
    next.updater = b"no-units same";
    let rig = Rig::new(v1(), &[next]);
    std::fs::write(rig.layout.current_updater(), b"no-units same").unwrap();
    let id = rig.request("req-sameupd-01", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(
        result.status,
        UpdateStatus::SucceededUpdaterKept,
        "{result:?}"
    );
    assert_eq!(
        std::fs::read(rig.layout.current_updater()).unwrap(),
        b"no-units same"
    );
    assert!(!rig.layout.previous_updater().exists());
    rig.assert_settled(&id);
}

#[test]
fn a_tagged_reference_to_an_own_image_is_still_checked_against_the_release() {
    use crate::host::{Container, Service};
    use crate::machine::judge;
    let release = v1().release();
    let other = v2().own(Image::Api);
    let tagged = format!("{REGISTRY}/laysi-one-api:v0.1.0@{other}");
    let service = Service {
        name: "api".into(),
        image: tagged.clone(),
    };
    let container = Container {
        id: "x".into(),
        name: "laysi-one-api".into(),
        service: "api".into(),
        running: true,
        exit_code: None,
        restarts: 0,
        has_healthcheck: false,
        health: None,
        image_name: tagged,
        image_digest: other,
        created: chrono::Utc::now(),
    };
    let verdict = judge(&[container], &[service], Some(&release), REGISTRY, None);
    assert!(verdict.unwrap_err().contains("v0.1.0"));
}

#[test]
fn a_release_whose_compose_file_names_an_own_image_off_the_manifest_is_refused_before_anything_stops(
) {
    let next = v2();
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    // An image in Laysi's web repository that the registry hands out and
    // the signed manifest does not name.
    let unsigned = digest("web-unsigned");
    {
        let mut w = rig.world.borrow_mut();
        let image = w.registry[&next.own(Image::Web)].clone();
        w.registry.insert(unsigned.clone(), image);
        let compose = next.compose().replace(
            "service web ${LAYSI_REGISTRY}/laysi-one-web@${LAYSI_WEB_DIGEST}",
            &format!("service web ${{LAYSI_REGISTRY}}/laysi-one-web@{unsigned}"),
        );
        assert_ne!(compose, next.compose());
        w.registry
            .get_mut(&next.own(Image::Api))
            .unwrap()
            .files
            .insert(
                format!("{IMAGE_SHARE}/{}", COMPOSE_FILES[0]),
                compose.into_bytes(),
            );
    }
    let before = rig.world.borrow().containers.clone();
    let id = rig.request("req-unsigned-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();

    let result = rig.result(&id);
    assert_eq!(result.request_id, id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert_eq!(
        result.reason.as_deref(),
        Some("新版 compose 檔的 web 不是版本清單上的 web 映像")
    );
    assert!(result.downtime_from.is_none());
    assert_eq!(rig.installed(), "v0.1.0");
    let w = rig.world.borrow();
    assert_eq!(w.dumps, 0);
    assert!(
        !w.local.contains(&unsigned),
        "the unsigned image was pulled"
    );
    assert!(w.marked.is_empty(), "stopped or started: {:?}", w.marked);
    for (service, c) in &w.containers {
        assert_eq!(c.created, before[service].created, "{service}");
        assert_eq!(c.running, before[service].running, "{service}");
    }
    drop(w);
    rig.assert_settled(&id);
}

#[test]
fn a_container_running_another_image_than_its_digest_names_is_rolled_back_before_web_and_mcp_start()
{
    let next = v2();
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    let substitute = digest("api-substitute");
    {
        let mut w = rig.world.borrow_mut();
        let image = w.registry[&next.own(Image::Api)].clone();
        w.registry.insert(substitute.clone(), image);
        w.runs_instead = Some((next.own(Image::Api), substitute));
    }
    let id = rig.request("req-substitute-1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();

    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RolledBack, "{result:?}");
    let reason = result.reason.unwrap();
    assert!(
        reason.contains("api") && reason.contains("不符"),
        "{reason}"
    );
    // web and mcp started once each: on the old release, after the restore.
    let w = rig.world.borrow();
    for entry in ["web", "mcp"] {
        let starts = w
            .marked
            .iter()
            .filter(|(what, _)| *what == format!("start {entry}"))
            .count();
        assert_eq!(starts, 1, "{entry}: {:?}", w.marked);
    }
    assert_eq!(w.restores, 1);
    drop(w);
    rig.assert_settled(&id);
}

#[test]
fn a_restore_that_fails_while_recovering_starts_postgres_alone() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), &[broken]);
    rig.world.borrow_mut().crash_on = Some("restoring");
    let id = rig.request("req-recoverfail", &rig.manifest("v0.2.0"));
    let cut = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.run_request())));
    assert!(cut.is_err(), "the power went in the restoring phase");
    rig.world.borrow_mut().reboot();
    rig.world.borrow_mut().fail_restore = true;

    // The boot finishes the rollback, the restore fails, and what started is
    // postgres alone.
    assert!(rig.machine(|m| m.boot()).is_ok());
    assert_eq!(rig.result(&id).status, UpdateStatus::RollbackFailed);
    assert_eq!(rig.backups(), vec!["req-recoverfail.dump".to_string()]);
    assert!(rig.running("postgres"));
    for service in ["api", "mcp", "web", "backup", "rustfs"] {
        assert!(!rig.running(service), "{service} started");
    }
    assert!(!rig.switching());
}

#[test]
fn a_release_that_leaves_a_service_out_is_refused_before_anything_stops() {
    let next = v2();
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    let without_api: String = next
        .compose()
        .lines()
        .filter(|l| !l.starts_with("service api "))
        .map(|l| format!("{l}\n"))
        .collect();
    rig.world
        .borrow_mut()
        .registry
        .get_mut(&next.own(Image::Api))
        .unwrap()
        .files
        .insert(
            format!("{IMAGE_SHARE}/{}", COMPOSE_FILES[0]),
            without_api.into_bytes(),
        );
    let id = rig.request("req-noapi-0001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert!(result.reason.unwrap().contains("api"));
    assert!(result.downtime_from.is_none());
    rig.assert_settled(&id);
}

#[test]
fn a_request_whose_journal_cannot_be_written_is_answered_as_failed() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new(v1(), &[v2()]);
    let state = rig.layout.state_dir();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap();
    let id = rig.request("req-nojournal1", &rig.manifest("v0.2.0"));
    let outcome = rig.run();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(outcome.is_err());
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    rig.assert_settled(&id);

    // Killed between accepting a request and writing its journal: the
    // ExecStopPost that follows answers it.
    let id = RequestId::parse("req-killed-0001").unwrap();
    let text = UpdateRequest::new(id.clone(), &rig.manifest("v0.2.0"))
        .unwrap()
        .encode();
    std::fs::write(rig.layout.requests_dir().join(REQUEST_FILE), text).unwrap();
    let accepted = UpdateResult::new(id.clone(), UpdateStatus::Accepted, chrono::Utc::now());
    std::fs::write(
        rig.layout.results_dir().join(id.as_str()),
        accepted.encode(),
    )
    .unwrap();
    rig.machine(|m| m.discard_requests(true)).unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::FailedUnchanged);
    assert_eq!(rig.requests_left(), 0);
}

#[test]
fn a_rollback_that_cannot_put_the_files_back_starts_nothing() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), &[broken]);
    rig.world.borrow_mut().crash_on = Some("rolling-back");
    let id = rig.request("req-nofiles-001", &rig.manifest("v0.2.0"));
    let cut = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.run_request())));
    assert!(cut.is_err());
    rig.world.borrow_mut().reboot();
    for file in rig.layout.stack_files() {
        std::fs::remove_file(Layout::previous_of(&file)).unwrap();
    }
    assert!(rig.machine(|m| m.boot()).is_ok());
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RollbackFailed, "{result:?}");
    assert_eq!(
        result.running, None,
        "the files name no release the host had"
    );
    assert_eq!(rig.backups(), vec!["req-nofiles-001.dump".to_string()]);
    let w = rig.world.borrow();
    for (service, c) in &w.containers {
        assert!(!c.running, "{service} started");
    }
}

#[test]
fn a_release_that_runs_an_image_in_the_wrong_service_is_refused() {
    let next = v2();
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    let swapped = next.compose().replace(
        "service web ${LAYSI_REGISTRY}/laysi-one-web@${LAYSI_WEB_DIGEST}",
        "service web ${LAYSI_REGISTRY}/laysi-one-backup@${LAYSI_BACKUP_DIGEST}",
    );
    assert_ne!(swapped, next.compose());
    rig.world
        .borrow_mut()
        .registry
        .get_mut(&next.own(Image::Api))
        .unwrap()
        .files
        .insert(
            format!("{IMAGE_SHARE}/{}", COMPOSE_FILES[0]),
            swapped.into_bytes(),
        );
    let id = rig.request("req-swapped-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert!(result.reason.unwrap().contains("web"));
    rig.assert_settled(&id);
}

#[test]
fn a_release_without_rustfs_is_refused() {
    let next = v2();
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    let without: String = next
        .compose()
        .lines()
        .filter(|l| !l.starts_with("service rustfs "))
        .map(|l| format!("{l}\n"))
        .collect();
    rig.world
        .borrow_mut()
        .registry
        .get_mut(&next.own(Image::Api))
        .unwrap()
        .files
        .insert(
            format!("{IMAGE_SHARE}/{}", COMPOSE_FILES[0]),
            without.into_bytes(),
        );
    let id = rig.request("req-norustfs01", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert!(result.reason.unwrap().contains("rustfs"));
    rig.assert_settled(&id);
}

#[test]
fn a_kept_dump_starts_no_database_under_a_service_that_will_not_stop() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), std::slice::from_ref(&broken));
    rig.world.borrow_mut().fail_restore = true;
    let id = rig.request("req-stuckdump1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RollbackFailed);
    // A rustfs container started by hand on the old image ignores podman
    // stop; postgres is stopped.
    {
        let mut w = rig.world.borrow_mut();
        let rustfs = digest(v1().rustfs);
        let clock = w.clock;
        w.containers.insert(
            "rustfs".into(),
            FakeContainer {
                reference: format!("docker.io/rustfs/rustfs@{rustfs}"),
                digest: rustfs.clone(),
                running: true,
                exit_code: None,
                created: clock,
            },
        );
        w.containers.get_mut("postgres").unwrap().running = false;
        w.unstoppable = Some(rustfs);
    }
    assert!(rig.machine(|m| m.boot()).is_ok());
    assert!(!rig.world.borrow().containers["postgres"].running);
}

#[test]
fn a_rollback_failure_cut_before_its_result_is_answered_at_boot() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    let rig = Rig::new(v1(), std::slice::from_ref(&broken));
    // The old web no longer starts, so the rollback fails; the power goes
    // right after the journal says so and before the result does.
    let old_web = v1().own(Image::Web);
    rig.world
        .borrow_mut()
        .registry
        .get_mut(&old_web)
        .unwrap()
        .behaviour = Behaviour::ExitsAtStart;
    rig.world.borrow_mut().crash_on = Some("rollback-failed");
    let id = rig.request("req-cutfinal-01", &rig.manifest("v0.2.0"));
    let cut = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.run_request())));
    assert!(cut.is_err());
    assert!(!rig.result(&id).status.is_final());
    rig.world.borrow_mut().reboot();
    let _ = rig.machine(|m| m.boot());
    assert_eq!(rig.result(&id).status, UpdateStatus::RollbackFailed);
    // The request still in its directory is not taken for a new one.
    let restores = rig.world.borrow().restores;
    let _ = rig.run();
    assert_eq!(rig.world.borrow().restores, restores);
    assert_eq!(rig.result(&id).status, UpdateStatus::RollbackFailed);
}

#[test]
fn a_final_result_that_cannot_be_written_keeps_the_journal_until_it_is() {
    let rig = Rig::new(v1(), &[v2()]);
    let id = rig.request("req-noresult-01", &rig.manifest("v0.2.0"));
    // A directory in the result's place: every write of this result fails,
    // and the marker beside it can still be written.
    let blocker = rig.layout.results_dir().join(id.as_str());
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "").unwrap();
    let journal = rig.layout.journal();
    let outcome = rig.machine(|m| m.run_request());
    std::fs::remove_dir_all(&blocker).unwrap();
    assert!(outcome.is_err(), "the final result could not be written");
    assert!(journal.exists(), "the journal waits for the result");
    assert!(rig.switching(), "and so does the marker");
    // ExecStopPost does not take it for a request that never started.
    rig.machine(|m| m.discard_requests(true)).unwrap();
    assert!(rig.machine(|m| m.read_result(&id)).is_none());
    // The recovery unit finishes the journal and writes the result.
    rig.machine(|m| m.boot()).unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::Succeeded);
    assert!(!journal.exists());
    assert!(!rig.switching());
}

#[test]
fn an_unreadable_env_file_stops_the_update_before_anything_changes() {
    let rig = Rig::new(v1(), &[v2()]);
    let env = rig.layout.stack_files()[2].clone();
    std::fs::write(&env, [0xff, 0xfe, b'\n']).unwrap();
    let id = rig.request("req-badenv-0001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert!(result.downtime_from.is_none());
    assert_eq!(std::fs::read(&env).unwrap(), [0xff, 0xfe, b'\n']);
    assert_eq!(rig.installed(), "v0.1.0");
}

#[test]
fn a_release_larger_than_the_disk_is_refused_after_pulling_before_anything_stops() {
    let next = v2();
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    let before = rig.world.borrow().containers.clone();
    {
        let mut w = rig.world.borrow_mut();
        // Enough for twice the running images, not for what the release
        // turns out to weigh.
        w.free = 4 * 1024 * 1024 * 1024;
        for image in Image::ALL {
            w.registry.get_mut(&next.own(image)).unwrap().size = 1500 * 1024 * 1024;
        }
    }
    let id = rig.request("req-toolarge-01", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RefusedDiskSpace, "{result:?}");
    assert!(result.downtime_from.is_none());
    let w = rig.world.borrow();
    assert_eq!(w.dumps, 0);
    for image in Image::ALL {
        assert!(!w.local.contains(&next.own(image)), "{image} was kept");
    }
    for (service, c) in &w.containers {
        assert_eq!(c.created, before[service].created, "{service}");
        assert_eq!(c.running, before[service].running, "{service}");
    }
}

#[test]
fn a_rollback_removes_the_images_of_the_release_it_left() {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    broken.rustfs = "rustfs-2";
    let rig = Rig::new(v1(), std::slice::from_ref(&broken));
    let id = rig.request("req-rbprune-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RolledBack);
    let w = rig.world.borrow();
    for image in Image::ALL {
        assert!(!w.local.contains(&broken.own(image)), "{image} was kept");
        assert!(w.local.contains(&v1().own(image)), "{image} of v1 went");
    }
    assert!(
        !w.local.contains(&digest("rustfs-2")),
        "the new rustfs was kept"
    );
    assert!(w.local.contains(&digest("rustfs-1")));
}

#[test]
fn a_swap_cut_before_it_began_does_not_bring_back_an_older_updater() {
    let mut next = v2();
    next.updater = b"broken updater";
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    // The journal says the swap was begun; the power went before the
    // current updater was copied anywhere. `previous` is from long ago.
    std::fs::write(rig.layout.previous_updater(), b"updater ancient").unwrap();
    std::fs::write(rig.layout.incoming_updater(), b"broken updater").unwrap();
    let mut j = journal::Journal::new(
        RequestId::parse("req-swapcut-001").unwrap(),
        chrono::Utc::now(),
        v1().release(),
        next.release(),
    );
    j.phase = Phase::Committed;
    j.updater_swapped = true;
    std::fs::write(rig.layout.journal(), j.encode()).unwrap();
    let _ = rig.machine(|m| m.boot());
    assert_eq!(
        std::fs::read(rig.layout.current_updater()).unwrap(),
        b"updater installed"
    );
    let id = RequestId::parse("req-swapcut-001").unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::SucceededUpdaterKept);
}

fn never_healthy() -> Spec {
    let mut broken = v2();
    broken.api = Behaviour::NeverHealthy;
    broken
}

#[test]
fn a_failed_restore_is_retried_once_after_making_room() {
    let broken = never_healthy();
    let rig = Rig::new(v1(), std::slice::from_ref(&broken));
    rig.world.borrow_mut().fail_next_restores = 1;
    let id = rig.request("req-retryok-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RolledBack, "{result:?}");
    let note = result.rollback_reason.clone().unwrap();
    assert!(
        note.contains("第一次還原資料庫失敗") && note.contains("No space left"),
        "{note}"
    );
    assert!(note.contains("重試後成功"), "{note}");
    assert!(result.reason.unwrap().contains("api"));
    let w = rig.world.borrow();
    assert_eq!(w.restores, 2);
    // The retry found the new release's images gone, and the old ones there.
    let before_retry = &w.local_at_restore[1];
    for image in Image::ALL {
        assert!(
            !before_retry.contains(&broken.own(image)),
            "{image} of v0.2.0 kept"
        );
        assert!(
            before_retry.contains(&v1().own(image)),
            "{image} of v0.1.0 removed"
        );
    }
    assert!(w.local_at_restore[0].contains(&broken.own(Image::Api)));
    drop(w);
    assert_eq!(rig.retries_begun(id.as_str()), 1);
    assert!(rig.all_marked(), "{:?}", rig.world.borrow().marked);
    rig.assert_settled(&id);
}

#[test]
fn a_retry_that_fails_too_ends_the_rollback_and_keeps_the_dump_as_it_was() {
    let broken = never_healthy();
    let rig = Rig::new(v1(), std::slice::from_ref(&broken));
    rig.world.borrow_mut().fail_restore = true;
    let id = rig.request("req-retryfail1", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::RollbackFailed, "{result:?}");
    let why = result.rollback_reason.unwrap();
    assert!(
        why.contains("重試後") && why.contains("第一次") && why.contains("備份保留"),
        "{why}"
    );
    assert_eq!(rig.world.borrow().restores, 2);
    assert_eq!(rig.retries_begun(id.as_str()), 1);
    // The dump is the database as it was at step 5.
    let dump =
        std::fs::read_to_string(rig.layout.backup_dir().join("req-retryfail1.dump")).unwrap();
    assert_eq!(dump, "fake-dump\nm1\nwritten-before-the-update\n");
    // The new release's images went to make room; the old release's stay
    // for whoever restores by hand.
    for image in Image::ALL {
        assert!(!rig.world.borrow().local.contains(&broken.own(image)));
        assert!(rig.world.borrow().local.contains(&v1().own(image)));
    }
    assert!(!rig.switching(), "the result is final");

    // No boot tries again.
    for _ in 0..3 {
        rig.world.borrow_mut().reboot();
        assert!(rig.machine(|m| m.boot()).is_ok());
        assert_eq!(rig.world.borrow().restores, 2);
        assert!(rig.running("postgres"));
        assert!(!rig.running("api"));
        assert!(!rig.switching());
    }
    assert_eq!(rig.retries_begun(id.as_str()), 1);
}

#[test]
fn a_power_cut_at_any_point_of_a_retried_restore_ends_on_the_old_release() {
    for cut in [Cut::Power, Cut::Process] {
        sweep_with(
            v1(),
            never_healthy(),
            &[UpdateStatus::RolledBack, UpdateStatus::FailedUnchanged],
            cut,
            |w| w.fail_next_restores = 1,
        );
    }
}

/// Every restore fails: wherever the power goes, the rollback ends failed
/// after one retry at most, and the dump stays as it was.
#[test]
fn a_power_cut_during_a_failing_retry_never_begins_a_second_one() {
    let calls = {
        let rig = Rig::new(v1(), &[never_healthy()]);
        rig.world.borrow_mut().fail_restore = true;
        rig.request("req-retrycut-00", &rig.manifest("v0.2.0"));
        let start = rig.world.borrow().calls;
        rig.machine(|m| m.run_request()).unwrap();
        let end = rig.world.borrow().calls;
        end - start
    };
    let mut retried = 0;
    for k in 1..=calls {
        let rig = Rig::new(v1(), &[never_healthy()]);
        rig.world.borrow_mut().fail_restore = true;
        let id = rig.request("req-retrycut-01", &rig.manifest("v0.2.0"));
        let at = rig.world.borrow().calls + k;
        rig.world.borrow_mut().crash_at = Some(at);
        let cut = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.run_request())));
        assert!(cut.is_err(), "the cut at call {k} of {calls} happened");
        assert_marked_while_switching(&rig, &id, k);
        let begun_before_boot = rig.retries_begun(id.as_str());
        rig.world.borrow_mut().reboot();
        let _ = rig.machine(|m| m.boot());
        let _ = rig.run();

        let result = rig.result(&id);
        let begun = rig.retries_begun(id.as_str());
        assert!(begun <= 1, "cut at call {k}: {begun} retries");
        assert!(!rig.switching(), "cut at call {k}: marker left");
        match result.status {
            UpdateStatus::RollbackFailed => {
                assert_eq!(begun, 1, "cut at call {k}");
                let dump = rig.layout.backup_dir().join("req-retrycut-01.dump");
                assert_eq!(
                    std::fs::read_to_string(dump).unwrap(),
                    "fake-dump\nm1\nwritten-before-the-update\n",
                    "cut at call {k}"
                );
                // A cut during the retry resumes it: one more restore at most.
                assert!(rig.world.borrow().restores <= 3, "cut at call {k}");
                if begun_before_boot == 1 {
                    retried += 1;
                }
            }
            // Cut before anything stopped, or before the switch.
            UpdateStatus::FailedUnchanged | UpdateStatus::RolledBack => {
                assert_eq!(begun, 0, "cut at call {k}");
                rig.assert_settled(&id);
            }
            other => panic!("cut at call {k}: {other:?}"),
        }
        let restores = rig.world.borrow().restores;
        rig.world.borrow_mut().reboot();
        let _ = rig.machine(|m| m.boot());
        assert_eq!(rig.world.borrow().restores, restores, "cut at call {k}");
    }
    assert!(retried > 3, "only {retried} cuts fell inside the retry");
}

#[test]
fn an_image_the_registry_will_not_hand_out_is_reported_as_such() {
    let next = v2();
    let rig = Rig::new(v1(), std::slice::from_ref(&next));
    rig.world.borrow_mut().private_repo = Some(format!("{REGISTRY}/laysi-one-api"));
    let id = rig.request("req-private-001", &rig.manifest("v0.2.0"));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert_eq!(
        result.reason.as_deref(),
        Some("無法取得 api 映像：映像庫拒絕提供或沒有這個映像")
    );
    assert!(result.downtime_from.is_none());
    rig.assert_settled(&id);
    assert!(rig.world.borrow().marked.is_empty(), "nothing stopped");

    // A release whose images were never published.
    rig.world.borrow_mut().private_repo = None;
    let ghost = Spec::good("v0.3.0", "m3");
    let id = rig.request("req-missing-001", &signed_manifest(&rig.key, &ghost));
    rig.run().unwrap();
    let result = rig.result(&id);
    assert_eq!(result.status, UpdateStatus::FailedUnchanged, "{result:?}");
    assert!(result.reason.unwrap().starts_with("無法取得 api 映像"));
    rig.assert_settled(&id);
}

#[test]
fn a_pull_that_fails_otherwise_keeps_podmans_words() {
    use crate::machine::pull_failure;
    for e in [
        "podman exited with exit status: 125: Error: dial tcp: lookup ghcr.io: no such host",
        "Error: copying system image from manifest list: open /home/stack/.local/share/containers/storage/overlay/l: permission denied",
    ] {
        assert_eq!(
            pull_failure(" api 映像", e),
            format!("無法下載 api 映像：{e}")
        );
    }
    for refused in [
        "reading manifest sha256:11 in ghcr.io/laysi/laysi-one-api: denied",
        "Error: unauthorized: authentication required",
        "manifest unknown",
        "name unknown: repository name not known to registry",
    ] {
        assert!(
            pull_failure("新版的映像", refused).starts_with("無法取得新版的映像："),
            "{refused}"
        );
    }
}

#[test]
fn a_stale_marker_is_removed_by_the_next_run_that_finds_no_update() {
    let rig = Rig::new(v1(), &[v2()]);
    let marker = rig.layout.results_dir().join(SWITCHING_FILE);
    // Left by a run that died between dropping its journal and the marker.
    std::fs::write(&marker, "request req-gone-000001\n").unwrap();
    assert!(rig.switching());
    rig.world.borrow_mut().reboot();
    rig.machine(|m| m.boot()).unwrap();
    assert!(!rig.switching());

    std::fs::write(&marker, "request req-gone-000001\n").unwrap();
    let id = rig.request("req-same-00009", &rig.manifest("v0.1.0"));
    rig.run().unwrap();
    assert_eq!(rig.result(&id).status, UpdateStatus::RefusedSameVersion);
    assert!(!rig.switching());
}

#[test]
fn a_journal_past_step_4_without_a_marker_gets_one_until_it_is_finished() {
    // An update the marker did not exist for yet, cut while restoring.
    let rig = Rig::new(v1(), &[never_healthy()]);
    rig.world.borrow_mut().crash_on = Some("restoring");
    let id = rig.request("req-nomarker-01", &rig.manifest("v0.2.0"));
    let cut = catch_unwind(AssertUnwindSafe(|| rig.machine(|m| m.run_request())));
    assert!(cut.is_err());
    std::fs::remove_file(rig.layout.results_dir().join(SWITCHING_FILE)).unwrap();
    rig.world.borrow_mut().reboot();
    rig.world.borrow_mut().marked.clear();
    rig.machine(|m| m.boot()).unwrap();
    // web and mcp came back on the restored database with the marker in
    // place; the boot then starts the stack once more after it is gone.
    let marked = rig.world.borrow().marked.clone();
    assert_eq!(
        marked[..2],
        [
            ("start mcp".to_string(), true),
            ("start web".to_string(), true)
        ]
    );
    assert!(
        marked[2..].iter().all(|(_, present)| !present),
        "{marked:?}"
    );
    assert_eq!(rig.result(&id).status, UpdateStatus::RolledBack);
    rig.run().unwrap();
    rig.assert_settled(&id);
}
