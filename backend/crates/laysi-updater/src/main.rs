//! The command line of the host-side updater.
//!
//! ```text
//! laysi-updater install --manifest <file> [--registry <registry>]
//! laysi-updater run                 # a request arrived (laysi-updater.service)
//! laysi-updater boot                # recover, then start the stack (boot unit)
//! laysi-updater discard-requests    # after every run (ExecStopPost)
//! laysi-updater stop-stack          # at shutdown (ExecStop of the boot unit)
//! laysi-updater self-check          # run on a new updater before it replaces this one
//! laysi-updater write-units
//! laysi-updater status
//! laysi-updater version
//! ```
//!
//! `LAYSI_UPDATER_ROOT` names the directory everything lives in (see
//! `layout`); the units set it.

use std::path::Path;
use std::process::ExitCode;

use laysi_updater::host::Host;
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
        "boot" => with_machine(|m| m.boot()),
        "discard-requests" => with_machine(|m| m.discard_requests()),
        "clear-requests-inside" => clear_requests_inside(args.get(1)),
        "stop-stack" => stop_stack(),
        "self-check" => self_check(),
        "write-units" => units::write(&Layout::from_env()?),
        "install" => install::install(&args[1..]),
        "status" => status(),
        "version" | "--version" => {
            println!("{}", version());
            Ok(())
        }
        _ => Err("usage: laysi-updater install|run|boot|discard-requests|stop-stack|self-check|write-units|status|version".into()),
    }
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
    Ok(())
}
