//! What a licence token carries, defined once for both sides of it.
//!
//! The licence server signs this payload and an installation verifies it, so
//! the two have to agree on every field. They used to hold a copy each, in
//! separate repositories, where a rename on one side surfaced as a token the
//! other could not parse — at a customer's site, with nothing on screen to
//! say why. One definition makes that a compile error instead.
//!
//! Only the shape lives here. Signing stays on the licence server, which is
//! the side that holds the private key, and verifying stays in the product;
//! each is a dozen lines over `ed25519-dalek` and neither needs the other's
//! error handling.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// What the installation is told, and what it can prove without us.
///
/// Extra fields are ignored rather than fatal on the reading side: the
/// signature covers the encoded bytes, so a newer server can add one without
/// stopping an older build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicensePayload {
    pub license_no: String,
    /// The installation this token was minted for. Carried inside the
    /// signature so that copying the token, or the whole database it sits in,
    /// onto a second machine produces a token that machine can see is not
    /// its own.
    pub fingerprint: String,
    pub install_id: String,
    pub licensee: String,
    #[serde(default)]
    pub licensee_tax_id: String,
    /// Display name of the partner that sold the licence. The screen names
    /// them because a customer with a problem should be able to read who to
    /// call off the same page that reports the problem.
    pub partner: String,
    #[serde(default)]
    pub partner_no: String,
    pub issued_at: DateTime<Utc>,
    /// The date this token stops being believed. One date decides the whole
    /// state machine on the installation side; `expires_on` below is shown to
    /// a person and decides nothing.
    pub not_after: NaiveDate,
    /// End of the licence year the partner sold.
    pub expires_on: NaiveDate,
}
