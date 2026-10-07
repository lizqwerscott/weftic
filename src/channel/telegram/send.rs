//! Telegram outbound: `sendMessage` / `editMessageText`.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::channel::DeliveryTarget;
use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
use crate::channel::registry::ChannelRuntime;
use crate::channel::sender::{BoxedSendFuture, ChannelSender, SentMessage};

const API_BASE: &str = "https://api.telegram.org";

pub struct TelegramSender {
    client: reqwest::Client,
    token: String,
    chat_id: String,
}

impl TelegramSender {
    pub fn new(token: impl Into<String>, chat_id: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.into(),
            chat_id: chat_id.into(),
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{API_BASE}/bot{}/{}", self.token, method)
    }
}

impl ChannelSender for TelegramSender {
    fn send_message<'a>(&'a self, text: &'a str) -> BoxedSendFuture<'a, Result<SentMessage>> {
        Box::pin(async move {
            let body = serde_json::json!({ "chat_id": self.chat_id, "text": text });

            let response = self
                .client
                .post(self.url("sendMessage"))
                .json(&body)
                .send()
                .await
                .context("calling telegram sendMessage")?
                .text()
                .await
                .context("reading telegram sendMessage response")?;

            let sent = parse_sent(&response)?;
            tracing::info!(target: "telegram", "sent message {}", sent.id);
            Ok(sent)
        })
    }

    fn edit_message<'a>(
        &'a self,
        message_id: &'a str,
        text: &'a str,
    ) -> BoxedSendFuture<'a, Result<()>> {
        Box::pin(async move {
            let body = serde_json::json!({
                "chat_id": self.chat_id,
                "message_id": message_id,
                "text": text,
            });

            let response = self
                .client
                .post(self.url("editMessageText"))
                .json(&body)
                .send()
                .await
                .context("calling telegram editMessageText")?
                .text()
                .await
                .context("reading telegram editMessageText response")?;

            parse_edited(&response)?;
            tracing::info!(target: "telegram", "edited message {message_id}");
            Ok(())
        })
    }
}

pub struct TelegramRuntime {
    token: String,
}

impl TelegramRuntime {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl ChannelRuntime for TelegramRuntime {
    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::new(StreamMode::Off, ReplyMode::MessageTool)
    }

    fn sender(&self, target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>> {
        Some(Arc::new(TelegramSender::new(
            self.token.clone(),
            target.target_id(),
        )))
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    ok: bool,
    description: Option<String>,
    result: Option<serde_json::Value>,
}

fn parse_sent(body: &str) -> Result<SentMessage> {
    let response: ApiResponse =
        serde_json::from_str(body).context("decoding telegram sendMessage response")?;

    if !response.ok {
        return Err(anyhow!(
            "telegram sendMessage failed: {}",
            response
                .description
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }

    let result = response
        .result
        .ok_or_else(|| anyhow!("telegram sendMessage returned no result"))?;

    let message_id = result
        .get("message_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow!("telegram sendMessage response has no message_id"))?;

    Ok(SentMessage::new(message_id.to_string()))
}

fn parse_edited(body: &str) -> Result<()> {
    let response: ApiResponse =
        serde_json::from_str(body).context("decoding telegram editMessageText response")?;

    if !response.ok {
        return Err(anyhow!(
            "telegram editMessageText failed: {}",
            response
                .description
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;

    #[test]
    fn parse_sent_reads_the_message_id() {
        let sent = parse_sent(r#"{"ok":true,"result":{"message_id":42}}"#).unwrap();

        assert_eq!(sent, SentMessage::new("42"));
    }

    #[test]
    fn parse_sent_rejects_an_error_response() {
        let error =
            parse_sent(r#"{"ok":false,"description":"Bad Request: chat not found"}"#).unwrap_err();

        assert_eq!(
            error.to_string(),
            "telegram sendMessage failed: Bad Request: chat not found"
        );
    }

    #[test]
    fn parse_sent_rejects_a_response_without_a_result() {
        let error = parse_sent(r#"{"ok":true}"#).unwrap_err();

        assert_eq!(error.to_string(), "telegram sendMessage returned no result");
    }

    #[test]
    fn parse_sent_reports_a_non_json_body() {
        let error = parse_sent("<html>502</html>").unwrap_err();

        assert_eq!(
            format!("{error:#}"),
            "decoding telegram sendMessage response: expected value at line 1 column 1"
        );
    }

    #[test]
    fn parse_edited_accepts_an_ok_response() {
        parse_edited(r#"{"ok":true,"result":true}"#).unwrap();
    }

    #[test]
    fn parse_edited_rejects_an_error_response() {
        let error =
            parse_edited(r#"{"ok":false,"description":"message to edit not found"}"#).unwrap_err();

        assert_eq!(
            error.to_string(),
            "telegram editMessageText failed: message to edit not found"
        );
    }

    #[test]
    fn telegram_runtime_streams_off_and_delivers_via_the_message_tool() {
        let runtime = TelegramRuntime::new("token");

        assert_eq!(
            runtime.capabilities(),
            ChannelCapabilities::new(StreamMode::Off, ReplyMode::MessageTool)
        );
    }

    #[test]
    fn telegram_runtime_has_a_sender_for_a_target() {
        let runtime = TelegramRuntime::new("token");
        let target = DeliveryTarget::direct(Channel::Telegram, "default", "123456");

        assert!(runtime.sender(&target).is_some());
    }
}
