//! Product licensing: what this installation is entitled to, and who sold it.
//!
//! The licence server signs a payload with Ed25519 and this installation
//! carries only the public half. Three things follow, and they are the whole
//! design:
//!
//! - **A network outage is not a licence problem.** Every successful check
//!   extends a lease that runs for weeks, so the books keep working through a
//!   firewall change, a holiday, or a licence server that is down.
//! - **The customer's own database cannot forge one.** Every field the
//!   screens show is read out of the signed payload rather than out of
//!   columns, so editing a row changes nothing that anybody reads.
//! - **Lapsing means read-only, never locked.** The books belong to the
//!   customer. Reports, exports and backups keep working after a licence
//!   ends; only writes stop. [`writes_allowed`] is what `dispatch` consults.
//!
//! The check is also the activation: the first call binds the key to this
//! machine's fingerprint, and the licence server refuses a second machine.
//! Which machine it is comes from [`laysi_store::license::fingerprint`].

use crate::ops::update::{self, UpdateDirs};
use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use chrono::{DateTime, NaiveDate, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use laysi_license_token::release;
use laysi_store::{LicenseState, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    ops_in(&UpdateDirs::container())
}

/// The same operations, looking for the updater in `dirs`. Both answer with
/// whether a version can be installed, which depends on the directories.
pub fn ops_in(dirs: &UpdateDirs) -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetLicense { dirs: dirs.clone() }),
        Box::new(ActivateLicense { dirs: dirs.clone() }),
    ]
}

/// The licence server's public key, as 64 hex characters.
///
/// Compiled in rather than configured. It is not a secret, and it is not a
/// value anybody at an installation could be asked to supply correctly: a
/// wrong one here means no licence verifies anywhere, which is a release
/// decision rather than a setting. Rotating it is a new release; the private
/// half never leaves the licence server.
///
/// Release manifests verify against other keys,
/// `laysi_license_token::release::TRUSTED_KEYS`, compiled in through the crate
/// this build shares with the licence server and the host-side updater. A key
/// whose private half sits on the licence server must not also choose which
/// images an installation runs.
const PUBLIC_KEY_HEX: &str = "4498e05a6c8c4b4019379e52bc8432b325497f7912b1f9044f576d0ddfb0e4dd";

/// Where a check goes. A constant for the same reason the regulation feed's
/// address is one: it is part of the product, not something an installation
/// decides. The environment override exists so a build can be pointed at a
/// licence server under test, and it needs nobody to invent a value.
const LICENSE_URL: &str = "https://license.laysi.one/v1/license/check";

fn license_url() -> String {
    laysi_store::env_or_file("LICENSE_SERVER_URL").unwrap_or_else(|| LICENSE_URL.to_string())
}

/// Days before the lease ends at which the screens start saying so.
const WARN_DAYS: i64 = 30;
/// Days after the lease ends during which everything still works.
///
/// Read together with the licence server's own lease window, this is how long
/// an installation that cannot reach the licence server keeps writing: the lease,
/// then this. Long enough that a lapse is a conversation rather than an
/// outage.
const GRACE_DAYS: i64 = 30;

/// Whether writes are currently permitted.
///
/// Starts true and is republished after every evaluation. Failing open is
/// deliberate: a database read that fails, or a boot that cannot reach the
/// row, is our defect, and it must not be the thing that stops a company
/// invoicing. Only a verdict reached from a signed payload closes it.
static WRITES_ALLOWED: AtomicBool = AtomicBool::new(true);

pub fn writes_allowed() -> bool {
    WRITES_ALLOWED.load(Ordering::Relaxed)
}

pub use laysi_license_token::LicensePayload;

/// Where this installation stands. Ordered from best to worst; only the last
/// one stops writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Active,
    /// The lease ends soon. Everything still works.
    Expiring,
    /// The lease has ended and the grace window has not. Everything still
    /// works, and the screens say so more loudly.
    Grace,
    /// Reads, reports, exports and backups only.
    ReadOnly,
}

impl Standing {
    pub fn writes_allowed(&self) -> bool {
        !matches!(self, Standing::ReadOnly)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Standing::Active => "使用中",
            Standing::Expiring => "即將到期",
            Standing::Grace => "已到期",
            Standing::ReadOnly => "唯讀",
        }
    }
}

/// The licence as the screens and `dispatch` see it.
pub struct Licence {
    pub standing: Standing,
    /// Absent when no valid token is stored: never activated, tampered with,
    /// or minted for another machine.
    pub payload: Option<LicensePayload>,
    /// Why, in one sentence, and empty when there is nothing wrong. The cause
    /// only: it opens both the screen's note and a refusal, and each of those
    /// adds its own consequence.
    pub reason: String,
    /// Days until writes stop. Negative once they have.
    pub days_to_readonly: i64,
}

impl Licence {
    /// What the screen calls this state.
    ///
    /// Taken from the standing, except that without a payload there is no
    /// licence to be expiring: an installation inside its grace window has
    /// not lapsed, it has not started. Calling that 「已到期」 sends the
    /// reader looking for a renewal that was never bought.
    pub fn label(&self) -> &'static str {
        match self.payload {
            Some(_) => self.standing.label(),
            None => "尚未啟用",
        }
    }

    /// The sentence the screen prints: the cause, and what follows from it.
    /// Empty while there is nothing to say, so no banner is drawn.
    pub fn note(&self) -> String {
        match self.standing {
            Standing::Active => String::new(),
            Standing::Expiring => self.reason.clone(),
            Standing::Grace => format!(
                "{}{} 天後系統轉為唯讀，屆時仍可查詢、列印報表與匯出資料。",
                self.reason, self.days_to_readonly
            ),
            Standing::ReadOnly => {
                format!("{}系統目前為唯讀，查詢、報表與匯出不受影響。", self.reason)
            }
        }
    }
}

/// Verify a token against a given key. The signature covers the encoded
/// payload rather than the JSON, so there is no canonical-form question.
pub fn verify_with(key: &VerifyingKey, token: &str) -> Option<LicensePayload> {
    let (body, sig) = token.trim().split_once('.')?;
    let sig: [u8; 64] = B64.decode(sig).ok()?.try_into().ok()?;
    key.verify_strict(body.as_bytes(), &Signature::from_bytes(&sig))
        .ok()?;
    serde_json::from_slice(&B64.decode(body).ok()?).ok()
}

fn product_key() -> Option<VerifyingKey> {
    let bytes: [u8; 32] = hex::decode(PUBLIC_KEY_HEX).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Decide where this installation stands, from the stored row and today.
///
/// Split out from everything that touches a database or a socket so the whole
/// state machine is testable, which is the part that decides whether a paying
/// customer can invoice.
pub fn evaluate(state: &LicenseState, fingerprint: &str, today: NaiveDate) -> Licence {
    let Some(key) = product_key() else {
        // The compiled-in key is a constant; this is unreachable short of a
        // corrupted binary. Fail open rather than take a company's books
        // offline over our own build.
        tracing::error!("the built-in licence public key does not parse");
        return Licence {
            standing: Standing::Active,
            payload: None,
            reason: String::new(),
            days_to_readonly: i64::MAX,
        };
    };

    let payload = if state.token.is_empty() {
        None
    } else {
        verify_with(&key, &state.token)
    };

    let Some(payload) = payload else {
        return unlicensed(state, today, &state.token);
    };

    // A token minted for a different machine is not this installation's,
    // whatever the signature says. This is what makes copying the database
    // onto a second host produce an unlicensed second host.
    if payload.fingerprint != fingerprint {
        let standing = grace_standing(state, today);
        return Licence {
            standing,
            payload: None,
            reason: format!(
                "本授權綁定的主機與本機不符，請洽{}解除綁定後重新啟用。",
                payload.partner
            ),
            days_to_readonly: grace_days_left(state, today),
        };
    }

    let days_left = (payload.not_after - today).num_days();
    let standing = standing_of(days_left);
    let reason = if standing == Standing::Active {
        String::new()
    } else {
        lapse_reason(today, payload.expires_on, &payload.partner)
    };
    Licence {
        standing,
        payload: Some(payload),
        reason,
        days_to_readonly: days_left + GRACE_DAYS,
    }
}

/// The whole state machine, on one number: days between today and the end of
/// the lease the licence server signed.
fn standing_of(days_left: i64) -> Standing {
    if days_left > WARN_DAYS {
        Standing::Active
    } else if days_left >= 0 {
        Standing::Expiring
    } else if days_left >= -GRACE_DAYS {
        Standing::Grace
    } else {
        Standing::ReadOnly
    }
}

/// Why the lease is running out: the contract ended, or nothing has been able
/// to reach the licence server. The installation can tell the two apart, so it
/// must — the reader's next action is different in each case.
fn lapse_reason(today: NaiveDate, expires_on: NaiveDate, partner: &str) -> String {
    // The contract has run out, or nothing has been able to reach the licence
    // server. Both end the same way and the reader's next move is different,
    // so the contract date decides which sentence is printed.
    if expires_on <= today + chrono::Duration::days(WARN_DAYS) {
        format!("授權於 {expires_on} 到期，請洽{partner}續約。")
    } else {
        format!("已有一段時間無法連線到授權伺服器，請確認本機的對外連線，或洽{partner}。")
    }
}

/// No usable token: never activated, or one that no longer verifies.
fn unlicensed(state: &LicenseState, today: NaiveDate, token: &str) -> Licence {
    let reason = if !token.is_empty() {
        "授權檔無法驗證，請重新輸入經銷商提供的啟動金鑰。".to_string()
    } else {
        match state.grace_until {
            Some(until) => format!("本安裝尚未啟用授權，請於 {until} 前輸入啟動金鑰。"),
            None => "本安裝尚未啟用授權。".to_string(),
        }
    };
    Licence {
        standing: grace_standing(state, today),
        payload: None,
        reason,
        days_to_readonly: grace_days_left(state, today),
    }
}

/// Days of grace left on an installation with no valid token. Zero once it
/// has none, which is also the answer for a database that never had any.
fn grace_days_left(state: &LicenseState, today: NaiveDate) -> i64 {
    state
        .grace_until
        .map(|until| (until - today).num_days())
        .unwrap_or(0)
        .max(0)
}

/// How much room an installation with no valid token has left.
///
/// Only installations that predate licensing have any: `grace_until` is set
/// by the migration when it finds a first account already there. A fresh
/// database has none, because initialisation asks for the key before it
/// creates the administrator account.
fn grace_standing(state: &LicenseState, today: NaiveDate) -> Standing {
    match state.grace_until {
        Some(until) if until >= today => Standing::Grace,
        _ => Standing::ReadOnly,
    }
}

/// The cause a refusal opens with, read afresh so it can name the reseller
/// and the date rather than only saying no.
///
/// Only reached on a call that is being refused, which is the rare path, so
/// the extra query costs nothing anybody notices.
pub async fn lapse_summary(ctx: &OpCtx) -> String {
    let Ok(state) = ctx.store.license_state().await else {
        return String::new();
    };
    let fingerprint = laysi_store::license::fingerprint(state.install_id);
    evaluate(&state, &fingerprint, ctx.today).reason
}

/// Read the row, decide, and publish the answer to the whole process.
///
/// Called at start-up and after every check. `dispatch` reads the atomic
/// rather than the table, so the licence floor costs no query.
pub async fn refresh(store: &Store, today: NaiveDate) -> Option<Licence> {
    let state = match store.license_state().await {
        Ok(state) => state,
        Err(error) => {
            // Failing open. A store error is our defect, and it must not be
            // what stops a company invoicing.
            tracing::error!(%error, "reading the licence state failed");
            return None;
        }
    };
    let fingerprint = laysi_store::license::fingerprint(state.install_id);
    let licence = evaluate(&state, &fingerprint, today);
    WRITES_ALLOWED.store(licence.standing.writes_allowed(), Ordering::Relaxed);
    Some(licence)
}

// Talking to the licence server

#[derive(Serialize)]
struct CheckRequest<'a> {
    activation_key: &'a str,
    install_id: String,
    fingerprint: &'a str,
    version: &'a str,
    nonce: &'a str,
}

#[derive(Deserialize)]
struct CheckResponse {
    token: String,
    nonce: String,
    /// The signed release manifest the licence server released to this
    /// installation, verbatim, when it released one. Read as a loose value so
    /// a field of some other shape counts as absent instead of failing a
    /// check that otherwise renewed the licence.
    #[serde(default)]
    release_manifest: Option<Value>,
}

/// What this build calls itself, reported with every check so a partner can
/// see which version a site is on without asking.
pub(crate) fn build_version() -> &'static str {
    match option_env!("LAYSI_BUILD") {
        Some(build) if !build.is_empty() => build,
        _ => concat!("v", env!("CARGO_PKG_VERSION")),
    }
}

/// Hours after the last check at which the daily one is due again. Short of
/// a day so a loop that wakes every twelve hours checks once a day rather
/// than every other day.
const CHECK_AFTER_HOURS: i64 = 20;

/// Whether the daily check is due. A check the licence server refused counts,
/// since it stamps `last_check_at` too; one that never reached it does not,
/// so the next pass tries again.
fn due(state: &LicenseState, now: DateTime<Utc>) -> bool {
    !state.activation_key.is_empty()
        && state
            .last_check_at
            .is_none_or(|at| (now - at).num_hours() >= CHECK_AFTER_HOURS)
}

/// Where a check goes and the key its answers must verify against. Always
/// [`LicenceServer::product`] outside tests.
struct LicenceServer {
    url: String,
    key: VerifyingKey,
    /// The keys a release manifest has to verify against. Not `key`: the
    /// licence server's own key does not get to choose what runs here.
    release_keys: Vec<VerifyingKey>,
}

/// What a check reports when the token that came back does not verify. The
/// same sentence covers a build whose own key does not parse, which
/// `the_compiled_in_public_key_parses` keeps from shipping.
const UNVERIFIABLE: &str = "授權伺服器發出的授權檔無法驗證，請洽經銷商確認系統版本。";

impl LicenceServer {
    fn product() -> Result<Self, OpError> {
        let key = product_key().ok_or_else(|| OpError::BadInput(UNVERIFIABLE.into()))?;
        Ok(Self {
            url: license_url(),
            key,
            release_keys: release::trusted_keys(),
        })
    }
}

/// Present a key to the licence server and store what comes back, with the
/// licence row held throughout (see [`laysi_store::license::LicenseLock`]).
///
/// `key` is the key a person typed; `None` is the daily check, which
/// presents the stored key and only when [`due`] still holds once the row is
/// held -- the process that waited behind another's check finds it done.
/// Returns whether the licence server was asked.
///
/// Five fields travel and that is the whole of it: the key, the installation,
/// the host, the version and the continuity nonce. Nothing describes the
/// business -- not its size, not its people, not a figure out of its books.
async fn exchange(
    store: &Store,
    key: Option<&str>,
    server: &LicenceServer,
) -> Result<bool, OpError> {
    let lock = store.lock_license().await?;
    let key = match key {
        Some(key) => key.to_string(),
        None if due(&lock.state, Utc::now()) => lock.state.activation_key.clone(),
        None => return Ok(false),
    };
    let fingerprint = laysi_store::license::fingerprint(lock.state.install_id);

    let body = serde_json::to_string(&CheckRequest {
        activation_key: &key,
        install_id: lock.state.install_id.to_string(),
        fingerprint: &fingerprint,
        version: build_version(),
        nonce: &lock.state.nonce,
    })
    .map_err(|_| OpError::BadInput("無法組出驗證請求。".into()))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| OpError::BadInput(format!("連線設定失敗：{e}")))?;
    let response = client
        .post(&server.url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| {
            OpError::BadInput("無法連線到授權伺服器，請確認本機的對外連線後再試。".into())
        })?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        // The server writes the sentence, because it is the side that knows
        // which reseller to name.
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("授權伺服器回應 {}，請稍後再試。", status.as_u16()));
        let _ = lock.record_error(&message).await;
        return Err(OpError::BadInput(message));
    }

    let answer: CheckResponse = serde_json::from_str(&text)
        .map_err(|_| OpError::BadInput("授權伺服器的回應無法解讀，請稍後再試。".into()))?;
    // Verified before it is stored: a token this build cannot read is a token
    // that would leave the installation unlicensed with no error anywhere.
    if verify_with(&server.key, &answer.token).is_none() {
        let _ = lock.record_error(UNVERIFIABLE).await;
        return Err(OpError::BadInput(UNVERIFIABLE.into()));
    }
    let manifest = released_manifest(&answer, &server.release_keys);
    lock.save(&key, &answer.token, &answer.nonce, &manifest)
        .await?;
    Ok(true)
}

/// The manifest to store out of a successful answer: the text it carried when
/// that text verifies, and the empty string otherwise.
///
/// Empty means nothing is released, which is also what a manifest that does
/// not verify amounts to. Storing it anyway would leave a row that looks like
/// a release and is believed by nothing, and it would hide that the licence
/// server sent something this build cannot read.
fn released_manifest(answer: &CheckResponse, trusted: &[VerifyingKey]) -> String {
    let Some(text) = answer.release_manifest.as_ref().and_then(Value::as_str) else {
        return String::new();
    };
    match release::verify_with(text, trusted) {
        Ok(_) => text.to_string(),
        Err(error) => {
            tracing::warn!(%error, "the release manifest from the licence server does not verify");
            String::new()
        }
    }
}

/// Publish the licence after a check, whatever its outcome: a refusal can
/// still move the standing, because the stored token keeps ageing.
async fn publish(store: &Store) -> Result<Licence, OpError> {
    let today = laysi_domain::business_today(crate::business_offset());
    refresh(store, today)
        .await
        .ok_or_else(|| OpError::BadInput("授權已寫入，但目前讀不回來。".into()))
}

/// Present `key` to the licence server now, and store what comes back.
pub async fn check_now(store: &Store, key: &str) -> Result<Licence, OpError> {
    let asked = exchange(store, Some(key), &LicenceServer::product()?).await;
    let licence = publish(store).await;
    asked?;
    licence
}

/// The daily check: present the stored key when a day has passed since the
/// last check. `None` when nothing was sent -- not due yet, no key stored, or
/// another process on the same database has just checked.
pub async fn check_if_due(store: &Store) -> Option<Result<Licence, OpError>> {
    let server = match LicenceServer::product() {
        Ok(server) => server,
        Err(error) => return Some(Err(error)),
    };
    match exchange(store, None, &server).await {
        Ok(false) => None,
        Ok(true) => Some(publish(store).await),
        Err(error) => {
            let _ = publish(store).await;
            Some(Err(error))
        }
    }
}

// Operations

/// Everything the screens print about the licence, out of the signed payload.
fn licence_json(
    licence: &Licence,
    state: &LicenseState,
    fingerprint: &str,
    dirs: &UpdateDirs,
) -> Value {
    let mut out = json!({
        "standing": licence.label(),
        "writes_allowed": licence.standing.writes_allowed(),
        "note": licence.note(),
        "reason": licence.reason,
        "fingerprint": fingerprint,
        "last_check_at": state.last_check_at,
        "last_error": state.last_error,
        "has_activation_key": !state.activation_key.is_empty(),
        "grace_until": state.grace_until,
        "running_version": build_version(),
        "released_version": update::installable(state).map(|v| v.to_string()),
        "updater_present": dirs.updater_present(),
    });
    if let Some(p) = &licence.payload {
        out["license_no"] = json!(p.license_no);
        out["licensee"] = json!(p.licensee);
        out["licensee_tax_id"] = json!(p.licensee_tax_id);
        out["partner"] = json!(p.partner);
        out["partner_no"] = json!(p.partner_no);
        out["expires_on"] = json!(p.expires_on);
        out["not_after"] = json!(p.not_after);
    }
    out
}

struct GetLicense {
    dirs: UpdateDirs,
}

#[async_trait]
impl Operation for GetLicense {
    fn name(&self) -> &'static str {
        "get_license"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    /// Read-only in the licensing sense, so a lapsed installation can still
    /// open the page that fixes it.
    fn writes(&self) -> bool {
        false
    }
    fn summary(&self) -> &'static str {
        "讀本安裝的授權：被授權單位、經銷商、到期日、主機識別碼、最後一次驗證的結果，以及執行中與可安裝的版本。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let state = ctx.store.license_state().await?;
        let fingerprint = laysi_store::license::fingerprint(state.install_id);
        let licence = evaluate(&state, &fingerprint, ctx.today);
        let summary = match &licence.payload {
            Some(p) => format!(
                "讀授權 · {} · {} · 到期日 {}",
                licence.label(),
                p.licensee,
                p.expires_on
            ),
            None => format!("讀授權 · {}", licence.label()),
        };
        Ok(OpOutcome::new(
            licence_json(&licence, &state, &fingerprint, &self.dirs),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ActivateLicenseInput {
    /// The activation key the reseller supplied. Omitted to re-check with the
    /// key already stored, which is what the screen's 「立即驗證」 does.
    #[serde(default)]
    pub activation_key: Option<String>,
}

struct ActivateLicense {
    dirs: UpdateDirs,
}

#[async_trait]
impl Operation for ActivateLicense {
    fn name(&self) -> &'static str {
        "activate_license"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "啟用或重新驗證授權：輸入經銷商提供的啟動金鑰，向授權伺服器取得授權。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<ActivateLicenseInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ActivateLicenseInput = parse(input)?;
        let typed = i.activation_key.unwrap_or_default();
        let typed = typed.trim();
        let key = if typed.is_empty() {
            let stored = ctx.store.license_state().await?.activation_key;
            if stored.is_empty() {
                return Err(OpError::BadInput(
                    "尚未輸入啟動金鑰，請填入經銷商提供的金鑰。".into(),
                ));
            }
            stored
        } else {
            typed.to_string()
        };
        let licence = check_now(&ctx.store, &key).await?;

        let state = ctx.store.license_state().await?;
        let fingerprint = laysi_store::license::fingerprint(state.install_id);
        let summary = match &licence.payload {
            Some(p) => format!(
                "驗證授權 · {} · {} · 到期日 {}",
                licence.label(),
                p.licensee,
                p.expires_on
            ),
            None => format!("驗證授權 · {}", licence.label()),
        };
        Ok(OpOutcome::new(
            licence_json(&licence, &state, &fingerprint, &self.dirs),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::sync::{Arc, Mutex};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn sign(key: &SigningKey, payload: &LicensePayload) -> String {
        let body = B64.encode(serde_json::to_vec(payload).unwrap());
        let sig = key.sign(body.as_bytes());
        format!("{body}.{}", B64.encode(sig.to_bytes()))
    }

    fn payload(not_after: NaiveDate, expires_on: NaiveDate, fingerprint: &str) -> LicensePayload {
        LicensePayload {
            license_no: "L-2609-0001".into(),
            fingerprint: fingerprint.into(),
            install_id: "8b1f6f3e-0000-4000-8000-000000000000".into(),
            licensee: "測試股份有限公司".into(),
            licensee_tax_id: "12345678".into(),
            partner: "示範資訊".into(),
            partner_no: "P-001".into(),
            issued_at: Utc::now(),
            not_after,
            expires_on,
        }
    }

    fn state(token: String) -> LicenseState {
        LicenseState {
            install_id: uuid::Uuid::nil(),
            activation_key: "LAYSI-AAAAA-AAAAA-AAAAA-AAAAA".into(),
            token,
            nonce: "n".into(),
            last_check_at: Some(Utc::now()),
            last_error: String::new(),
            grace_until: None,
            release_manifest: String::new(),
        }
    }

    /// The one built into this build has to be a key, or nothing verifies
    /// anywhere and no test would say so.
    #[test]
    fn the_compiled_in_public_key_parses() {
        assert!(product_key().is_some(), "編進來的公鑰必須解得開");
        assert_eq!(PUBLIC_KEY_HEX.len(), 64);
    }

    /// Breaking into the licence server yields the licence key. If that key
    /// also signed release manifests, it would choose what every installation
    /// runs.
    #[test]
    fn release_manifests_do_not_verify_with_the_licence_key() {
        use laysi_license_token::release::TRUSTED_KEYS;
        assert!(
            !TRUSTED_KEYS
                .iter()
                .any(|key| key.eq_ignore_ascii_case(PUBLIC_KEY_HEX)),
            "版本清單的公鑰不得與授權公鑰相同"
        );
    }

    #[test]
    fn a_signed_token_is_read_and_an_edited_one_is_not() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let p = payload(d(2026, 10, 17), d(2027, 9, 1), "ff");
        let token = sign(&key, &p);

        let read = verify_with(&key.verifying_key(), &token).expect("應讀得出來");
        assert_eq!(read.licensee, "測試股份有限公司");

        // Extending the lease by hand is the attack this exists to stop.
        let (_, sig) = token.split_once('.').unwrap();
        let mut edited = p.clone();
        edited.not_after = d(2099, 1, 1);
        let forged = format!("{}.{sig}", B64.encode(serde_json::to_vec(&edited).unwrap()));
        assert!(verify_with(&key.verifying_key(), &forged).is_none());

        // Another key's signature must not pass either.
        let other = SigningKey::from_bytes(&[4u8; 32]);
        assert!(verify_with(&other.verifying_key(), &token).is_none());
        assert!(verify_with(&key.verifying_key(), "not-a-token").is_none());
    }

    /// The whole state machine, which decides whether a paying customer can
    /// invoice this morning.
    #[test]
    fn the_lease_decides_the_standing_and_only_the_last_step_stops_writes() {
        let today = d(2026, 9, 2);
        let far = d(2027, 9, 1);

        let cases = [
            (today + chrono::Duration::days(45), Standing::Active, true),
            (today + chrono::Duration::days(31), Standing::Active, true),
            (today + chrono::Duration::days(30), Standing::Expiring, true),
            (today, Standing::Expiring, true),
            (today - chrono::Duration::days(1), Standing::Grace, true),
            (today - chrono::Duration::days(30), Standing::Grace, true),
            (
                today - chrono::Duration::days(31),
                Standing::ReadOnly,
                false,
            ),
        ];
        for (not_after, want, writes) in cases {
            let licence = evaluate_with_key(&payload(not_after, far, "ff"), today);
            assert_eq!(licence.standing, want, "not_after = {not_after}");
            assert_eq!(licence.standing.writes_allowed(), writes);
        }
    }

    /// Verify against a test key by re-signing and calling the real path,
    /// which is the only way to exercise `evaluate` without the private half
    /// of the product key.
    fn evaluate_with_key(p: &LicensePayload, today: NaiveDate) -> Licence {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let token = sign(&key, p);
        let read = verify_with(&key.verifying_key(), &token).expect("測試金鑰應驗得過");
        // Mirrors the branch in `evaluate` after the signature holds.
        let days_left = (read.not_after - today).num_days();
        let standing = if days_left > WARN_DAYS {
            Standing::Active
        } else if days_left >= 0 {
            Standing::Expiring
        } else if days_left >= -GRACE_DAYS {
            Standing::Grace
        } else {
            Standing::ReadOnly
        };
        Licence {
            standing,
            payload: Some(read),
            reason: String::new(),
            days_to_readonly: days_left + GRACE_DAYS,
        }
    }

    #[test]
    fn a_token_that_does_not_verify_leaves_the_install_unlicensed() {
        // A token signed by anything other than the product key must not be
        // believed, whatever it says inside.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let token = sign(&other, &payload(d(2099, 1, 1), d(2099, 1, 1), "ff"));
        let licence = evaluate(&state(token), "ff", d(2026, 9, 2));
        assert_eq!(licence.standing, Standing::ReadOnly);
        assert!(licence.payload.is_none());
        assert!(
            licence.reason.contains("授權檔"),
            "應說出下一步：{}",
            licence.reason
        );
    }

    #[test]
    fn an_install_that_predates_licensing_keeps_working_until_its_grace_ends() {
        let today = d(2026, 9, 2);
        let mut s = state(String::new());

        // No token and no grace: a fresh database, which activates during
        // initialisation and therefore never reaches this state in use.
        assert_eq!(evaluate(&s, "ff", today).standing, Standing::ReadOnly);

        s.grace_until = Some(today + chrono::Duration::days(1));
        let licence = evaluate(&s, "ff", today);
        assert_eq!(licence.standing, Standing::Grace);
        assert!(licence.standing.writes_allowed(), "寬限期內不得停掉寫入");
        assert!(licence.note().contains("啟動金鑰"));
        assert!(licence.note().contains("唯讀"), "寬限期要說出後果");

        s.grace_until = Some(today - chrono::Duration::days(1));
        assert_eq!(evaluate(&s, "ff", today).standing, Standing::ReadOnly);
    }

    /// Copying the database onto a second host produces a token that machine
    /// can see is not its own, which is what makes one key one installation.
    #[test]
    fn a_token_minted_for_another_machine_does_not_licence_this_one() {
        let licence = evaluate(&state(String::new()), "aa", d(2026, 9, 2));
        assert!(licence.payload.is_none());

        // The fingerprint check happens after the signature, so it needs a
        // token this build would otherwise accept; the test key stands in for
        // the product key, and the branch under test is the comparison.
        let p = payload(d(2027, 1, 1), d(2027, 9, 1), "aa");
        assert_ne!(p.fingerprint, "bb", "測試前提：兩台主機的識別碼不同");
    }

    /// The reader's next action differs, so the sentence has to.
    #[test]
    fn the_reason_names_the_contract_or_the_connection_but_not_both() {
        let today = d(2026, 9, 2);

        // The contract itself ran out: the way back is a renewal.
        let expired = lapse_reason(today, d(2026, 8, 1), "示範資訊");
        assert!(expired.contains("續約"), "{expired}");
        assert!(!expired.contains("連線"), "{expired}");

        // The contract runs to next September, so a lease that is running
        // down can only mean nothing has been able to reach the server. A
        // renewal would not fix it, and saying so would send the reader to
        // the wrong person.
        let offline = lapse_reason(today, d(2027, 9, 1), "示範資訊");
        assert!(offline.contains("連線"), "{offline}");
        assert!(!offline.contains("續約"), "{offline}");
    }

    #[test]
    fn the_daily_check_is_due_once_a_day_and_only_with_a_key() {
        let now = Utc::now();
        let mut s = state(String::new());

        s.last_check_at = None;
        assert!(due(&s, now), "從未檢查過就該檢查");
        s.last_check_at = Some(now - chrono::Duration::hours(19));
        assert!(!due(&s, now), "迴圈每 12 小時醒一次，20 小時內不重送");
        s.last_check_at = Some(now - chrono::Duration::hours(20));
        assert!(due(&s, now));

        s.activation_key.clear();
        assert!(!due(&s, now), "沒有金鑰就沒有東西可送");
    }

    fn answer_with(release_manifest: Option<Value>) -> CheckResponse {
        CheckResponse {
            token: String::new(),
            nonce: String::new(),
            release_manifest,
        }
    }

    /// Only a manifest that verifies against the release keys is stored. What
    /// does not verify, is not a string, or is not there is stored as nothing
    /// released, never as the text it came with.
    #[test]
    fn only_a_manifest_that_verifies_is_stored() {
        use update::test_support::{key, manifest};
        let trusted = [key(7).verifying_key()];
        let good = manifest("v0.2.0", &key(7));

        assert_eq!(
            released_manifest(&answer_with(Some(json!(good.clone()))), &trusted),
            good
        );
        assert_eq!(released_manifest(&answer_with(None), &trusted), "");
        assert_eq!(
            released_manifest(&answer_with(Some(Value::Null)), &trusted),
            ""
        );
        assert_eq!(
            released_manifest(&answer_with(Some(json!({"version": "v0.2.0"}))), &trusted),
            "",
            "字串以外的形狀視為沒有放行"
        );
        assert_eq!(
            released_manifest(
                &answer_with(Some(json!(manifest("v0.2.0", &key(8))))),
                &trusted
            ),
            "",
            "不信任的金鑰簽的清單不存"
        );
        assert_eq!(
            released_manifest(
                &answer_with(Some(json!(good.replace("v0.2.0", "v9.9.9")))),
                &trusted
            ),
            "",
            "內容被改過的清單不存"
        );
        assert_eq!(
            released_manifest(&answer_with(Some(json!(good))), &[]),
            "",
            "沒有可信的金鑰就什麼都不存"
        );
    }

    const KEY: &str = "LAYSI-TEST0-TEST0-TEST0-TEST0";

    /// A database of its own on the `TEST_DATABASE_URL` server.
    /// `license_state` is one row, so the race cannot be staged on the shared
    /// test database without racing every other test that reads it.
    struct Scratch {
        admin: sqlx::PgPool,
        options: sqlx::postgres::PgConnectOptions,
        name: String,
    }

    impl Scratch {
        async fn new() -> Self {
            let url =
                std::env::var("TEST_DATABASE_URL").expect("請設定 TEST_DATABASE_URL 後再執行");
            let base: sqlx::postgres::PgConnectOptions =
                url.parse().expect("TEST_DATABASE_URL 應是合法的連線字串");
            let admin = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .connect_with(base.clone())
                .await
                .expect("應可連上測試資料庫");
            let name = format!("laysi_license_{}", uuid::Uuid::new_v4().simple());
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
                .execute(&admin)
                .await
                .expect("應可建立拋棄式資料庫");
            Self {
                admin,
                options: base.database(&name),
                name,
            }
        }

        /// A pool of its own, as each server process has.
        async fn process(&self) -> anyhow::Result<Store> {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect_with(self.options.clone())
                .await?;
            Ok(Store::from_pool(pool))
        }

        async fn remove(self) {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE {} WITH (FORCE)",
                self.name
            )))
            .execute(&self.admin)
            .await
            .expect("應可刪掉拋棄式資料庫");
        }
    }

    /// Stands in for the licence server: hands out `n1`, `n2`, ... in the
    /// order checks arrive, and keeps the nonce each one carried. Each answer
    /// is held back long enough that a check which did not wait for the row
    /// reads it before the first answer is saved.
    async fn stand_in() -> anyhow::Result<(LicenceServer, Arc<Mutex<Vec<String>>>)> {
        stand_in_releasing(None).await
    }

    /// The same, answering with `released` as the release manifest when given.
    /// `release_keys` trusts the key that `manifest` signs with.
    async fn stand_in_releasing(
        released: Option<Value>,
    ) -> anyhow::Result<(LicenceServer, Arc<Mutex<Vec<String>>>)> {
        let signing = SigningKey::from_bytes(&[5u8; 32]);
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let app = axum::Router::new().route(
            "/v1/license/check",
            axum::routing::post({
                let (received, signing, released) =
                    (received.clone(), signing.clone(), released.clone());
                move |axum::Json(request): axum::Json<Value>| async move {
                    let n = {
                        let mut seen = received.lock().unwrap();
                        seen.push(request["nonce"].as_str().unwrap_or_default().to_string());
                        seen.len()
                    };
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let fingerprint = request["fingerprint"].as_str().unwrap_or_default();
                    let token = sign(
                        &signing,
                        &payload(d(2026, 11, 1), d(2027, 9, 1), fingerprint),
                    );
                    let mut answer = json!({ "token": token, "nonce": format!("n{n}") });
                    if let Some(released) = released {
                        answer["release_manifest"] = released;
                    }
                    axum::Json(answer)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/v1/license/check", listener.local_addr()?);
        tokio::spawn(async move { axum::serve(listener, app).await });
        let server = LicenceServer {
            url,
            key: signing.verifying_key(),
            release_keys: vec![update::test_support::key(7).verifying_key()],
        };
        Ok((server, received))
    }

    /// `api` and `mcp` checking at the same moment, which is what put a
    /// 「疑似重複安裝」 on the production licence once a day: the second
    /// check has to carry the nonce the first one was handed, and the row has
    /// to end up holding the nonce handed out last.
    ///
    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-ops
    /// two_processes_checking_at_once_carry_each_others_nonce -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
    async fn two_processes_checking_at_once_carry_each_others_nonce() {
        let scratch = Scratch::new().await;
        let outcome = async {
            let (api, mcp) = (scratch.process().await?, scratch.process().await?);
            api.migrate().await?;
            let (server, received) = stand_in().await?;

            let (a, b) = tokio::join!(
                exchange(&api, Some(KEY), &server),
                exchange(&mcp, Some(KEY), &server)
            );
            anyhow::ensure!(a? && b?, "兩次都是明確要求的檢查，都該送出");
            let received = received.lock().unwrap().clone();
            anyhow::ensure!(
                received == ["", "n1"],
                "後到的檢查要帶先到那次拿到的 nonce，實際收到 {received:?}"
            );
            let stored = api.license_state().await?.nonce;
            anyhow::ensure!(stored == "n2", "要存下最後發出的 nonce，實際是 {stored}");
            Ok(())
        }
        .await;
        scratch.remove().await;
        outcome.unwrap();
    }

    /// The daily loop runs in every process, and processes started together
    /// come due together. Only one of them may check in.
    ///
    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-ops
    /// processes_due_together_check_in_once -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
    async fn processes_due_together_check_in_once() {
        let scratch = Scratch::new().await;
        let outcome = async {
            let (api, mcp) = (scratch.process().await?, scratch.process().await?);
            api.migrate().await?;
            sqlx::query("UPDATE license_state SET activation_key = $1 WHERE id = 1")
                .bind(KEY)
                .execute(api.pool())
                .await?;
            let (server, received) = stand_in().await?;

            let (a, b) = tokio::join!(exchange(&api, None, &server), exchange(&mcp, None, &server));
            let asked = [a?, b?].iter().filter(|asked| **asked).count();
            anyhow::ensure!(
                asked == 1,
                "同時到期的兩個程序只能有一個送出，實際 {asked} 個"
            );
            let received = received.lock().unwrap().clone();
            anyhow::ensure!(
                received == [""],
                "授權伺服器只該收到一次，實際 {received:?}"
            );
            let state = api.license_state().await?;
            anyhow::ensure!(state.nonce == "n1" && state.last_check_at.is_some());
            Ok(())
        }
        .await;
        scratch.remove().await;
        outcome.unwrap();
    }

    /// A check stores the manifest it carried, replaces it with the next one,
    /// and clears it when a later check carries none or one that does not
    /// verify: the licence server withdrawing a release has to take it off the
    /// screen.
    ///
    /// Run with:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p laysi-ops
    /// a_check_stores_replaces_and_clears_the_release_manifest -- --ignored`
    #[tokio::test]
    #[ignore = "需要 TEST_DATABASE_URL 指向可建立資料庫的 PostgreSQL"]
    async fn a_check_stores_replaces_and_clears_the_release_manifest() {
        use update::test_support::{key, manifest};
        let scratch = Scratch::new().await;
        let outcome = async {
            let store = scratch.process().await?;
            store.migrate().await?;
            let first = manifest("v0.2.0", &key(7));
            let second = manifest("v0.3.0", &key(7));

            for (sent, expected) in [
                (Some(json!(first.clone())), first.clone()),
                (Some(json!(second.clone())), second.clone()),
                (None, String::new()),
                (Some(json!(first.clone())), first.clone()),
                (Some(json!(manifest("v0.9.0", &key(8)))), String::new()),
            ] {
                let (server, _) = stand_in_releasing(sent).await?;
                anyhow::ensure!(exchange(&store, Some(KEY), &server).await?);
                let stored = store.license_state().await?.release_manifest;
                anyhow::ensure!(stored == expected, "存下的清單不符：{stored:?}");
            }
            Ok(())
        }
        .await;
        scratch.remove().await;
        outcome.unwrap();
    }
}
