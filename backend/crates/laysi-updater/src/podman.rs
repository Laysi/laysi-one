//! The real host: podman, podman-compose, systemctl and systemd-run as the
//! stack user.
//!
//! Tool paths come from `state/updater.env`, which `install` writes with the
//! absolute paths it found, because a systemd user unit does not see the
//! PATH of an interactive shell.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use laysi_license_token::update::ToolVersions;

use crate::fsx;
use crate::host::{Container, Host, HostResult, LocalImage, Service, StackFiles};
use crate::layout::PROJECT;

const PROJECT_LABEL: &str = "io.podman.compose.project";
const SERVICE_LABEL: &str = "com.docker.compose.service";

pub struct PodmanHost {
    pub podman: String,
    pub compose: String,
    pub systemctl: String,
    pub systemd_run: String,
    pub root: PathBuf,
    /// For pulls, which wait on the network.
    pub pull_timeout: Duration,
}

impl PodmanHost {
    pub fn from_env(root: &Path) -> PodmanHost {
        let var = |key: &str, default: &str| {
            std::env::var(key)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        PodmanHost {
            podman: var("LAYSI_PODMAN", "podman"),
            compose: var("LAYSI_PODMAN_COMPOSE", "podman-compose"),
            systemctl: var("LAYSI_SYSTEMCTL", "systemctl"),
            // Installs from before it was in updater.env find it on PATH.
            systemd_run: var("LAYSI_SYSTEMD_RUN", "systemd-run"),
            root: root.to_path_buf(),
            pull_timeout: Duration::from_secs(60 * 60),
        }
    }

    fn podman(&self) -> Command {
        detached(Command::new(&self.podman))
    }

    /// `podman-compose --version`, which also runs podman.
    pub fn compose_version(&self) -> HostResult<String> {
        let mut cmd = detached(Command::new(&self.compose));
        cmd.arg("--podman-path").arg(&self.podman).arg("--version");
        let out = run(cmd, Some(Duration::from_secs(60)))?;
        parse_compose_version(&out)
            .map(str::to_string)
            .ok_or_else(|| format!("podman-compose --version printed {:?}", out.trim()))
    }

    pub fn podman_version(&self) -> HostResult<String> {
        let mut cmd = self.podman();
        cmd.args(["version", "--format", "{{.Client.Version}}"]);
        Ok(run(cmd, Some(Duration::from_secs(60)))?.trim().to_string())
    }

    fn compose(&self, files: &StackFiles) -> Command {
        let mut cmd = detached(Command::new(&self.compose));
        // The same podman the updater runs, whatever PATH the unit has.
        cmd.arg("--podman-path").arg(&self.podman);
        cmd.arg("-p").arg(PROJECT);
        cmd.arg("-f").arg(&files[0]).arg("-f").arg(&files[1]);
        cmd.arg("--env-file").arg(&files[2]);
        if let Some(dir) = files[0].parent() {
            cmd.current_dir(dir);
        }
        cmd
    }

    /// `podman-compose up` in a transient scope of its own.
    ///
    /// Rootless podman forwards each published port with a `rootlessport`
    /// process, and unlike conmon it stays in the cgroup of whoever started
    /// the container whatever `INVOCATION_ID` says. Started from the run or
    /// recovery unit, both oneshots without `RemainAfterExit=`, it is killed
    /// with the unit's cgroup the moment the run ends: every container stays
    /// up and healthy and nothing outside the machine reaches them. The scope
    /// outlives the run and goes away when its last process does.
    ///
    /// systemd-run sets `INVOCATION_ID` for what it runs in a scope, so it is
    /// removed again inside and conmon keeps the scope podman gives it. That
    /// scope has no default dependencies and neither has this one: at
    /// shutdown neither is stopped alongside the boot unit while its
    /// `ExecStop=` is still stopping the stack, postgres last.
    fn up_command(&self, files: &StackFiles, services: &[String], recreate: bool) -> Command {
        let mut compose = self.compose(files);
        compose.args(["up", "-d", "--no-deps"]);
        if recreate {
            compose.arg("--force-recreate");
        }
        compose.args(services);
        let mut cmd = detached(Command::new(&self.systemd_run));
        cmd.args(["--user", "--scope", "--collect", "--quiet"]);
        cmd.arg("--property=DefaultDependencies=no");
        cmd.args(["--", "env", "-u", "INVOCATION_ID"]);
        cmd.arg(compose.get_program()).args(compose.get_args());
        if let Some(dir) = compose.get_current_dir() {
            cmd.current_dir(dir);
        }
        cmd
    }

    /// One container at a time when inspecting them together fails: one
    /// removed between the two calls is skipped, and one an interrupted
    /// podman or a power cut left unreadable is removed, as `unreadable`
    /// decides. The removal goes to stderr, which the unit's journal keeps.
    fn inspect_containers(&mut self) -> HostResult<Vec<Container>> {
        let mut ps = self.podman();
        ps.args([
            "ps",
            "--all",
            "--no-trunc",
            "--format",
            "{{.ID}} {{.State}}",
        ])
        .arg("--filter")
        .arg(format!("label={PROJECT_LABEL}={PROJECT}"));
        let listed: Vec<(String, String)> = run(ps, None)?
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let id = fields.next()?.to_string();
                Some((id, fields.next().unwrap_or_default().to_string()))
            })
            .collect();
        if listed.is_empty() {
            return Ok(Vec::new());
        }
        let mut inspect = self.podman();
        inspect
            .args(["container", "inspect"])
            .args(listed.iter().map(|(id, _)| id));
        if let Ok(json) = run(inspect, None) {
            return parse_containers(&json);
        }
        let mut found = Vec::new();
        for (id, state) in &listed {
            let mut inspect = self.podman();
            inspect.args(["container", "inspect", id]);
            match run(inspect, None) {
                Ok(json) => found.extend(parse_containers(&json)?),
                Err(e) => match unreadable(state, &e) {
                    Unreadable::Gone => {}
                    Unreadable::Remove => {
                        let mut rm = self.podman();
                        rm.args(["rm", "--force", id]);
                        run(rm, Some(Duration::from_secs(120)))
                            .map_err(|rm| format!("{e}; removing it: {rm}"))?;
                        eprintln!(
                            "{} removed container {id} ({state}), which podman could not read: {e}",
                            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                        );
                    }
                    Unreadable::Fail => return Err(e),
                },
            }
        }
        Ok(found)
    }
}

/// podman leaves conmon in the calling unit's cgroup when `INVOCATION_ID` says
/// it runs under systemd, and systemd then kills it when the oneshot ends,
/// orphaning every container it started. Without the variable podman gives
/// each conmon a scope of its own, which outlives this run.
fn detached(mut cmd: Command) -> Command {
    cmd.env_remove("INVOCATION_ID");
    cmd
}

/// Run to completion, with stdin from nowhere unless given. `Ok(stdout)` on
/// a zero exit, otherwise the tail of stderr.
fn run(cmd: Command, timeout: Option<Duration>) -> HostResult<String> {
    let name = program_of(&cmd);
    run_as(cmd, &name, timeout)
}

/// `run`, with errors naming `name`: a wrapper that execs the program the
/// exit status belongs to would otherwise be named for it.
fn run_as(mut cmd: Command, name: &str, timeout: Option<Duration>) -> HostResult<String> {
    let shown = describe(&cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn(&mut cmd, &shown)?;
    let (out, err) = collect(&mut child);
    let status = wait(&mut child, timeout).map_err(|e| format!("{shown}: {e}"))?;
    let stdout = drained(out);
    let stderr = drained(err);
    if status.success() {
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    } else {
        Err(format!(
            "{name} exited with {status}: {}",
            tail(&String::from_utf8_lossy(&stderr))
        ))
    }
}

/// Run with stdin or stdout connected to a file, killed after `timeout`.
fn run_with_file(
    mut cmd: Command,
    file: &mut File,
    as_stdin: bool,
    timeout: Duration,
) -> HostResult<()> {
    let shown = describe(&cmd);
    let handle = file.try_clone().map_err(|e| e.to_string())?;
    if as_stdin {
        cmd.stdin(Stdio::from(handle)).stdout(Stdio::null());
    } else {
        cmd.stdin(Stdio::null()).stdout(Stdio::from(handle));
    }
    cmd.stderr(Stdio::piped());
    let mut child = spawn(&mut cmd, &shown)?;
    let (_, err) = collect(&mut child);
    let status = wait(&mut child, Some(timeout)).map_err(|e| format!("{shown}: {e}"))?;
    let stderr = drained(err);
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} exited with {status}: {}",
            program_of(&cmd),
            tail(&String::from_utf8_lossy(&stderr))
        ))
    }
}

/// In a process group of its own, so that a deadline ends podman-compose and
/// every podman it started, not only the process waited on.
fn spawn(cmd: &mut Command, shown: &str) -> HostResult<Child> {
    cmd.process_group(0)
        .spawn()
        .map_err(|e| format!("{shown}: {e}"))
}

/// How long the output of a finished command is waited for. Something it
/// left running can hold the pipe open; what was written by then is enough.
const DRAIN: Duration = Duration::from_secs(5);

type Reader = mpsc::Receiver<Vec<u8>>;

fn drained(reader: Reader) -> Vec<u8> {
    reader.recv_timeout(DRAIN).unwrap_or_default()
}

fn collect(child: &mut Child) -> (Reader, Reader) {
    let spawn = |pipe: Option<Box<dyn Read + Send>>| {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = send.send(buf);
        });
        receive
    };
    let out = spawn(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = spawn(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    (out, err)
}

fn wait(child: &mut Child, timeout: Option<Duration>) -> std::io::Result<std::process::ExitStatus> {
    let Some(timeout) = timeout else {
        return child.wait();
    };
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            // The child leads its own group: a negative pid signals all of it.
            if let Ok(group) = i32::try_from(child.id()) {
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other(format!(
                "did not finish within {} s",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// What to do with a project container `podman container inspect` fails on.
#[derive(Debug, PartialEq, Eq)]
enum Unreadable {
    /// Removed between `podman ps` and the inspect.
    Gone,
    /// It holds nothing, since the data lives in volumes, and recreating
    /// its service fails until it is gone.
    Remove,
    Fail,
}

/// A podman cut off halfway leaves a container nothing can read in two
/// ways. A create or a remove killed in the middle leaves its record in
/// podman's database without its storage ("container not known"), stuck in
/// "removing" with its name taken. A power cut leaves the storage of a
/// container created just before it half written: the `link` and `lower`
/// files of its layer exist and are empty, and every inspect fails on
/// "getting graph driver info", reading the empty link as the overlay
/// directory itself, which is no symlink. Either is removed, the second
/// only when the container does not run and is not being stopped: removing
/// one that does would stop a service. Any other error is returned, the
/// same wrapper around a permission or I/O error included, since it may
/// pass and says nothing about the container being broken.
fn unreadable(state: &str, error: &str) -> Unreadable {
    let empty_link = error.contains("getting graph driver info")
        && error.contains("readlink ")
        && error.contains("/overlay: invalid argument");
    if error.contains("no such container") {
        Unreadable::Gone
    } else if error.contains("container not known")
        || (empty_link && !matches!(state, "running" | "paused" | "stopping"))
    {
        Unreadable::Remove
    } else {
        Unreadable::Fail
    }
}

/// The command line, for errors. Not `{cmd:?}`, which starts with
/// `cd "<dir>" &&` when a working directory is set.
fn describe(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn program_of(cmd: &Command) -> String {
    file_name(cmd.get_program())
}

fn file_name(program: &OsStr) -> String {
    Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy()
        .into_owned()
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(3);
    lines[start..].join(" / ")
}

/// The version in `podman-compose --version`, which prints podman's own line
/// too: `podman version 4.9.3` and `podman-compose version 1.6.0`.
pub fn parse_compose_version(out: &str) -> Option<&str> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("podman-compose version"))
        .map(|v| v.trim_start_matches(':').trim())
        .find(|v| !v.is_empty())
}

/// The application database, named by the container's own environment.
const PSQL_SIZE: &str = r#"exec psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc 'select pg_database_size(current_database())'"#;
const PG_DUMP: &str = r#"exec pg_dump -Fc -U "$POSTGRES_USER" -d "$POSTGRES_DB""#;
/// The name goes in as a psql variable quoted with `:"db"`, never spliced into
/// the SQL; psql only interpolates variables in what it reads, not in `-c`.
const PG_DROP: &str = r#"printf '%s\n' 'DROP DATABASE IF EXISTS :"db" WITH (FORCE);' | exec psql -v ON_ERROR_STOP=1 -v db="$POSTGRES_DB" -U "$POSTGRES_USER" -d postgres"#;
const PG_RESTORE: &str =
    r#"exec pg_restore --create --exit-on-error -U "$POSTGRES_USER" -d postgres"#;

/// For a dump, its check or a restore. Far more than a small business's
/// database takes; what it guards against is a call that never returns
/// while every service is stopped.
const DATABASE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// The services and their images from `podman-compose config`, which prints
/// the merged, interpolated files as YAML with two-space indentation:
///
/// ```text
/// services:
///   api:
///     image: ghcr.io/laysi/laysi-one-api@sha256:…
/// ```
///
/// Only that shape is read: a service is a key two spaces in under
/// `services:`, its image the `image:` key four spaces in.
pub fn parse_config(yaml: &str) -> HostResult<Vec<Service>> {
    let unquote = |v: &str| {
        let v = v.trim();
        v.strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .or_else(|| v.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .unwrap_or(v)
            .to_string()
    };
    let mut services: Vec<Service> = Vec::new();
    let mut inside = false;
    for line in yaml.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            inside = line.trim_end() == "services:";
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(rest) = line.strip_prefix("  ").filter(|r| !r.starts_with(' ')) {
            let name = rest
                .trim_end()
                .strip_suffix(':')
                .ok_or_else(|| format!("podman-compose config: unexpected line {line:?}"))?;
            services.push(Service {
                name: unquote(name),
                image: String::new(),
            });
        } else if let Some(value) = line.strip_prefix("    image:") {
            let service = services.last_mut().ok_or_else(|| {
                format!("podman-compose config: image outside a service {line:?}")
            })?;
            service.image = unquote(value);
        }
    }
    if services.is_empty() {
        return Err("podman-compose config printed no services".into());
    }
    if let Some(s) = services.iter().find(|s| s.image.is_empty()) {
        return Err(format!("podman-compose config: {} has no image", s.name));
    }
    Ok(services)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Inspected {
    id: String,
    name: String,
    created: String,
    state: InspectedState,
    #[serde(default)]
    image_name: String,
    /// The ID of the image it runs.
    #[serde(default)]
    image: String,
    #[serde(default)]
    image_digest: String,
    #[serde(default)]
    restart_count: u32,
    config: InspectedConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectedState {
    #[serde(default)]
    running: bool,
    #[serde(default)]
    status: String,
    #[serde(default)]
    exit_code: i64,
    #[serde(default)]
    health: Option<InspectedHealth>,
    #[serde(default)]
    healthcheck: Option<InspectedHealth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectedHealth {
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectedConfig {
    #[serde(default)]
    labels: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    healthcheck: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListedImage {
    id: String,
    #[serde(default)]
    names: Option<Vec<String>>,
    #[serde(default)]
    repo_digests: Option<Vec<String>>,
    #[serde(default)]
    digest: String,
    #[serde(default)]
    size: u64,
}

fn parse_images(json: &str) -> HostResult<Vec<LocalImage>> {
    let listed: Vec<ListedImage> =
        serde_json::from_str(json).map_err(|e| format!("podman images output: {e}"))?;
    Ok(listed
        .into_iter()
        .map(|i| LocalImage {
            id: i.id,
            names: i.names.unwrap_or_default(),
            repo_digests: i.repo_digests.unwrap_or_default(),
            digest: i.digest,
            size: i.size,
        })
        .collect())
}

fn parse_containers(json: &str) -> HostResult<Vec<Container>> {
    let inspected: Vec<Inspected> =
        serde_json::from_str(json).map_err(|e| format!("podman inspect output: {e}"))?;
    Ok(inspected
        .into_iter()
        .filter_map(|c| {
            let labels = c.config.labels.unwrap_or_default();
            let service = labels.get(SERVICE_LABEL)?.clone();
            let health = c
                .state
                .health
                .or(c.state.healthcheck)
                .map(|h| h.status)
                .filter(|s| !s.is_empty());
            let has_healthcheck = c
                .config
                .healthcheck
                .as_ref()
                .is_some_and(|h| !h.is_null() && h.get("Test").is_some_and(|t| !t.is_null()));
            let exited = matches!(c.state.status.as_str(), "exited" | "stopped");
            Some(Container {
                id: c.id,
                name: c.name,
                service,
                running: c.state.running,
                exit_code: exited.then_some(c.state.exit_code),
                restarts: c.restart_count,
                has_healthcheck,
                health,
                image_name: c.image_name,
                image_id: c.image,
                image_digests: Some(c.image_digest)
                    .filter(|d| !d.is_empty())
                    .into_iter()
                    .collect(),
                created: DateTime::parse_from_rfc3339(&c.created)
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// Adds to each container the other digests local storage knows its image
/// by, after the one podman reports.
fn add_image_digests(containers: &mut [Container], images: &[LocalImage]) {
    for c in containers {
        let Some(image) = images.iter().find(|i| i.id == c.image_id) else {
            continue;
        };
        for digest in image.digests() {
            if !c.image_digests.iter().any(|d| d == digest) {
                c.image_digests.push(digest.to_string());
            }
        }
    }
}

impl Host for PodmanHost {
    fn now(&mut self) -> DateTime<Utc> {
        Utc::now()
    }

    fn sleep(&mut self, seconds: u64) {
        std::thread::sleep(Duration::from_secs(seconds));
    }

    fn checkpoint(&mut self, _label: &str) {}

    fn tool_versions(&mut self) -> ToolVersions {
        let podman = self.podman_version().unwrap_or_default();
        let compose = self.compose_version().unwrap_or_default();
        ToolVersions::new(&podman, &compose)
    }

    fn pull(&mut self, reference: &str) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.args(["pull", "--quiet", reference]);
        run(cmd, Some(self.pull_timeout)).map(drop)
    }

    fn extract(&mut self, image: &str, files: &[(&str, &Path)]) -> HostResult<()> {
        let name = format!("laysi-updater-extract-{}", std::process::id());
        let mut rm = self.podman();
        rm.args(["rm", "--force", "--ignore", &name]);
        run(rm, None)?;
        let mut create = self.podman();
        create.args(["create", "--name", &name, image]);
        run(create, None)?;
        let copied = files.iter().try_for_each(|(inside, dest)| {
            let _ = std::fs::remove_file(dest);
            let mut cp = self.podman();
            cp.arg("cp").arg(format!("{name}:{inside}")).arg(dest);
            run(cp, None).map(drop)
        });
        let mut rm = self.podman();
        rm.args(["rm", "--force", "--ignore", &name]);
        let removed = run(rm, None);
        copied?;
        removed.map(drop)
    }

    fn compose_pull(&mut self, files: &StackFiles) -> HostResult<()> {
        let mut cmd = self.compose(files);
        cmd.arg("pull");
        run(cmd, Some(self.pull_timeout)).map(drop)
    }

    fn compose_config(&mut self, files: &StackFiles) -> HostResult<Vec<Service>> {
        let mut cmd = self.compose(files);
        cmd.arg("config");
        parse_config(&run(cmd, Some(Duration::from_secs(120)))?)
    }

    fn compose_up(
        &mut self,
        files: &StackFiles,
        services: &[String],
        recreate: bool,
    ) -> HostResult<()> {
        if services.is_empty() {
            return Ok(());
        }
        let cmd = self.up_command(files, services, recreate);
        let name = file_name(OsStr::new(&self.compose));
        run_as(cmd, &name, Some(Duration::from_secs(30 * 60))).map(drop)
    }

    /// With every digest each container's image is known by. Without the
    /// image list each keeps the one podman reports for it.
    fn containers(&mut self) -> HostResult<Vec<Container>> {
        let mut containers = self.inspect_containers()?;
        if !containers.is_empty() {
            match self.images() {
                Ok(images) => add_image_digests(&mut containers, &images),
                Err(e) => eprintln!(
                    "{} listing images for the containers: {e}",
                    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ),
            }
        }
        Ok(containers)
    }

    fn stop(&mut self, names: &[String]) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.args(["stop", "--time", "30"]).args(names);
        run(cmd, None).map(drop)
    }

    fn remove_containers(&mut self, names: &[String]) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.args(["rm", "--ignore"]).args(names);
        run(cmd, Some(Duration::from_secs(120))).map(drop)
    }

    fn wait_exit(&mut self, name: &str) -> HostResult<i64> {
        let mut cmd = self.podman();
        cmd.args(["wait", name]);
        let out = run(cmd, Some(Duration::from_secs(10 * 60)))?;
        out.trim()
            .parse()
            .map_err(|_| format!("podman wait printed {out:?}"))
    }

    fn database_size(&mut self, postgres: &str) -> HostResult<u64> {
        let mut cmd = self.podman();
        cmd.args(["exec", postgres, "sh", "-c", PSQL_SIZE]);
        let out = run(cmd, Some(Duration::from_secs(60)))?;
        out.trim()
            .parse()
            .map_err(|_| format!("database size {out:?}"))
    }

    fn dump_database(&mut self, postgres: &str, to: &mut File) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.args(["exec", postgres, "sh", "-c", PG_DUMP]);
        run_with_file(cmd, to, false, DATABASE_TIMEOUT)
    }

    fn check_dump(&mut self, postgres: &str, dump: &mut File) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.args(["exec", "-i", postgres, "pg_restore", "--list"]);
        run_with_file(cmd, dump, true, DATABASE_TIMEOUT)
    }

    fn restore_database(&mut self, postgres: &str, from: &mut File) -> HostResult<()> {
        self.drop_database(postgres)?;
        let mut cmd = self.podman();
        cmd.args(["exec", "-i", postgres, "sh", "-c", PG_RESTORE]);
        run_with_file(cmd, from, true, DATABASE_TIMEOUT)
    }

    fn drop_database(&mut self, postgres: &str) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.args(["exec", postgres, "sh", "-c", PG_DROP]);
        run(cmd, Some(Duration::from_secs(10 * 60))).map(drop)
    }

    fn free_bytes(&mut self, path: &Path) -> HostResult<u64> {
        fsx::free_bytes(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn same_device(&mut self, a: &Path, b: &Path) -> HostResult<bool> {
        fsx::same_device(a, b).map_err(|e| e.to_string())
    }

    fn image_store(&mut self) -> HostResult<PathBuf> {
        let mut cmd = self.podman();
        cmd.args(["info", "--format", "{{.Store.GraphRoot}}"]);
        Ok(PathBuf::from(run(cmd, None)?.trim()))
    }

    fn images(&mut self) -> HostResult<Vec<LocalImage>> {
        let mut cmd = self.podman();
        cmd.args(["images", "--all", "--format", "json"]);
        parse_images(&run(cmd, None)?)
    }

    fn remove_image(&mut self, image: &LocalImage) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.arg("rmi").args(image.removal_targets());
        run(cmd, None).map(drop)
    }

    fn self_check(&mut self, binary: &Path) -> HostResult<()> {
        let mut cmd = Command::new(binary);
        cmd.arg("self-check").env("LAYSI_UPDATER_ROOT", &self.root);
        run(cmd, Some(Duration::from_secs(60))).map(drop)
    }

    fn install_units(&mut self, binary: &Path) -> HostResult<()> {
        let mut cmd = Command::new(binary);
        cmd.arg("write-units").env("LAYSI_UPDATER_ROOT", &self.root);
        run(cmd, Some(Duration::from_secs(60)))?;
        let mut reload = Command::new(&self.systemctl);
        reload.args(["--user", "daemon-reload"]);
        run(reload, Some(Duration::from_secs(60))).map(drop)
    }

    fn clear_requests(&mut self, dir: &Path) -> HostResult<()> {
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = self.podman();
        cmd.arg("unshare")
            .arg(me)
            .arg("clear-requests-inside")
            .arg(dir);
        run(cmd, Some(Duration::from_secs(60))).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The run unit ends, and its cgroup with it, right after the last
    /// `compose up`; a port forwarder started from there must not be in it.
    #[test]
    fn containers_are_started_in_a_scope_that_outlives_the_run() {
        let host = PodmanHost {
            podman: "/usr/bin/podman".into(),
            compose: "/opt/bin/podman-compose".into(),
            systemctl: "/usr/bin/systemctl".into(),
            systemd_run: "/usr/bin/systemd-run".into(),
            root: "/home/stack/.local/share/laysi-one".into(),
            pull_timeout: Duration::from_secs(1),
        };
        let stack = Path::new("/home/stack/.local/share/laysi-one/stack");
        let files = [
            stack.join("docker-compose.prod.yml"),
            stack.join("docker-compose.ship.yml"),
            stack.join(".env"),
        ];
        let cmd = host.up_command(&files, &["web".into(), "mcp".into()], true);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(cmd.get_program(), "/usr/bin/systemd-run");
        assert_eq!(
            args[..10],
            [
                "--user",
                "--scope",
                "--collect",
                "--quiet",
                "--property=DefaultDependencies=no",
                "--",
                "env",
                "-u",
                "INVOCATION_ID",
                "/opt/bin/podman-compose"
            ]
        );
        assert_eq!(
            args[args.len() - 6..],
            ["up", "-d", "--no-deps", "--force-recreate", "web", "mcp"]
        );
        assert_eq!(cmd.get_current_dir(), Some(stack));
        assert!(cmd
            .get_envs()
            .any(|(key, value)| key == "INVOCATION_ID" && value.is_none()));
    }

    /// The error is what podman 4.9.3 printed after a power cut during a
    /// rollback, for the postgres container it had just created (#64).
    #[test]
    fn only_a_container_podman_left_broken_is_removed() {
        let half_written = "podman exited with exit status: 125: Error: getting graph driver info \"33ae49a17a82\": readlink /home/stack/.local/share/containers/storage/overlay: invalid argument";
        assert_eq!(unreadable("created", half_written), Unreadable::Remove);
        assert_eq!(unreadable("exited", half_written), Unreadable::Remove);
        assert_eq!(unreadable("configured", half_written), Unreadable::Remove);
        assert_eq!(unreadable("running", half_written), Unreadable::Fail);
        assert_eq!(unreadable("paused", half_written), Unreadable::Fail);
        assert_eq!(unreadable("stopping", half_written), Unreadable::Fail);
        let no_storage = "Error: container not known";
        assert_eq!(unreadable("removing", no_storage), Unreadable::Remove);
        assert_eq!(unreadable("running", no_storage), Unreadable::Remove);
        assert_eq!(
            unreadable("exited", "Error: no such container 33ae49a17a82"),
            Unreadable::Gone
        );
        for other in [
            "Error: getting graph driver info \"33ae49a17a82\": readlink /home/stack/.local/share/containers/storage/overlay/l/QJ3V: permission denied",
            "Error: getting graph driver info \"33ae49a17a82\": open /home/stack/.local/share/containers/storage/overlay/33ae49a17a82/lower: input/output error",
            "Error: permission denied",
        ] {
            assert_eq!(unreadable("exited", other), Unreadable::Fail, "{other}");
        }
    }

    #[test]
    fn a_wrapped_command_fails_under_the_name_given() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo broken >&2; exit 3"]);
        let error = run_as(cmd, "podman-compose", None).unwrap_err();
        assert!(error.starts_with("podman-compose exited with"), "{error}");
        assert!(error.ends_with("broken"), "{error}");
    }

    #[test]
    fn a_deadline_ends_what_the_command_started_too() {
        let dir = std::env::temp_dir().join(format!("laysi-updater-group-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("sleep 30 & echo $! > {}; wait", pidfile.display()));
        let started = Instant::now();
        let outcome = run(cmd, Some(Duration::from_secs(1)));
        assert!(outcome.unwrap_err().contains("did not finish"));
        assert!(started.elapsed() < Duration::from_secs(15));
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Reaped by init or gone: either way no longer this group's sleep.
        std::thread::sleep(Duration::from_millis(200));
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "the background sleep survived the deadline");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn services_and_images_are_read_from_compose_config() {
        let yaml = "services:\n  api:\n    environment:\n      image: not-this\n    image: ghcr.io/laysi/laysi-one-api@sha256:11\n    restart: unless-stopped\n  secrets:\n    command:\n    - sh\n    image: 'docker.io/library/alpine:3.24@sha256:22'\nvolumes:\n  pgdata: {}\nx-podman:\n  in_pod: false\n";
        let services = parse_config(yaml).unwrap();
        assert_eq!(
            services,
            vec![
                Service {
                    name: "api".into(),
                    image: "ghcr.io/laysi/laysi-one-api@sha256:11".into()
                },
                Service {
                    name: "secrets".into(),
                    image: "docker.io/library/alpine:3.24@sha256:22".into()
                },
            ]
        );
        assert!(parse_config("services:\n  api:\n    restart: always\n").is_err());
        assert!(parse_config("volumes: {}\n").is_err());
    }

    #[test]
    fn the_compose_version_is_read_from_its_own_line() {
        let out = "podman version 4.9.3\npodman-compose version 1.6.0\n";
        assert_eq!(parse_compose_version(out), Some("1.6.0"));
        assert_eq!(
            parse_compose_version("podman-compose version: 1.0.6\n['podman', '--version', '']\nusing podman version: 4.9.3\n"),
            Some("1.0.6")
        );
        assert_eq!(parse_compose_version("podman version 4.9.3\n"), None);
    }

    #[test]
    fn containers_are_read_from_podman_inspect() {
        let json = r#"[
          {"Id":"aaa","Name":"laysi-one-api","Created":"2026-10-12T09:06:23.447972097Z",
           "State":{"Status":"running","Running":true,"ExitCode":0,"Health":{"Status":"healthy"}},
           "ImageName":"ghcr.io/laysi/laysi-one-api@sha256:11","Image":"img1","ImageDigest":"sha256:11",
           "Config":{"Labels":{"com.docker.compose.service":"api","io.podman.compose.project":"laysi-one"},
                     "Healthcheck":{"Test":["CMD","/usr/local/bin/laysi-server","--healthcheck"]}}},
          {"Id":"bbb","Name":"laysi-one-secrets","Created":"2026-10-12T09:06:20Z",
           "State":{"Status":"exited","Running":false,"ExitCode":0,"Health":{"Status":""}},
           "ImageName":"docker.io/library/alpine@sha256:22","ImageDigest":"sha256:22",
           "Config":{"Labels":{"com.docker.compose.service":"secrets"},"Healthcheck":null}},
          {"Id":"ccc","Name":"stray","Created":"2026-10-12T09:06:20Z",
           "State":{"Status":"running","Running":true,"ExitCode":0},
           "ImageName":"x","ImageDigest":"y","Config":{"Labels":null}}
        ]"#;
        let containers = parse_containers(json).unwrap();
        assert_eq!(
            containers.len(),
            2,
            "a container without a service label is not ours"
        );
        let api = &containers[0];
        assert_eq!(api.service, "api");
        assert!(api.running && api.has_healthcheck);
        assert_eq!(api.health.as_deref(), Some("healthy"));
        assert_eq!(api.exit_code, None);
        assert_eq!(api.image_id, "img1");
        assert_eq!(api.image_digests, ["sha256:11"]);
        let secrets = &containers[1];
        assert!(!secrets.running && !secrets.has_healthcheck);
        assert_eq!(secrets.exit_code, Some(0));
        assert_eq!(secrets.health, None);
    }

    #[test]
    fn a_container_knows_every_digest_its_image_was_pulled_by() {
        let json = r#"[
          {"Id":"aaa","Name":"laysi-one-backup","Created":"2026-10-15T15:03:29Z",
           "State":{"Status":"running","Running":true,"ExitCode":0},
           "ImageName":"ghcr.io/laysi/laysi-one-backup@sha256:bb","Image":"img1","ImageDigest":"sha256:aa",
           "Config":{"Labels":{"com.docker.compose.service":"backup"}}},
          {"Id":"bbb","Name":"laysi-one-web","Created":"2026-10-15T15:03:29Z",
           "State":{"Status":"running","Running":true,"ExitCode":0},
           "ImageName":"ghcr.io/laysi/laysi-one-web@sha256:cc","Image":"img2","ImageDigest":"sha256:cc",
           "Config":{"Labels":{"com.docker.compose.service":"web"}}}
        ]"#;
        let mut containers = parse_containers(json).unwrap();
        let images = [
            LocalImage {
                id: "img1".into(),
                repo_digests: vec![
                    "ghcr.io/laysi/laysi-one-backup@sha256:aa".into(),
                    "ghcr.io/laysi/laysi-one-backup@sha256:bb".into(),
                ],
                names: Vec::new(),
                digest: "sha256:aa".into(),
                size: 1,
            },
            LocalImage {
                id: "img3".into(),
                repo_digests: vec!["ghcr.io/laysi/laysi-one-web@sha256:dd".into()],
                names: Vec::new(),
                digest: "sha256:dd".into(),
                size: 1,
            },
        ];
        add_image_digests(&mut containers, &images);
        assert_eq!(containers[0].image_digests, ["sha256:aa", "sha256:bb"]);
        assert_eq!(
            containers[1].image_digests,
            ["sha256:cc"],
            "another image's digests are not this container's"
        );
    }

    /// podman 4.9.3 answers `rmi <id>` of an image with two names "unable
    /// to delete image … by ID with more than one tag". A platform manifest
    /// digest is a repo digest but not a name.
    #[test]
    fn an_image_pulled_under_two_digests_is_removed_by_both_names() {
        let json = r#"[
          {"Id":"bf84137d58a4","Names":["r/backup@sha256:58","r/backup@sha256:cd"],
           "RepoDigests":["r/backup@sha256:58","r/backup@sha256:5c","r/backup@sha256:cd"],
           "Digest":"sha256:cd","Size":10},
          {"Id":"7cf3a1e3f07f","Names":["r/api@sha256:10"],
           "RepoDigests":["r/api@sha256:10","r/api@sha256:77"],"Digest":"sha256:10","Size":20},
          {"Id":"0123456789ab","Names":null,"RepoDigests":[],"Digest":"","Size":5}
        ]"#;
        let images = parse_images(json).unwrap();
        assert_eq!(
            images[0].removal_targets(),
            ["r/backup@sha256:58", "r/backup@sha256:cd"]
        );
        assert_eq!(images[1].removal_targets(), ["r/api@sha256:10"]);
        assert_eq!(images[2].removal_targets(), ["0123456789ab"]);
    }
}
