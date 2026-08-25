//! Bulletin board: announcements and company rules.
//!
//! Both live in one table. What separates them is `kind`, and nothing else:
//! an announcement is read and forgotten, a rule is looked up months later to
//! settle what was in force at the time.
//!
//! **Every version is kept.** Changing the title, the body or the effective
//! date of a notice publishes a new version rather than overwriting the old
//! one, and anyone who can see the board can read any version. That is the
//! only record either side has of what the rule said on a given day, so it
//! cannot be a copy the newest edit is allowed to destroy.
//!
//! There is no read-confirmation. What the board does instead is remember,
//! per person, the moment they last opened it, so that "還沒看過" can be said
//! without asking anyone to sign anything. That mark lives with the
//! self-service side; nothing here reports who has read what.
//!
//! Employees reach the same rows through `list_my_notices` in `selfservice`,
//! which takes no employee field at all. `list_notices` here is the HR side:
//! it also lists what is scheduled and what has already come down.

use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_store::{NewNotice, Notice, NoticeEdit, NoticeVersion};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(ListNotices), Box::new(CreateNotice), Box::new(UpdateNotice)]
}

/// The two kinds the database accepts, with their labels.
pub(crate) const NOTICE_KINDS: [(&str, &str); 2] =
    [("announcement", "公告"), ("policy", "規章")];

pub(crate) fn kind_label(kind: &str) -> &'static str {
    NOTICE_KINDS.iter().find(|(code, _)| *code == kind).map_or("公告", |(_, label)| *label)
}

/// Where a notice stands today, as a code and the word for it.
///
/// The two kinds get different words for the same three states: a rule is in
/// force or repealed, an announcement is up or taken down.
pub(crate) fn notice_status(n: &Notice, today: NaiveDate) -> (&'static str, &'static str) {
    let policy = n.kind == "policy";
    if n.effective_on > today {
        return ("scheduled", if policy { "未生效" } else { "未發布" });
    }
    if n.expires_on.is_some_and(|end| end < today) {
        return ("ended", if policy { "已廢止" } else { "已下架" });
    }
    ("active", if policy { "施行中" } else { "公告中" })
}

/// The fields every screen shows. Callers add what only their side knows.
pub(crate) fn notice_json(n: &Notice, today: NaiveDate) -> Value {
    let (status, status_label) = notice_status(n, today);
    json!({
        "id": n.id,
        "kind": n.kind,
        "kind_label": kind_label(&n.kind),
        "title": n.title,
        "body": n.body,
        "effective_on": n.effective_on,
        "expires_on": n.expires_on,
        "status": status,
        "status_label": status_label,
        "version_no": n.version_no,
        "versions": n.versions,
        "published_at": n.published_at,
        "published_by": n.published_by,
    })
}

pub(crate) fn version_json(v: &NoticeVersion) -> Value {
    json!({
        "version_no": v.version_no,
        "title": v.title,
        "body": v.body,
        "effective_on": v.effective_on,
        "published_at": v.published_at,
        "published_by": v.published_by,
    })
}

/// One notice with every version it has ever had, newest first.
///
/// Shared by both list operations: the history is the same history whoever
/// asks, and neither side is allowed a different answer.
pub(crate) async fn history_outcome(
    ctx: &OpCtx,
    notice: &Notice,
    scope_note: Option<&str>,
) -> Result<OpOutcome, OpError> {
    let versions = ctx.store.notice_history(notice.id).await?;
    let mut data = json!({
        "notice": notice_json(notice, ctx.today),
        "versions": versions.iter().map(version_json).collect::<Vec<_>>(),
    });
    if let Some(note) = scope_note {
        data["scope_note"] = json!(note);
    }
    let summary = format!(
        "讀{}修訂紀錄 · {} · 共 {} 版",
        kind_label(&notice.kind),
        notice.title,
        versions.len()
    );
    Ok(OpOutcome::new(data, summary))
}

pub(crate) fn checked_kind(kind: &str) -> Result<&str, OpError> {
    NOTICE_KINDS
        .iter()
        .find(|(code, _)| *code == kind)
        .map(|(code, _)| *code)
        .ok_or_else(|| OpError::BadInput("類別請填 announcement 或 policy".into()))
}

fn checked_text(value: &str, field: &str) -> Result<String, OpError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(OpError::BadInput(format!("{field}還沒填")));
    }
    Ok(trimmed.to_string())
}

fn checked_window(effective_on: NaiveDate, expires_on: Option<NaiveDate>) -> Result<(), OpError> {
    match expires_on {
        Some(end) if end < effective_on => {
            Err(OpError::BadInput("結束日期早於開始日期，請改成開始日之後的日期".into()))
        }
        _ => Ok(()),
    }
}

pub(crate) fn missing_notice() -> OpError {
    OpError::BadInput("這則公告或規章已經不存在，請重新整理本頁。".into())
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListNoticesInput {
    /// One notice, from a previous listing. Given, the reply is that notice
    /// with every version it has ever had instead of the list.
    #[serde(default)]
    pub id: Option<i32>,
    /// Restrict to one kind, `announcement` or `policy`. Omit for both.
    #[serde(default)]
    pub kind: Option<String>,
    /// Include the ones already taken down or repealed.
    #[serde(default)]
    pub include_ended: bool,
}

struct ListNotices;

#[async_trait]
impl Operation for ListNotices {
    fn name(&self) -> &'static str {
        "list_notices"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀公告與規章，含生效期間與版次；帶 id 則回該則的每一個版本"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListNoticesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListNoticesInput = parse(input)?;
        if let Some(id) = i.id {
            let notice = ctx.store.notice(id).await?.ok_or_else(missing_notice)?;
            return history_outcome(ctx, &notice, None).await;
        }

        let kind = i.kind.as_deref().map(checked_kind).transpose()?;
        let rows = ctx.store.notices(kind, i.include_ended, ctx.today).await?;

        let policies = rows.iter().filter(|n| n.kind == "policy").count();
        let announcements = rows.len() - policies;
        let summary = format!(
            "讀公告與規章 · {} 則（公告 {announcements} 則 · 規章 {policies} 則）",
            rows.len()
        );
        Ok(OpOutcome::new(
            json!({
                "notices": rows.iter().map(|n| notice_json(n, ctx.today)).collect::<Vec<_>>(),
                "kinds": NOTICE_KINDS
                    .iter()
                    .map(|(code, label)| json!({ "kind": code, "label": label }))
                    .collect::<Vec<_>>(),
                "announcement_count": announcements,
                "policy_count": policies,
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateNoticeInput {
    /// `announcement` for a notice board post, `policy` for a company rule.
    pub kind: String,
    pub title: String,
    /// Plain text. Line breaks are kept as written.
    pub body: String,
    /// Publication date, or the date a rule takes effect. Defaults to today.
    #[serde(default)]
    pub effective_on: Option<NaiveDate>,
    /// The day it comes down or is repealed. Omit to leave it standing.
    #[serde(default)]
    pub expires_on: Option<NaiveDate>,
}

struct CreateNotice;

#[async_trait]
impl Operation for CreateNotice {
    fn name(&self) -> &'static str {
        "create_notice"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "發布一則公告或規章，記為第 1 版"
    }

    fn input_schema(&self) -> Value {
        schema_of::<CreateNoticeInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CreateNoticeInput = parse(input)?;
        let kind = checked_kind(&i.kind)?;
        let title = checked_text(&i.title, "標題")?;
        let body = checked_text(&i.body, "內容")?;
        let effective_on = i.effective_on.unwrap_or(ctx.today);
        checked_window(effective_on, i.expires_on)?;

        let notice = ctx
            .store
            .create_notice(NewNotice {
                kind,
                title: &title,
                body: &body,
                effective_on,
                expires_on: i.expires_on,
                actor: &ctx.actor,
            })
            .await?;

        let summary = format!("發布{} · {}", kind_label(kind), notice.title);
        Ok(OpOutcome::new(json!({ "notice": notice_json(&notice, ctx.today) }), summary))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateNoticeInput {
    /// Which notice, from `list_notices`.
    pub id: i32,
    /// Changing this publishes a new version.
    #[serde(default)]
    pub title: Option<String>,
    /// Changing this publishes a new version.
    #[serde(default)]
    pub body: Option<String>,
    /// Changing this publishes a new version.
    #[serde(default)]
    pub effective_on: Option<NaiveDate>,
    /// `YYYY-MM-DD` to set the end date, an empty string to clear it, omitted
    /// to leave it alone. This one is not part of the content, so it does not
    /// publish a version.
    #[serde(default)]
    pub expires_on: Option<String>,
}

struct UpdateNotice;

#[async_trait]
impl Operation for UpdateNotice {
    fn name(&self) -> &'static str {
        "update_notice"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "修改公告或規章，或設定下架與廢止日期。改動標題、內容或生效日會發布新的一版，先前版本留著供查閱。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<UpdateNoticeInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: UpdateNoticeInput = parse(input)?;
        let current = ctx.store.notice(i.id).await?.ok_or_else(missing_notice)?;

        let title = i.title.as_deref().map(|t| checked_text(t, "標題")).transpose()?;
        let body = i.body.as_deref().map(|b| checked_text(b, "內容")).transpose()?;
        let expires_on = match i.expires_on.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(text) => Some(Some(NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(
                |_| OpError::BadInput("結束日期請填 YYYY-MM-DD".into()),
            )?)),
        };
        checked_window(
            i.effective_on.unwrap_or(current.effective_on),
            expires_on.unwrap_or(current.expires_on),
        )?;

        if title.is_none() && body.is_none() && i.effective_on.is_none() && expires_on.is_none() {
            return Err(OpError::BadInput("沒有變更".into()));
        }

        let (notice, revised) = ctx
            .store
            .update_notice(NoticeEdit {
                id: i.id,
                title: title.as_deref(),
                body: body.as_deref(),
                effective_on: i.effective_on,
                expires_on,
                actor: &ctx.actor,
            })
            .await?;

        let summary = if revised {
            format!(
                "更新{} · {} · 第 {} 版",
                kind_label(&notice.kind),
                notice.title,
                notice.version_no
            )
        } else {
            format!("更新{} · {}", kind_label(&notice.kind), notice.title)
        };
        Ok(OpOutcome::new(
            json!({
                "notice": notice_json(&notice, ctx.today),
                "revised": revised,
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: &str, effective: &str, expires: Option<&str>, published: &str) -> Notice {
        let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let at = day(published);
        Notice {
            id: 1,
            kind: kind.into(),
            expires_on: expires.map(day),
            version_no: 1,
            versions: 1,
            title: "出勤管理辦法".into(),
            body: "上班時間 09:00。".into(),
            effective_on: day(effective),
            published_at: at.and_hms_opt(2, 0, 0).unwrap().and_utc(),
            published_by: "王小美".into(),
        }
    }

    fn on(day: &str) -> NaiveDate {
        NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap()
    }

    /// The same three states, but a rule is repealed where an announcement is
    /// taken down. One vocabulary for both would put 「已下架」 on a rule.
    #[test]
    fn each_kind_gets_its_own_words() {
        assert_eq!(
            notice_status(&notice("policy", "2026-09-01", None, "2026-08-25"), on("2026-08-25")).1,
            "未生效"
        );
        assert_eq!(
            notice_status(
                &notice("announcement", "2026-09-01", None, "2026-08-25"),
                on("2026-08-25")
            )
            .1,
            "未發布"
        );
        assert_eq!(
            notice_status(&notice("policy", "2026-01-01", None, "2026-01-01"), on("2026-08-25")).1,
            "施行中"
        );
        assert_eq!(
            notice_status(
                &notice("policy", "2026-01-01", Some("2026-08-24"), "2026-01-01"),
                on("2026-08-25")
            )
            .1,
            "已廢止"
        );
    }

    /// The last day is inclusive: something that comes down today is still up
    /// today.
    #[test]
    fn the_end_date_is_the_last_day_it_stands() {
        let n = notice("announcement", "2026-08-01", Some("2026-08-25"), "2026-08-01");
        assert_eq!(notice_status(&n, on("2026-08-25")).0, "active");
    }

    #[test]
    fn an_end_date_before_the_start_is_refused() {
        assert!(checked_window(on("2026-09-01"), Some(on("2026-08-01"))).is_err());
        assert!(checked_window(on("2026-09-01"), Some(on("2026-09-01"))).is_ok());
        assert!(checked_window(on("2026-09-01"), None).is_ok());
    }
}
