//! `install`: the first time, at the factory, as the user that will own the
//! stack. It is the only shipping step: given a signed manifest it lays out
//! the host, takes the compose files out of that release's api image, writes
//! the first `state/installed` and `.env`, writes the units, makes sure the
//! user lingers so they run without a login, and starts the stack through
//! the boot unit, which runs `secrets` before anything else.
//!
//! Everything that can refuse, the manifest above all, is checked before
//! anything is written, and a run that fails before `state/installed` is
//! written can simply be run again. Once it is written the machine is
//! installed: this command refuses to run again, and the boot unit starts
//! the stack at every boot.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use laysi_license_token::release::{self, Image, MAX_MANIFEST_BYTES};

use crate::envfile::{self, EnvFile};
use crate::fsx;
use crate::host::Host;
use crate::journal::{self, Release};
use crate::layout::{Layout, COMPOSE_FILES, IMAGE_SHARE};
use crate::machine::check_release_files;
use crate::podman::PodmanHost;
use crate::units;

const MAX_COMPOSE_BYTES: usize = 256 * 1024;
const MAX_UPDATER_BYTES: usize = 64 * 1024 * 1024;

pub fn install(args: &[String]) -> Result<(), String> {
    let mut manifest_path = None;
    let mut registry = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manifest" => manifest_path = it.next().map(PathBuf::from),
            "--registry" => registry = it.next().cloned(),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let manifest_path = manifest_path.ok_or("--manifest <file> is required")?;
    let layout = Layout::from_env()?;
    if !units::plain_path(&layout.root) {
        return Err(format!(
            "{} must be an absolute path of letters, digits, '/', '.', '_' and '-'",
            layout.root.display()
        ));
    }
    if layout.installed().exists() {
        return Err(format!(
            "{} already has an installed release",
            layout.root.display()
        ));
    }
    if let Some(registry) = &registry {
        let plain = !registry.is_empty()
            && registry
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"./:_-".contains(&b));
        if !plain {
            return Err(format!("{registry} is not a registry name"));
        }
    }
    runtime_dir();

    // Everything that can refuse comes before anything is written.
    let text = read_manifest(&manifest_path)?;
    let manifest =
        release::verify_with(&text, &crate::trusted_keys()).map_err(|e| e.to_string())?;
    let release = Release::of(&manifest);
    let tools = tool_env()?;
    for line in tools.lines() {
        if let Some((key, value)) = line.split_once('=') {
            std::env::set_var(key, value);
        }
    }
    let mut host = PodmanHost::from_env(&layout.root);
    network_backend(&host.podman)?;
    println!("installing {}", release.version);

    for (dir, mode) in layout.directories() {
        fsx::ensure_dir(&dir, mode).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    fsx::copy_atomic(&me, &layout.current_updater(), 0o755, MAX_UPDATER_BYTES)
        .map_err(|e| format!("installing the updater: {e}"))?;
    fsx::write_atomic(&layout.tool_env(), tools.as_bytes(), 0o600).map_err(|e| e.to_string())?;

    let mut env = EnvFile::default();
    if let Some(registry) = &registry {
        env.set(envfile::REGISTRY, registry);
    }
    let env = env.for_release(&release, &layout.root.to_string_lossy());
    let registry = env.registry();
    let api = format!(
        "{}@{}",
        envfile::repository(&registry, Image::Api),
        release.digest(Image::Api)
    );
    host.pull(&api)?;
    let incoming = layout.incoming_files();
    let sources: Vec<String> = COMPOSE_FILES
        .iter()
        .map(|name| format!("{IMAGE_SHARE}/{name}"))
        .collect();
    host.extract(
        &api,
        &[
            (sources[0].as_str(), &incoming[0]),
            (sources[1].as_str(), &incoming[1]),
        ],
    )?;
    fsx::write_atomic(&incoming[2], env.render().as_bytes(), 0o600).map_err(|e| e.to_string())?;
    let services = host.compose_config(&incoming)?;
    check_release_files(&services, &release, &registry)?;
    let stack = layout.stack_files();
    for (from, to) in incoming.iter().zip(stack.iter()) {
        fsx::copy_atomic(from, to, 0o600, MAX_COMPOSE_BYTES).map_err(|e| e.to_string())?;
    }
    fsx::empty_own_dir(&layout.incoming_dir()).map_err(|e| e.to_string())?;
    host.compose_pull(&stack)?;

    units::write(&layout)?;
    linger()?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", units::PATH_UNIT, units::BOOT_UNIT])?;

    // From here the machine is installed: install refuses to run again, and
    // the boot unit, not this command, is what starts the stack.
    fsx::write_atomic(
        &layout.installed(),
        journal::encode_installed(&release).as_bytes(),
        0o600,
    )
    .map_err(|e| e.to_string())?;
    // Blocks until the boot unit has run: secrets, then the stack.
    let started = systemctl(&["start", units::BOOT_UNIT]);
    systemctl(&["start", units::PATH_UNIT])?;
    started.map_err(|e| {
        format!(
            "{e}: {} is installed but its stack did not come up; every boot tries again, \
             or `systemctl --user start {}` now",
            release.version,
            units::BOOT_UNIT
        )
    })?;
    println!("installed {} in {}", release.version, layout.root.display());
    Ok(())
}

fn read_manifest(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut text = String::new();
    file.take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(text)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Absolute paths of the tools, and a PATH that reaches them: units do not
/// see the PATH of the shell that ran the install, and podman-compose runs
/// `podman` by name.
fn tool_env() -> Result<String, String> {
    let mut out = String::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for (key, name) in [
        ("LAYSI_PODMAN", "podman"),
        ("LAYSI_PODMAN_COMPOSE", "podman-compose"),
        ("LAYSI_SYSTEMCTL", "systemctl"),
    ] {
        let found = which(name).ok_or_else(|| format!("{name} is not on PATH"))?;
        if !units::plain_path(&found) {
            return Err(format!(
                "{} has characters a unit cannot take",
                found.display()
            ));
        }
        if let Some(dir) = found.parent() {
            if !dirs.iter().any(|d| d == dir) {
                dirs.push(dir.to_path_buf());
            }
        }
        out.push_str(&format!("{key}={}\n", found.display()));
    }
    for dir in ["/usr/local/bin", "/usr/bin", "/bin"] {
        if !dirs.iter().any(|d| d == Path::new(dir)) {
            dirs.push(PathBuf::from(dir));
        }
    }
    let path: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    out.push_str(&format!("PATH={}\n", path.join(":")));
    Ok(out)
}

/// The services find each other by name. netavark answers those names with
/// aardvark-dns; CNI needs a plugin that podman 4.9 on Ubuntu does not bring,
/// and without it the api never reaches postgres.
fn network_backend(podman: &str) -> Result<(), String> {
    let out = Command::new(podman)
        .args(["info", "--format", "{{.Host.NetworkBackend}}"])
        .output()
        .map_err(|e| format!("{podman}: {e}"))?;
    let backend = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if backend == "netavark" {
        Ok(())
    } else {
        Err(format!(
            "podman uses the {backend:?} network backend; install netavark and aardvark-dns and run `podman system reset` first"
        ))
    }
}

/// An ssh session without a login has no runtime directory set, and both
/// rootless podman and `systemctl --user` need it.
fn runtime_dir() {
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let uid = unsafe { libc::getuid() };
        let dir = format!("/run/user/{uid}");
        if Path::new(&dir).is_dir() {
            std::env::set_var("XDG_RUNTIME_DIR", dir);
        }
    }
}

fn user() -> Result<String, String> {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| "USER is not set".to_string())
}

/// The units run at boot only when the user lingers. Enabling it for
/// oneself is allowed on most systems; where it is not, root has to do it
/// once.
fn linger() -> Result<(), String> {
    let user = user()?;
    let lingering = Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger", "--value"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes");
    if lingering {
        return Ok(());
    }
    let status = Command::new("loginctl")
        .args(["enable-linger", &user])
        .status()
        .map_err(|e| format!("loginctl: {e}"))?;
    if !status.success() {
        return Err(format!(
            "loginctl enable-linger {user} failed; run it as root once, then install again"
        ));
    }
    Ok(())
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .map_err(|e| format!("systemctl: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("systemctl --user {} failed", args.join(" ")))
    }
}
