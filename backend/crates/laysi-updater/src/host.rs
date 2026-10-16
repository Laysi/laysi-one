//! Everything the updater does to the host other than its own files: podman,
//! podman-compose, the database inside the postgres container, disk space,
//! systemd and the clock.
//!
//! The state machine sees only this trait, so the tests drive it with a fake
//! host that can lose power at any call.

use std::fs::File;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use laysi_license_token::update::ToolVersions;

pub type HostResult<T> = Result<T, String>;

/// The stack files one podman-compose call reads: the two compose files and
/// the env file.
pub type StackFiles = [PathBuf; 3];

/// A container of the compose project, as podman reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    /// The compose service it was created for.
    pub service: String,
    pub running: bool,
    /// Set once a container has exited.
    pub exit_code: Option<i64>,
    /// How often its restart policy has started it again since it was
    /// created.
    pub restarts: u32,
    pub has_healthcheck: bool,
    /// `healthy`, `unhealthy` or `starting` when it has a healthcheck.
    pub health: Option<String>,
    /// The reference it was created from, e.g. `ghcr.io/laysi/laysi-one-api@sha256:…`.
    pub image_name: String,
    /// The digest of the image it runs. For an image pulled by its index
    /// digest this is the index digest.
    pub image_digest: String,
    pub created: DateTime<Utc>,
}

/// A service as the merged compose files define it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    /// The image reference after interpolation, e.g.
    /// `docker.io/rustfs/rustfs:1.0.1@sha256:…`.
    pub image: String,
}

/// An image in local storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalImage {
    pub id: String,
    /// `repo@sha256:…`, including the digest it was pulled by.
    pub repo_digests: Vec<String>,
    /// `repo:tag`.
    pub names: Vec<String>,
    pub digest: String,
    pub size: u64,
}

pub trait Host {
    fn now(&mut self) -> DateTime<Utc>;
    fn sleep(&mut self, seconds: u64);
    /// A point at which the fake host may cut the power. The real host does
    /// nothing here.
    fn checkpoint(&mut self, label: &str);
    /// The podman and podman-compose this run calls, as they report
    /// themselves; `unknown` for one that does not answer. Recorded in the
    /// results, never checked.
    fn tool_versions(&mut self) -> ToolVersions;

    fn pull(&mut self, reference: &str) -> HostResult<()>;
    /// Copy files out of an image without running it: `(path in the image,
    /// destination on the host)`.
    fn extract(&mut self, image: &str, files: &[(&str, &Path)]) -> HostResult<()>;

    fn compose_pull(&mut self, files: &StackFiles) -> HostResult<()>;
    /// Every service the compose files define, with its image.
    fn compose_config(&mut self, files: &StackFiles) -> HostResult<Vec<Service>>;
    /// `up -d --no-deps`, with `--force-recreate` when `recreate`.
    fn compose_up(
        &mut self,
        files: &StackFiles,
        services: &[String],
        recreate: bool,
    ) -> HostResult<()>;

    /// Every container of the project, running or not.
    fn containers(&mut self) -> HostResult<Vec<Container>>;
    fn stop(&mut self, names: &[String]) -> HostResult<()>;
    /// Remove stopped containers. Their volumes stay.
    fn remove_containers(&mut self, names: &[String]) -> HostResult<()>;
    /// Wait for a container to exit and return its exit code.
    fn wait_exit(&mut self, name: &str) -> HostResult<i64>;

    fn database_size(&mut self, postgres: &str) -> HostResult<u64>;
    /// `pg_dump -Fc` of the application database into `to`.
    fn dump_database(&mut self, postgres: &str, to: &mut File) -> HostResult<()>;
    /// `pg_restore --list` of a dump: it is a complete archive.
    fn check_dump(&mut self, postgres: &str, dump: &mut File) -> HostResult<()>;
    /// `DROP DATABASE … WITH (FORCE)`, then `pg_restore --create`.
    fn restore_database(&mut self, postgres: &str, from: &mut File) -> HostResult<()>;
    fn drop_database(&mut self, postgres: &str) -> HostResult<()>;

    fn free_bytes(&mut self, path: &Path) -> HostResult<u64>;
    fn same_device(&mut self, a: &Path, b: &Path) -> HostResult<bool>;
    /// Where podman keeps images.
    fn image_store(&mut self) -> HostResult<PathBuf>;
    fn images(&mut self) -> HostResult<Vec<LocalImage>>;
    fn remove_image(&mut self, id: &str) -> HostResult<()>;

    /// Run `<binary> self-check` against this host's state.
    fn self_check(&mut self, binary: &Path) -> HostResult<()>;
    /// Have `binary` write the systemd units, then reload the user manager.
    fn install_units(&mut self, binary: &Path) -> HostResult<()>;
    /// Give the requests directory back to the stack user with
    /// [`REQUESTS_MODE`](crate::layout::REQUESTS_MODE), whatever the api did
    /// to it.
    fn reclaim_requests(&mut self, dir: &Path) -> HostResult<()>;
    /// The same, then empty it of the entries the api created, which belong
    /// to the api's uid.
    fn clear_requests(&mut self, dir: &Path) -> HostResult<()>;
}
