//! The host-side updater of Laysi One.
//!
//! It runs on the host rather than in a container: a container cannot replace
//! itself, and handing one the podman socket would hand it everything the
//! stack owns. It runs as the rootless user that owns the stack, from systemd
//! user units it writes itself.
//!
//! Linked as a static musl binary with rust-lld alone, so it depends on
//! `laysi-license-token` for the shared types and on nothing that compiles C.

pub mod envfile;
pub mod fsx;
pub mod host;
pub mod install;
pub mod journal;
pub mod layout;
pub mod machine;
pub mod podman;
pub mod units;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod machine_tests;

use laysi_license_token::release::{self, VerifyingKey};

/// Whether this build also trusts the test keys (the `test-keys` feature).
/// The api image never builds with it.
pub const TEST_KEYS: bool = cfg!(feature = "test-keys");

/// The keys a release manifest has to be signed with: the ones compiled into
/// `laysi-license-token`, and in a `test-keys` build also the public keys in
/// `LAYSI_UPDATER_TEST_KEYS` at compile time, which scripts/updater-e2e.ts
/// signs its releases with.
pub fn trusted_keys() -> Vec<VerifyingKey> {
    #[allow(unused_mut)]
    let mut keys = release::trusted_keys();
    #[cfg(feature = "test-keys")]
    keys.extend(
        env!("LAYSI_UPDATER_TEST_KEYS")
            .split_whitespace()
            .filter_map(release::parse_public_key),
    );
    keys
}
