//! Operations, one module per business area.
//!
//! Every module exports `ops()` returning its own operation list. Adding a
//! module means one `pub mod` line here and one line in `all()`; nothing else
//! in the shared files needs to change.

use crate::Operation;

pub mod assets;
pub mod attendance;
pub mod backup;
pub mod banking;
pub mod budget;
pub mod calendar;
pub mod documents;
pub mod employees;
pub mod expenses;
pub mod hr_compliance;
pub mod imports;
pub mod insurance;
pub mod ledger;
pub mod masters;
pub mod notices;
pub mod payables;
pub mod payroll;
pub mod receivables;
pub mod regulations;
pub mod reports;
pub mod security;
pub mod selfservice;
pub mod settings;
pub mod tax;

/// Collect every module's operations. Used by `Registry::new()`.
pub fn all() -> Vec<Box<dyn Operation>> {
    let mut out = Vec::new();
    out.extend(ledger::ops());
    out.extend(receivables::ops());
    out.extend(payables::ops());
    out.extend(banking::ops());
    out.extend(tax::ops());
    out.extend(assets::ops());
    out.extend(expenses::ops());
    out.extend(budget::ops());
    out.extend(documents::ops());
    out.extend(imports::ops());
    out.extend(reports::ops());
    out.extend(masters::ops());
    out.extend(employees::ops());
    out.extend(attendance::ops());
    out.extend(insurance::ops());
    out.extend(payroll::ops());
    out.extend(hr_compliance::ops());
    out.extend(notices::ops());
    out.extend(selfservice::ops());
    out.extend(settings::ops());
    out.extend(regulations::ops());
    out.extend(calendar::ops());
    out.extend(security::ops());
    out.extend(backup::ops());
    out
}
