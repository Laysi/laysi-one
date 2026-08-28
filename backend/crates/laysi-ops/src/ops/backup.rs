//! Off-site backup: where the copy goes and who can still open it.
//!
//! The destination is any S3-compatible bucket and it is a setting, not
//! deployment wiring: nothing in this product asks a person to edit a file on
//! the host. `secret_key` goes in and never comes back, the same contract as
//! the OIDC client secret.
//!
//! The key pair is asymmetric because a dump carries complete national ID
//! numbers and the copy in someone else's bucket has to be encrypted, while a
//! symmetric key would have to stay readable to whatever encrypts — which
//! means storing it in the database that is itself inside the encrypted file.
//! Restoring would then need the key that is only inside what it unlocks.
//!
//! The backup container generates the pair, because it is what owns `age` and
//! therefore what cannot get the key format wrong. The private half lands in
//! `age_identity_pending` and the screen shows it once;
//! [`ConfirmBackupKey`] is the administrator saying it is written down, and
//! the job refuses to run until then, so no archive ever contains the key
//! that opens it.

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use laysi_store::WriteBackupSetting;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(GetBackupSetting),
        Box::new(UpdateBackupSetting),
        Box::new(ConfirmBackupKey),
    ]
}

/// What still stands between the settings and a backup actually running.
///
/// Returned as one sentence rather than a set of flags: the screen prints it
/// where the next backup time would otherwise go, and every item is something
/// the administrator does on that same page.
fn blocked_reason(s: &laysi_store::BackupSetting) -> Option<String> {
    if !s.enabled {
        return Some("備份未啟用。".into());
    }
    if s.bucket.trim().is_empty() || s.endpoint.trim().is_empty() {
        return Some("尚未填寫儲存位置與桶子名稱。".into());
    }
    if s.access_key.trim().is_empty() || s.secret_key.is_empty() {
        return Some("尚未填寫存取金鑰。".into());
    }
    if s.age_recipient.trim().is_empty() {
        return Some("第一次備份前會產生還原金鑰，產生後請保存。".into());
    }
    if !s.age_identity_pending.is_empty() {
        return Some("還原金鑰尚未保存，保存後才會開始備份。".into());
    }
    None
}

struct GetBackupSetting;

#[async_trait]
impl Operation for GetBackupSetting {
    fn name(&self) -> &'static str {
        "get_backup_setting"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "讀異地備份設定：儲存位置、桶子、路徑、保留天數與上次備份結果。存取金鑰不在回應內，設定後也無法取回。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let s = ctx.store.backup_setting().await?;
        let blocked = blocked_reason(&s);
        let summary = match (&blocked, s.last_run_at) {
            (Some(reason), _) => format!("讀異地備份設定 · {reason}"),
            (None, Some(at)) => format!("讀異地備份設定 · 上次備份 {at}"),
            (None, None) => "讀異地備份設定 · 尚未備份過".to_string(),
        };
        Ok(OpOutcome::new(
            json!({
                "enabled": s.enabled,
                "endpoint": s.endpoint,
                "region": s.region,
                "bucket": s.bucket,
                "prefix": s.prefix,
                "access_key": s.access_key,
                "has_secret_key": !s.secret_key.is_empty(),
                "keep_days": s.keep_days,
                "age_recipient": s.age_recipient,
                // Shown once. Non-empty means nobody has saved it yet, which
                // is also why no backup has run.
                "restore_key_to_save": s.age_identity_pending,
                "blocked_reason": blocked,
                "last_run_at": s.last_run_at,
                "last_error": s.last_error,
                "last_bytes": s.last_bytes,
                "updated_at": s.updated_at,
                "updated_by": s.updated_by,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateBackupSettingInput {
    /// Whether backups run at all. Turning it on needs the location, the
    /// bucket and both halves of the access key.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The service address, e.g. `https://s3.ap-northeast-1.amazonaws.com`.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
    /// Folder inside the bucket. An empty string writes at the top.
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub access_key: Option<String>,
    /// Write only. Leave the field out to keep the stored key; an empty
    /// string clears it.
    #[serde(default)]
    pub secret_key: Option<String>,
    /// How many days of backups the remote keeps.
    #[serde(default)]
    pub keep_days: Option<i32>,
}

struct UpdateBackupSetting;

#[async_trait]
impl Operation for UpdateBackupSetting {
    fn name(&self) -> &'static str {
        "update_backup_setting"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "設定異地備份：儲存位置、桶子、路徑、存取金鑰與保留天數。存取密鑰留空表示沿用原設定。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UpdateBackupSettingInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UpdateBackupSettingInput = parse(input)?;
        let now = ctx.store.backup_setting().await?;

        let endpoint = i
            .endpoint
            .as_deref()
            .map(str::trim)
            .unwrap_or(&now.endpoint);
        let bucket = i.bucket.as_deref().map(str::trim).unwrap_or(&now.bucket);
        if !endpoint.is_empty()
            && !(endpoint.starts_with("https://") || endpoint.starts_with("http://"))
        {
            return Err(OpError::BadInput(
                "儲存位置請填完整網址，開頭為 https://。".into(),
            ));
        }
        if let Some(days) = i.keep_days {
            if days < 1 {
                return Err(OpError::BadInput("保留天數至少 1 天。".into()));
            }
        }
        // Enabling with nowhere to write would leave a switch that is on and
        // a job that never runs, and the screen would have to explain why.
        let turning_on = i.enabled.unwrap_or(now.enabled);
        if turning_on && (endpoint.is_empty() || bucket.is_empty()) {
            return Err(OpError::BadInput(
                "請先填寫儲存位置與桶子名稱，再啟用備份。".into(),
            ));
        }

        let saved = ctx
            .store
            .update_backup_setting(WriteBackupSetting {
                enabled: i.enabled,
                endpoint: i.endpoint.as_deref().map(str::trim),
                region: i.region.as_deref().map(str::trim),
                bucket: i.bucket.as_deref().map(str::trim),
                prefix: i.prefix.as_deref().map(str::trim),
                access_key: i.access_key.as_deref().map(str::trim),
                secret_key: i.secret_key.as_deref(),
                keep_days: i.keep_days,
                updated_by: &ctx.actor,
            })
            .await?;

        let blocked = blocked_reason(&saved);
        let summary = match &blocked {
            Some(reason) => format!("設定異地備份 · {reason}"),
            None => format!("設定異地備份 · 目的地 {}", saved.bucket),
        };
        Ok(OpOutcome::new(
            json!({ "bucket": saved.bucket, "enabled": saved.enabled, "blocked_reason": blocked }),
            summary,
        ))
    }
}

struct ConfirmBackupKey;

#[async_trait]
impl Operation for ConfirmBackupKey {
    fn name(&self) -> &'static str {
        "confirm_backup_key"
    }
    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }
    fn summary(&self) -> &'static str {
        "確認還原金鑰已保存。確認後系統不再留存該金鑰，備份隨即開始執行。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let now = ctx.store.backup_setting().await?;
        if now.age_identity_pending.is_empty() {
            return Err(OpError::BadInput("目前沒有待保存的還原金鑰。".into()));
        }
        let saved = ctx.store.clear_backup_identity(&ctx.actor).await?;
        Ok(OpOutcome::new(
            json!({ "blocked_reason": blocked_reason(&saved) }),
            "確認還原金鑰已保存 · 系統不再留存".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting() -> laysi_store::BackupSetting {
        laysi_store::BackupSetting {
            enabled: true,
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            bucket: "laysi-backup".into(),
            prefix: String::new(),
            access_key: "key".into(),
            secret_key: "secret".into(),
            age_recipient: "age1example".into(),
            age_identity_pending: String::new(),
            keep_days: 30,
            updated_at: chrono::Utc::now(),
            updated_by: String::new(),
            last_run_at: None,
            last_error: String::new(),
            last_bytes: None,
        }
    }

    /// The one rule the whole design rests on: while the restore key is still
    /// waiting to be written down, no backup runs. Otherwise the first archive
    /// would contain the key that opens it.
    #[test]
    fn nothing_runs_while_the_restore_key_is_unsaved() {
        let mut s = setting();
        assert_eq!(blocked_reason(&s), None, "設定齊全時不應被擋");

        s.age_identity_pending = "AGE-SECRET-KEY-1EXAMPLE".into();
        assert!(blocked_reason(&s).is_some(), "金鑰未保存時必須擋下備份");

        s.age_identity_pending = String::new();
        s.age_recipient = String::new();
        assert!(blocked_reason(&s).is_some(), "還沒有公鑰時也不該備份");
    }

    /// Every reason names something the administrator does on the same page.
    #[test]
    fn every_reason_says_what_to_do_rather_than_what_is_wrong() {
        let mut s = setting();
        for missing in [0, 1, 2] {
            match missing {
                0 => s.enabled = false,
                1 => {
                    s.enabled = true;
                    s.bucket = String::new();
                }
                _ => {
                    s.bucket = "laysi-backup".into();
                    s.secret_key = String::new();
                }
            }
            let reason = blocked_reason(&s).expect("應被擋下");
            assert!(!reason.contains('_'), "畫面文字不得出現識別字：{reason}");
            assert!(reason.ends_with('。'), "訊息應為完整句子：{reason}");
        }
    }
}
