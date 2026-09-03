//! Pushing a message out to the company's chat tool.
//!
//! One address, kept in `company_setting`, and one message shape per tool.
//! The tools do not agree on where a plain message goes: Slack, Google Chat
//! and the retired Teams connector read `text`, Discord reads `content`,
//! Lark reads `content.text` under a `msg_type`, and Teams through Power
//! Automate takes an adaptive card. Discord's `content` is a string where
//! Lark's is an object, so one payload carrying every field at once is not
//! available.
//!
//! The shape is chosen from the host rather than from a second setting. The
//! address already says which tool it is, and a dropdown that has to agree
//! with it is one more thing to get wrong. An unrecognised host gets `text`,
//! which is what a relay or a workflow tool reads as well.
//!
//! Delivery is attempted while the caller waits and the result is reported
//! back to the person who published, rather than queued and retried. The
//! board itself is the record, so a message that did not go out costs a
//! sentence on the screen and nothing else.

use crate::{OpCtx, OpError, OpOutcome, Operation, Risk};
use serde_json::{json, Value};
use std::time::Duration;

/// How a tool takes a plain message.
#[derive(Clone, Copy)]
enum Shape {
    /// `{"text": …}`, which is also what a relay or a workflow tool reads.
    Text,
    /// `{"content": …}`
    Discord,
    /// `{"msg_type": "text", "content": {"text": …}}`
    Lark,
    /// An adaptive card, which is what Teams takes through Power Automate.
    Card,
}

/// The tools that need a shape or a name of their own, by the domain their
/// addresses sit under. The screen matches an address against this same list
/// while it is being typed, so what it says is sent and what is sent are one
/// list rather than two that drift.
const TOOLS: [(&str, &str, Shape); 8] = [
    ("hooks.slack.com", "Slack", Shape::Text),
    ("chat.googleapis.com", "Google Chat", Shape::Text),
    ("webhook.office.com", "Teams", Shape::Text),
    ("logic.azure.com", "Teams", Shape::Card),
    ("discord.com", "Discord", Shape::Discord),
    ("discordapp.com", "Discord", Shape::Discord),
    ("open.feishu.cn", "飛書", Shape::Lark),
    ("open.larksuite.com", "Lark", Shape::Lark),
];

/// What an address below none of those domains is called.
const GENERIC: &str = "聊天室";

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![Box::new(TestNoticePush)]
}

/// Where the board is pushed. Empty means nobody asked for a push.
pub const NOTICE_WEBHOOK_SETTING: &str = "notice_webhook_url";

/// Discord refuses a `content` over 2000 characters, the lowest ceiling
/// among the tools.
const MAX_CHARS: usize = 1800;

const TIMEOUT: Duration = Duration::from_secs(10);

/// An address a message can actually be posted to.
pub fn validate_url(value: &str) -> Result<(), OpError> {
    let url = reqwest::Url::parse(value).map_err(|_| {
        OpError::BadInput("推送網址請填完整的網址，例如 https://hooks.slack.com/services/…".into())
    })?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(OpError::BadInput("推送網址要以 https:// 開頭".into()));
    }
    Ok(())
}

/// Whether a host is that domain or one below it.
fn under(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// The tool an address belongs to.
fn tool_of(url: &str) -> Option<(&'static str, Shape)> {
    let host = reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_lowercase)?;
    TOOLS
        .iter()
        .find(|(domain, _, _)| under(&host, domain))
        .map(|(_, label, shape)| (*label, *shape))
}

/// What to call the place an address posts to.
pub fn target_label(url: &str) -> &'static str {
    tool_of(url).map_or(GENERIC, |(label, _)| label)
}

/// The list the screen matches a half-typed address against, so that what it
/// says will receive the message is decided by this table and not by a copy
/// of it written in TypeScript.
pub fn targets() -> Value {
    json!({
        "generic": GENERIC,
        "domains": TOOLS
            .iter()
            .map(|(domain, label, _)| json!({ "domain": domain, "label": label }))
            .collect::<Vec<_>>(),
    })
}

/// The message body the tool at this address understands.
fn payload(url: &str, text: &str) -> Value {
    match tool_of(url).map(|(_, shape)| shape) {
        Some(Shape::Lark) => json!({ "msg_type": "text", "content": { "text": text } }),
        Some(Shape::Discord) => json!({ "content": text }),
        Some(Shape::Card) => json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {
                    "type": "AdaptiveCard",
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "version": "1.4",
                    "body": [{ "type": "TextBlock", "text": text, "wrap": true }],
                },
            }],
        }),
        Some(Shape::Text) | None => json!({ "text": text }),
    }
}
/// Cut to what the strictest tool accepts, saying that it was cut.
fn truncated(text: &str) -> String {
    if text.chars().count() <= MAX_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_CHARS).collect();
    format!("{head}…（全文見公告與規章）")
}

async fn deliver(url: &str, text: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| "推送失敗：連線設定有誤".to_string())?;
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .body(payload(url, &truncated(text)).to_string())
        .send()
        .await
        .map_err(|_| "推送失敗：無法連線到推送網址，請確認基礎設定的網址與對外連線".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "推送失敗：聊天室回應 {}，請確認基礎設定的推送網址",
            response.status().as_u16()
        ));
    }
    Ok(())
}

/// Send one message, if an address is set.
///
/// `None` means no address, so nothing is reported. `Some` is the sentence
/// for the person who published, delivered or not: the write it follows has
/// already succeeded, and a chat tool being down does not undo it.
pub(crate) async fn push(ctx: &OpCtx, text: &str) -> Option<String> {
    let settings = ctx.store.company_settings().await.ok()?;
    let url = settings
        .iter()
        .find(|s| s.code == NOTICE_WEBHOOK_SETTING)?
        .value
        .trim()
        .to_string();
    if url.is_empty() {
        return None;
    }
    Some(match deliver(&url, text).await {
        Ok(()) => format!("已推送到{}", target_label(&url)),
        Err(note) => note,
    })
}

/// A message sent by hand, so that an address nobody has posted to yet is
/// not first exercised by a company announcement.
struct TestNoticePush;

#[async_trait::async_trait]
impl Operation for TestNoticePush {
    fn name(&self) -> &'static str {
        "test_notice_push"
    }

    fn risk(&self) -> Risk {
        Risk::AdminOnly
    }

    fn summary(&self) -> &'static str {
        "送一則測試訊息到公告推送網址"
    }

    fn input_schema(&self) -> Value {
        crate::schema_of::<crate::Empty>()
    }

    async fn call(&self, ctx: &OpCtx, _input: Value) -> Result<OpOutcome, OpError> {
        let note = push(ctx, "Laysi One 推送測試").await.ok_or_else(|| {
            OpError::BadInput("公告推送網址還沒填，請在基礎設定填入網址後再測試。".into())
        })?;
        Ok(OpOutcome::new(
            json!({ "push": note }),
            format!("測試公告推送 · {note}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool reads a different field, so the host has to pick the shape.
    /// Sending Slack's `text` to Lark posts nothing and still answers 200.
    #[test]
    fn each_tool_gets_the_field_it_reads() {
        let text = "【公告】停電通知";
        assert_eq!(
            payload("https://hooks.slack.com/services/T/B/x", text)["text"],
            json!(text)
        );
        assert_eq!(
            payload(
                "https://chat.googleapis.com/v1/spaces/A/messages?key=k",
                text
            )["text"],
            json!(text)
        );
        assert_eq!(
            payload("https://discord.com/api/webhooks/1/x", text)["content"],
            json!(text)
        );
        assert_eq!(
            payload("https://open.feishu.cn/open-apis/bot/v2/hook/x", text)["content"]["text"],
            json!(text)
        );
        assert_eq!(
            payload("https://open.larksuite.com/open-apis/bot/v2/hook/x", text)["msg_type"],
            json!("text")
        );
        assert_eq!(
            payload("https://prod-1.westus.logic.azure.com/workflows/x", text)["attachments"][0]
                ["content"]["body"][0]["text"],
            json!(text)
        );
    }

    /// A host nobody knows is a relay or a workflow tool, and those read the
    /// plain field.
    #[test]
    fn an_unknown_host_gets_text() {
        assert_eq!(
            payload("https://hooks.example.com/abc", "嗨")["text"],
            json!("嗨")
        );
    }

    /// The domain has to be the host, not a word somewhere in the address.
    #[test]
    fn a_lookalike_address_is_not_the_tool() {
        let out = payload("https://relay.example.com/discord.com/hook", "嗨");
        assert_eq!(out["text"], json!("嗨"));
        assert!(out.get("content").is_none());
    }

    #[test]
    fn a_long_notice_is_cut_to_what_discord_takes() {
        let long = "字".repeat(5000);
        let cut = truncated(&long);
        assert!(cut.chars().count() < 2000);
        assert!(cut.ends_with("（全文見公告與規章）"));
        assert_eq!(truncated("短"), "短");
    }

    #[test]
    fn only_https_addresses_are_stored() {
        assert!(validate_url("https://hooks.slack.com/services/T/B/x").is_ok());
        assert!(validate_url("http://hooks.slack.com/services/T/B/x").is_err());
        assert!(validate_url("hooks.slack.com").is_err());
        assert!(validate_url("").is_err());
    }
}
