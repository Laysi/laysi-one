//! The command line of the host-side updater.
//!
//! ```text
//! laysi-updater install --manifest <file> [--registry <registry>]
//! laysi-updater run                 # a request arrived (laysi-updater.service)
//! laysi-updater boot                # recover, then start the stack (boot unit)
//! laysi-updater boot --fallback     # the same as the previous updater (fallback unit)
//! laysi-updater discard-requests    # after every run (ExecStopPost)
//! laysi-updater stop-stack          # at shutdown (ExecStop of the boot unit)
//! laysi-updater self-check          # run on a new updater before it replaces this one
//! laysi-updater write-units
//! laysi-updater retry-rollback      # by hand, after a rollback-failed
//! laysi-updater status
//! laysi-updater version
//! ```
//!
//! `LAYSI_UPDATER_ROOT` names the directory everything lives in (see
//! `layout`); the units set it.

use std::path::Path;
use std::process::{Command, ExitCode};

use laysi_updater::host::Host;
use laysi_updater::journal::Phase;
use laysi_updater::layout::Layout;
use laysi_updater::machine::{Config, Machine, DATABASE, ENTRIES};
use laysi_updater::podman::PodmanHost;
use laysi_updater::{fsx, install, trusted_keys, units, TEST_KEYS};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("laysi-updater: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: &[String]) -> Result<(), String> {
    let command = args.first().map(String::as_str).unwrap_or("");
    match command {
        "run" => with_machine(|m| m.run_request()),
        "boot" if args.get(1).is_some_and(|a| a == "--fallback") => {
            with_machine(|m| m.fallback_boot())
        }
        "boot" => with_machine(|m| m.boot()),
        "discard-requests" => with_machine(|m| m.discard_requests(run_failed())),
        "clear-requests-inside" => clear_requests_inside(args.get(1)),
        "stop-stack" => stop_stack(),
        "self-check" => self_check(),
        "write-units" => units::write(&Layout::from_env()?),
        "install" => install::install(&args[1..]),
        "retry-rollback" => retry_rollback(),
        "status" => status(),
        "version" | "--version" => {
            println!("{}", version());
            Ok(())
        }
        _ => Err("usage: laysi-updater install|run|boot|discard-requests|stop-stack|self-check|write-units|retry-rollback|status|version".into()),
    }
}

/// Whether the run this `ExecStopPost=` follows failed. systemd sets
/// `SERVICE_RESULT` for the commands it runs after a service; run by hand,
/// there is no run to have failed.
fn run_failed() -> bool {
    std::env::var("SERVICE_RESULT").is_ok_and(|r| r != "success")
}

fn version() -> String {
    let build = option_env!("LAYSI_BUILD").unwrap_or(env!("CARGO_PKG_VERSION"));
    if TEST_KEYS {
        format!("laysi-updater {build} (trusts test keys)")
    } else {
        format!("laysi-updater {build}")
    }
}

fn with_machine(
    f: impl FnOnce(&mut Machine<PodmanHost>) -> Result<(), String>,
) -> Result<(), String> {
    let layout = Layout::from_env()?;
    let mut host = PodmanHost::from_env(&layout.root);
    let mut machine = Machine::new(&mut host, layout, trusted_keys(), Config::from_env());
    let outcome = f(&mut machine);
    if let Err(e) = &outcome {
        machine.log(format!("failed: {e}"));
    }
    outcome
}

/// Inside `podman unshare`, where the requests directory's entries, which
/// belong to the api's uid, can be removed.
fn clear_requests_inside(dir: Option<&String>) -> Result<(), String> {
    let dir = Path::new(dir.ok_or("which directory?")?);
    let expected = Layout::from_env()?.requests_dir();
    if dir != expected {
        return Err(format!("{} is not {}", dir.display(), expected.display()));
    }
    fsx::clear_untrusted(dir)
        .map(drop)
        .map_err(|e| format!("emptying {}: {e}", dir.display()))
}

/// Stop the stack at shutdown, web and mcp first and postgres last, so the
/// database is shut down cleanly rather than cut off.
fn stop_stack() -> Result<(), String> {
    let layout = Layout::from_env()?;
    let mut host = PodmanHost::from_env(&layout.root);
    let containers = host.containers()?;
    let running = |filter: &dyn Fn(&str) -> bool| -> Vec<String> {
        containers
            .iter()
            .filter(|c| c.running && filter(&c.service))
            .map(|c| c.name.clone())
            .collect()
    };
    let entries = running(&|s| ENTRIES.contains(&s));
    let rest = running(&|s| !ENTRIES.contains(&s) && s != DATABASE);
    let database = running(&|s| s == DATABASE);
    for group in [entries, rest, database] {
        if !group.is_empty() {
            host.stop(&group)?;
        }
    }
    Ok(())
}

/// What a new updater has to pass before it replaces the running one: it
/// executes on this host, trusts a release key, can render its units and
/// reads the state the running one wrote, the journal of this very update
/// included.
fn self_check() -> Result<(), String> {
    let layout = Layout::from_env()?;
    if trusted_keys().is_empty() {
        return Err("no trusted release key compiled in".into());
    }
    units::render(&layout)?;
    let mut host = PodmanHost::from_env(&layout.root);
    let machine = Machine::new(&mut host, layout.clone(), Vec::new(), Config::default());
    machine
        .load_journal()
        .map_err(|e| format!("journal: {e}"))?;
    if layout.installed().exists() {
        machine
            .load_installed()
            .map_err(|e| format!("installed: {e}"))?;
    }
    println!("{} ok", version());
    Ok(())
}

/// Set a rollback that failed back to a phase it is finished from, have
/// the recovery unit finish it, and say how it ended. The unit does the
/// work rather than this process: a restore outlives the ssh session that
/// asked for it, and the unit runs with the tool paths of `updater.env`.
fn retry_rollback() -> Result<(), String> {
    install::runtime_dir();
    let layout = Layout::from_env()?;
    let mut host = PodmanHost::from_env(&layout.root);
    let systemctl = host.systemctl.clone();
    let (request, restores) = {
        let mut machine = Machine::new(&mut host, layout.clone(), Vec::new(), Config::default());
        let request = machine.retry_rollback()?;
        let restores = matches!(machine.load_journal(), Ok(Some(j)) if j.restore_db);
        (request, restores)
    };
    if restores {
        println!("{request}: 重新還原資料庫並啟動更新前的版本，完成前不要關機");
    } else {
        println!("{request}: 啟動更新前的版本，完成前不要關機");
    }
    let started = Command::new(&systemctl)
        .args(["--user", "start", units::RECOVER_UNIT])
        .status()
        .map_err(|e| format!("{systemctl}: {e}"))?;
    let machine = Machine::new(&mut host, layout, Vec::new(), Config::default());
    match machine.load_journal()? {
        None => {
            let running = machine
                .read_result(&request)
                .and_then(|r| r.running)
                .map(|v| v.to_string())
                .unwrap_or_default();
            println!("{request}: 已回到 {running}");
            Ok(())
        }
        Some(j) if j.phase == Phase::RollbackFailed => Err(format!(
            "{request}: 仍然回退失敗{}：{}",
            if j.restore_db {
                "，資料庫備份保留著"
            } else {
                ""
            },
            j.rollback_reason.unwrap_or_default()
        )),
        Some(j) => Err(format!(
            "{request}: 停在 {}，沒有做完（systemctl {started}）；下一次開機會接著做",
            j.phase
        )),
    }
}

fn status() -> Result<(), String> {
    let layout = Layout::from_env()?;
    let mut host = PodmanHost::from_env(&layout.root);
    let machine = Machine::new(&mut host, layout.clone(), Vec::new(), Config::default());
    println!("{}", version());
    match machine.load_installed() {
        Ok(release) => println!("installed {}", release.version),
        Err(e) => println!("installed unknown ({e})"),
    }
    match machine.load_journal() {
        Ok(Some(j)) => println!("journal {} {}", j.request, j.phase),
        Ok(None) => println!("journal none"),
        Err(e) => println!("journal unreadable ({e})"),
    }
    if let Ok(entries) = std::fs::read_dir(layout.backup_dir()) {
        for entry in entries.flatten() {
            println!("backup {}", entry.path().display());
        }
    }
    Ok(())
}
