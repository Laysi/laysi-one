//! Server components shared across binaries.
//!
//! The main server and `laysi-admin` must share one password algorithm and
//! one set of parameters rather than each copying them. Hashing itself lives
//! in `laysi-store` because that crate owns `user_account`, and the
//! `create_user_account` operation needs it too; this re-export keeps the
//! existing `laysi_server::password::*` call sites working.

pub mod demo;
pub use laysi_store::password;
