//! The real host: podman, podman-compose and systemctl as the stack user.
//!
//! Tool paths come from `state/updater.env`, which `install` writes with the
//! absolute paths it found, because a systemd user unit does not see the
//! PATH of an interactive shell.

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
fn run(mut cmd: Command, timeout: Option<Duration>) -> HostResult<String> {
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
            "{} exited with {status}: {}",
            program_of(&cmd),
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
    Path::new(cmd.get_program())
        .file_name()
        .unwrap_or(cmd.get_program())
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
                image_digest: c.image_digest,
                created: DateTime::parse_from_rfc3339(&c.created)
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_default(),
            })
        })
        .collect())
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
        let mut cmd = self.compose(files);
        cmd.args(["up", "-d", "--no-deps"]);
        if recreate {
            cmd.arg("--force-recreate");
        }
        cmd.args(services);
        run(cmd, Some(Duration::from_secs(30 * 60))).map(drop)
    }

    /// One container at a time when inspecting them together fails: one
    /// removed between the two calls is skipped, and one an interrupted
    /// podman left half created or half removed is removed.
    fn containers(&mut self) -> HostResult<Vec<Container>> {
        let mut ps = self.podman();
        ps.args(["ps", "--all", "--quiet", "--no-trunc", "--filter"])
            .arg(format!("label={PROJECT_LABEL}={PROJECT}"));
        let ids: Vec<String> = run(ps, None)?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut inspect = self.podman();
        inspect.args(["container", "inspect"]).args(&ids);
        if let Ok(json) = run(inspect, None) {
            return parse_containers(&json);
        }
        let mut found = Vec::new();
        for id in &ids {
            let mut inspect = self.podman();
            inspect.args(["container", "inspect", id]);
            match run(inspect, None) {
                Ok(json) => found.extend(parse_containers(&json)?),
                Err(e) if e.contains("no such container") => {}
                // Its record is in podman's database and its storage is not:
                // a create or a remove was killed halfway, which leaves the
                // container stuck in "removing" and its name taken. It holds
                // nothing, since the data lives in volumes, and recreating
                // its service fails until it is gone.
                Err(e) if e.contains("container not known") => {
                    let mut rm = self.podman();
                    rm.args(["rm", "--force", id]);
                    run(rm, Some(Duration::from_secs(120)))
                        .map_err(|rm| format!("{e}; removing it: {rm}"))?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(found)
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
        let listed: Vec<ListedImage> = serde_json::from_str(&run(cmd, None)?)
            .map_err(|e| format!("podman images output: {e}"))?;
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

    fn remove_image(&mut self, id: &str) -> HostResult<()> {
        let mut cmd = self.podman();
        cmd.args(["rmi", id]);
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
           "ImageName":"ghcr.io/laysi/laysi-one-api@sha256:11","ImageDigest":"sha256:11",
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
        let secrets = &containers[1];
        assert!(!secrets.running && !secrets.has_healthcheck);
        assert_eq!(secrets.exit_code, Some(0));
        assert_eq!(secrets.health, None);
    }
}
