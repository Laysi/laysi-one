//! Where everything lives on the host.
//!
//! ```text
//! <root>/                   0700  the stack user
//!   bin/laysi-updater             the current updater
//!   bin/laysi-updater.previous    the one before it, run when the current one
//!                                 fails at boot
//!   stack/                        compose files and .env the stack runs with,
//!                                 each with a `.previous` copy beside it
//!   state/installed               the release this host runs
//!   state/journal                 the update in progress, if any
//!   state/lock                    held by every run
//!   state/updater.env             tool paths, read by the systemd units
//!   state/updater.log
//!   backup/                       database dumps taken before a switch, 0600
//!   incoming/                     files taken from the release being installed
//!   requests/                     mounted into api read-write (`:U`)
//!   results/                      mounted into api and mcp read-only
//! ```
//!
//! Only the last two are mounted into a container. The journal, the backups,
//! `.env` and the compose files are not reachable from any container.

use std::path::{Path, PathBuf};

/// The compose project. Container labels and volume names derive from it.
pub const PROJECT: &str = "laysi-one";

/// Where the api image carries what the updater takes out of it.
pub const IMAGE_SHARE: &str = "/usr/local/share/laysi-one";
pub const IMAGE_UPDATER: &str = "/usr/local/share/laysi-one/laysi-updater";

/// The two compose files, in the order they are passed to podman-compose.
pub const COMPOSE_FILES: [&str; 2] = ["docker-compose.prod.yml", "docker-compose.ship.yml"];
pub const ENV_FILE: &str = ".env";
pub const PREVIOUS: &str = ".previous";

pub const UPDATER: &str = "laysi-updater";

#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Layout {
        Layout { root: root.into() }
    }

    /// `LAYSI_UPDATER_ROOT`, or `~/.local/share/laysi-one`.
    pub fn from_env() -> Result<Layout, String> {
        if let Some(root) = std::env::var_os("LAYSI_UPDATER_ROOT") {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err("LAYSI_UPDATER_ROOT must be an absolute path".into());
            }
            return Ok(Layout::new(root));
        }
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        Ok(Layout::new(
            PathBuf::from(home).join(".local/share/laysi-one"),
        ))
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }
    pub fn current_updater(&self) -> PathBuf {
        self.bin_dir().join(UPDATER)
    }
    pub fn previous_updater(&self) -> PathBuf {
        self.bin_dir().join(format!("{UPDATER}{PREVIOUS}"))
    }

    pub fn stack_dir(&self) -> PathBuf {
        self.root.join("stack")
    }
    /// The files the stack runs with: the two compose files, then `.env`.
    pub fn stack_files(&self) -> [PathBuf; 3] {
        let dir = self.stack_dir();
        [
            dir.join(COMPOSE_FILES[0]),
            dir.join(COMPOSE_FILES[1]),
            dir.join(ENV_FILE),
        ]
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn installed(&self) -> PathBuf {
        self.state_dir().join("installed")
    }
    pub fn journal(&self) -> PathBuf {
        self.state_dir().join("journal")
    }
    pub fn lock(&self) -> PathBuf {
        self.state_dir().join("lock")
    }
    pub fn tool_env(&self) -> PathBuf {
        self.state_dir().join("updater.env")
    }
    pub fn log(&self) -> PathBuf {
        self.state_dir().join("updater.log")
    }

    pub fn backup_dir(&self) -> PathBuf {
        self.root.join("backup")
    }
    pub fn incoming_dir(&self) -> PathBuf {
        self.root.join("incoming")
    }
    /// The files of the release being installed, in [`Layout::stack_files`]
    /// order. Its updater is [`Layout::incoming_updater`].
    pub fn incoming_files(&self) -> [PathBuf; 3] {
        let dir = self.incoming_dir();
        [
            dir.join(COMPOSE_FILES[0]),
            dir.join(COMPOSE_FILES[1]),
            dir.join(ENV_FILE),
        ]
    }
    pub fn incoming_updater(&self) -> PathBuf {
        self.incoming_dir().join(UPDATER)
    }

    pub fn requests_dir(&self) -> PathBuf {
        self.root.join("requests")
    }
    pub fn results_dir(&self) -> PathBuf {
        self.root.join("results")
    }

    /// The copy of a stack file that the last switch replaced.
    pub fn previous_of(path: &Path) -> PathBuf {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(PREVIOUS);
        path.with_file_name(name)
    }

    /// Every directory the updater creates, with its mode.
    pub fn directories(&self) -> [(PathBuf, u32); 8] {
        [
            (self.root.clone(), 0o700),
            (self.bin_dir(), 0o700),
            (self.stack_dir(), 0o700),
            (self.state_dir(), 0o700),
            (self.backup_dir(), 0o700),
            (self.incoming_dir(), 0o700),
            // Read by the api, which runs as another uid: listing and reading
            // are what it needs, and nothing in it is secret.
            (self.results_dir(), 0o755),
            // Chowned to the api's uid by `:U` when the container starts. The
            // stack user keeps read access, which inotify and reading the
            // request need.
            (self.requests_dir(), 0o755),
        ]
    }
}
