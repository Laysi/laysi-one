//! Server components shared across binaries.
//!
//! The main server and `laysi-admin` must share one password algorithm and
//! one set of parameters rather than each copying them.

pub mod demo;
pub mod password;
