//! Sends a generated password to the account it belongs to.
//!
//! Configured by `SMTP_URL` and `SMTP_FROM` alone (each also readable as
//! `_FILE`). This server is one deployment run by the vendor rather than
//! something a customer installs, so there is nobody to put a settings screen
//! in front of and no value for anybody else to invent.
//!
//! Absent or unusable configuration is not an error: the console falls back
//! to showing the password once on screen, which is what it did before mail
//! existed. A password that reaches nobody is the failure worth avoiding.

use anyhow::{anyhow, Context, Result};
use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl Mailer {
    /// `None` when either setting is missing, which switches the console back
    /// to showing passwords on screen.
    pub fn from_env(url: Option<String>, from: Option<String>) -> Option<Self> {
        let (url, from) = (url?, from?);
        let from: Mailbox = match from.parse() {
            Ok(from) => from,
            Err(error) => {
                tracing::warn!(%error, "SMTP_FROM is not an address; mail is off");
                return None;
            }
        };
        match AsyncSmtpTransport::<Tokio1Executor>::from_url(&url) {
            Ok(builder) => Some(Self {
                transport: builder.build(),
                from,
            }),
            Err(error) => {
                tracing::warn!(%error, "SMTP_URL is not usable; mail is off");
                None
            }
        }
    }

    /// The address is typed by whoever created the account, so a rejected
    /// mailbox is an ordinary mistake rather than a fault: the caller reports
    /// it and shows the password instead.
    pub async fn send_password(&self, to: &str, password: &str, base: &str) -> Result<()> {
        let to: Mailbox = to.parse().map_err(|_| anyhow!("{to} 不是可以收信的地址"))?;
        let body = format!(
            "Laysi One 授權管理\n\n\
             帳號：{to_address}\n\
             密碼：{password}\n\
             網址：{base}\n\n\
             首次登入必須重新設定密碼。本密碼僅此一封信留存。\n",
            to_address = to.email,
        );
        let message = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject("Laysi One 授權管理 · 登入密碼")
            .body(body)
            .context("組不出郵件內容")?;
        self.transport.send(message).await.context("寄送郵件失敗")?;
        Ok(())
    }
}
