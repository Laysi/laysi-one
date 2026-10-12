//! The `.env` the stack runs with.
//!
//! The updater owns the keys below and leaves every other line as it was.
//! The shipped overlay refers to the digests with `${…:?}`, so a stack whose
//! `.env` lacks one refuses to start rather than running some other image.

use laysi_license_token::release::Image;

use crate::journal::Release;

pub const REGISTRY: &str = "LAYSI_REGISTRY";
pub const HOST_DIR: &str = "LAYSI_HOST_DIR";
pub const RELEASE: &str = "LAYSI_RELEASE";
pub const DEFAULT_REGISTRY: &str = "ghcr.io/laysi";

pub fn digest_key(image: Image) -> &'static str {
    match image {
        Image::Api => "LAYSI_API_DIGEST",
        Image::Web => "LAYSI_WEB_DIGEST",
        Image::Backup => "LAYSI_BACKUP_DIGEST",
    }
}

/// The repository an image of this release is pulled from.
pub fn repository(registry: &str, image: Image) -> String {
    format!("{registry}/laysi-one-{}", image.name())
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvFile {
    lines: Vec<String>,
}

impl EnvFile {
    pub fn parse(text: &str) -> EnvFile {
        EnvFile {
            lines: text.lines().map(str::to_string).collect(),
        }
    }

    fn key_of(line: &str) -> Option<&str> {
        let line = line.trim_start();
        if line.starts_with('#') {
            return None;
        }
        let (key, _) = line.split_once('=')?;
        Some(key.trim().trim_start_matches("export ").trim())
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().rev().find_map(|line| {
            (EnvFile::key_of(line) == Some(key))
                .then(|| line.split_once('=').map(|(_, v)| v.trim()))
                .flatten()
        })
    }

    /// Replace every line that sets `key` with one, or append it.
    pub fn set(&mut self, key: &str, value: &str) {
        self.lines.retain(|line| EnvFile::key_of(line) != Some(key));
        self.lines.push(format!("{key}={value}"));
    }

    pub fn render(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    pub fn registry(&self) -> String {
        self.get(REGISTRY)
            .filter(|r| !r.is_empty())
            .unwrap_or(DEFAULT_REGISTRY)
            .to_string()
    }

    /// This file with the release's digests and the host directory in it.
    pub fn for_release(&self, release: &Release, host_dir: &str) -> EnvFile {
        let mut next = self.clone();
        let registry = self.registry();
        next.set(REGISTRY, &registry);
        next.set(HOST_DIR, host_dir);
        next.set(RELEASE, &release.version.to_string());
        for image in Image::ALL {
            next.set(digest_key(image), &release.digest(image).to_string());
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use laysi_license_token::release::{ImageDigest, ReleaseVersion};

    #[test]
    fn owned_keys_are_replaced_and_others_kept() {
        let base = EnvFile::parse(
            "# kept\nMCP_PORT=9000\nLAYSI_API_DIGEST=sha256:old\nLAYSI_API_DIGEST=sha256:older\nLAYSI_REGISTRY=localhost:5000/laysi\n",
        );
        let d = |n: u8| {
            ImageDigest::parse(&format!("sha256:{}", format!("{n:02x}").repeat(32))).unwrap()
        };
        let release = Release {
            version: ReleaseVersion::parse("v0.1.14").unwrap(),
            digests: [d(1), d(2), d(3)],
        };
        let next = EnvFile::parse(&base.for_release(&release, "/home/s/laysi").render());
        assert_eq!(next.get("MCP_PORT"), Some("9000"));
        assert_eq!(next.get(REGISTRY), Some("localhost:5000/laysi"));
        assert_eq!(next.get(HOST_DIR), Some("/home/s/laysi"));
        assert_eq!(next.get(RELEASE), Some("v0.1.14"));
        assert_eq!(
            next.get("LAYSI_API_DIGEST"),
            Some(d(1).to_string().as_str())
        );
        assert_eq!(
            next.get("LAYSI_BACKUP_DIGEST"),
            Some(d(3).to_string().as_str())
        );
        assert_eq!(next.render().matches("LAYSI_API_DIGEST=").count(), 1);
        assert!(next.render().starts_with("# kept\n"));
        assert_eq!(EnvFile::default().registry(), DEFAULT_REGISTRY);
    }
}
