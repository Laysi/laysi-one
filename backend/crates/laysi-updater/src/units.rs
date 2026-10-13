//! The systemd user units, written by the updater itself so that a new
//! updater brings its own.
//!
//! - `laysi-updater.path` watches for `requests/request` and starts
//!   `laysi-updater.service`, which runs one update. Its `ExecStopPost=`
//!   empties the requests directory however the run ended: a request left in
//!   place would start the service again the moment it stops, five times in a
//!   second, after which the path unit gives up watching for good.
//! - `laysi-updater-boot.service` is how the stack starts at boot: it finishes
//!   an interrupted update first. Nothing else starts the stack, since the
//!   compose services use `unless-stopped`, which `podman-restart.service`
//!   does not start.
//! - When either fails, `OnFailure=` runs the same thing again, the boot unit
//!   with the previous updater: an updater that cannot boot must not leave a
//!   machine nobody can log in to without its stack.

use std::path::{Path, PathBuf};

use crate::fsx;
use crate::layout::Layout;

pub const PATH_UNIT: &str = "laysi-updater.path";
pub const RUN_UNIT: &str = "laysi-updater.service";
pub const BOOT_UNIT: &str = "laysi-updater-boot.service";
pub const RECOVER_UNIT: &str = "laysi-updater-recover.service";
pub const FALLBACK_UNIT: &str = "laysi-updater-fallback.service";

/// Characters a path may have to be written into a unit unquoted.
pub fn plain_path(path: &Path) -> bool {
    path.to_str().is_some_and(|p| {
        p.starts_with('/')
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    })
}

pub fn unit_dir() -> Result<PathBuf, String> {
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(config).join("systemd/user"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

/// Write the units into the user's unit directory. The caller reloads the
/// user manager.
pub fn write(layout: &Layout) -> Result<(), String> {
    write_to(layout, &unit_dir()?)
}

fn write_to(layout: &Layout, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in render(layout)? {
        fsx::write_atomic(&dir.join(name), text.as_bytes(), 0o644)
            .map_err(|e| format!("writing {name}: {e}"))?;
    }
    Ok(())
}

pub fn render(layout: &Layout) -> Result<Vec<(&'static str, String)>, String> {
    if !plain_path(&layout.root) {
        return Err(format!(
            "{} has characters a systemd unit cannot take unquoted",
            layout.root.display()
        ));
    }
    let root = layout.root.display();
    let current = layout.current_updater();
    let current = current.display();
    let previous = layout.previous_updater();
    let previous = previous.display();
    let tool_env = layout.tool_env();
    let tool_env = tool_env.display();
    let request = layout
        .requests_dir()
        .join(laysi_license_token::update::REQUEST_FILE);
    let request = request.display();
    let environment =
        format!("Environment=LAYSI_UPDATER_ROOT={root}\nEnvironmentFile=-{tool_env}\n");

    Ok(vec![
        (
            PATH_UNIT,
            format!(
                "[Unit]\n\
                 Description=Laysi One update requests\n\
                 After={BOOT_UNIT}\n\
                 \n\
                 [Path]\n\
                 PathExists={request}\n\
                 Unit={RUN_UNIT}\n\
                 \n\
                 [Install]\n\
                 WantedBy=default.target\n"
            ),
        ),
        (
            RUN_UNIT,
            format!(
                "[Unit]\n\
                 Description=Laysi One update\n\
                 # A run that is refused ends in well under a second; the\n\
                 # default limit would stop the path unit after five of them.\n\
                 StartLimitIntervalSec=0\n\
                 OnFailure={RECOVER_UNIT}\n\
                 \n\
                 [Service]\n\
                 Type=oneshot\n\
                 {environment}\
                 ExecStart={current} run\n\
                 ExecStopPost=-{current} discard-requests\n\
                 ExecStopPost=-{previous} discard-requests\n\
                 # Migrations decide how long an update takes; the updater\n\
                 # has its own deadlines.\n\
                 TimeoutStartSec=infinity\n"
            ),
        ),
        (
            BOOT_UNIT,
            format!(
                "[Unit]\n\
                 Description=Laysi One start-up\n\
                 OnFailure={FALLBACK_UNIT}\n\
                 \n\
                 [Service]\n\
                 Type=oneshot\n\
                 RemainAfterExit=yes\n\
                 {environment}\
                 ExecStart={current} boot\n\
                 ExecStop=-{current} stop-stack\n\
                 TimeoutStartSec=infinity\n\
                 # Three groups of podman stop --time 30, postgres last.\n\
                 TimeoutStopSec=180\n\
                 \n\
                 [Install]\n\
                 WantedBy=default.target\n"
            ),
        ),
        (
            RECOVER_UNIT,
            format!(
                "[Unit]\n\
                 Description=Laysi One recovery after an update run failed\n\
                 StartLimitIntervalSec=0\n\
                 OnFailure={FALLBACK_UNIT}\n\
                 \n\
                 [Service]\n\
                 Type=oneshot\n\
                 {environment}\
                 ExecStart={current} boot\n\
                 TimeoutStartSec=infinity\n"
            ),
        ),
        (
            FALLBACK_UNIT,
            format!(
                "[Unit]\n\
                 Description=Laysi One start-up with the previous updater\n\
                 StartLimitIntervalSec=0\n\
                 \n\
                 [Service]\n\
                 Type=oneshot\n\
                 {environment}\
                 ExecStart={previous} boot\n\
                 TimeoutStartSec=infinity\n"
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_name_the_root_and_both_updaters() {
        let layout = Layout::new("/home/stack/.local/share/laysi-one");
        let units = render(&layout).unwrap();
        assert_eq!(units.len(), 5);
        let get = |name: &str| &units.iter().find(|(n, _)| *n == name).unwrap().1;
        assert!(get(PATH_UNIT)
            .contains("PathExists=/home/stack/.local/share/laysi-one/requests/request\n"));
        assert!(get(RUN_UNIT).contains("StartLimitIntervalSec=0\n"));
        assert!(get(RUN_UNIT).contains(
            "ExecStopPost=-/home/stack/.local/share/laysi-one/bin/laysi-updater discard-requests\n"
        ));
        assert!(get(BOOT_UNIT).contains("OnFailure=laysi-updater-fallback.service\n"));
        assert!(get(FALLBACK_UNIT).contains(
            "ExecStart=/home/stack/.local/share/laysi-one/bin/laysi-updater.previous boot\n"
        ));
        for (_, text) in &units {
            assert!(!text.contains("restart"), "nothing restarts on its own");
        }
    }

    #[test]
    fn units_are_written_where_the_user_manager_reads_them() {
        let dir = std::env::temp_dir().join(format!("laysi-updater-units-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = Layout::new("/home/stack/.local/share/laysi-one");
        write_to(&layout, &dir.join("systemd/user")).unwrap();
        for (name, text) in render(&layout).unwrap() {
            let written = std::fs::read_to_string(dir.join("systemd/user").join(name)).unwrap();
            assert_eq!(written, text);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_root_that_needs_quoting_is_refused() {
        assert!(render(&Layout::new("/home/a b/laysi")).is_err());
        assert!(render(&Layout::new("relative/laysi")).is_err());
        assert!(render(&Layout::new("/home/x/$HOME")).is_err());
    }
}
