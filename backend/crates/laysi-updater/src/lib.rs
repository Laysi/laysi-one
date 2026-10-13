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
pub mod journal;
pub mod layout;
pub mod machine;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod machine_tests;
