//! Telegram outbound: rich messages, with a plain-text fallback.

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
    max_attempts: u32,
}

impl TelegramSender {
    pub fn new(token: impl Into<String>, chat_id: impl Into<String>, max_attempts: u32) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.into(),
            chat_id: chat_id.into(),
            max_attempts: max_attempts.max(1),
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{API_BASE}/bot{}/{}", self.token, method)
    }

    async fn post(&self, method: &str, body: &serde_json::Value) -> Result<String> {
        self.client
            .post(self.url(method))
            .json(body)
            .send()
            .await
            .map_err(|error| anyhow!("calling telegram {method}: {}", error.without_url()))?
            .text()
            .await
            .map_err(|error| {
                anyhow!(
                    "reading telegram {method} response: {}",
                    error.without_url()
                )
            })
    }

    async fn send_once(&self, text: &str) -> Result<SentMessage, AttemptError> {
        match self
            .post("sendRichMessage", &rich_body(&self.chat_id, text))
            .await
        {
            Ok(response) => match parse_sent(&response) {
                Ok(sent) => {
                    tracing::info!(target: "telegram", "sent rich message {}", sent.id);
                    return Ok(sent);
                }
                Err(error) => {
                    tracing::warn!(target: "telegram", "rich send rejected, falling back: {error:#}");
                }
            },
            Err(error) => {
                tracing::warn!(target: "telegram", "rich send failed, falling back: {error:#}");
            }
        }

        let response = self
            .post("sendMessage", &plain_body(&self.chat_id, text))
            .await
            .map_err(AttemptError::transport)?;
        let sent = parse_sent(&response).map_err(AttemptError::rejected)?;
        tracing::info!(target: "telegram", "sent message {}", sent.id);
        Ok(sent)
    }

    async fn edit_once(&self, message_id: &str, text: &str) -> Result<(), AttemptError> {
        let rich = rich_edit_body(&self.chat_id, message_id, text);
        match self.post("editMessageText", &rich).await {
            Ok(response) => match parse_edited(&response) {
                Ok(()) => {
                    tracing::info!(target: "telegram", "edited rich message {message_id}");
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(target: "telegram", "rich edit rejected, falling back: {error:#}");
                }
            },
            Err(error) => {
                tracing::warn!(target: "telegram", "rich edit failed, falling back: {error:#}");
            }
        }

        let plain = plain_edit_body(&self.chat_id, message_id, text);
        let response = self
            .post("editMessageText", &plain)
            .await
            .map_err(AttemptError::transport)?;
        parse_edited(&response).map_err(AttemptError::rejected)?;
        tracing::info!(target: "telegram", "edited message {message_id}");
        Ok(())
    }
}

impl ChannelSender for TelegramSender {
    fn send_message<'a>(&'a self, text: &'a str) -> BoxedSendFuture<'a, Result<SentMessage>> {
        Box::pin(async move {
            let mut attempt = 0;

            loop {
                attempt += 1;
                match self.send_once(text).await {
                    Ok(sent) => return Ok(sent),
                    Err(error) => {
                        tracing::warn!(
                            target: "telegram",
                            "send attempt {attempt}/{} failed: {error}",
                            self.max_attempts
                        );
                        if !error.retryable || attempt >= self.max_attempts {
                            return Err(error.source);
                        }
                    }
                }
            }
        })
    }

    fn edit_message<'a>(
        &'a self,
        message_id: &'a str,
        text: &'a str,
    ) -> BoxedSendFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut attempt = 0;

            loop {
                attempt += 1;
                match self.edit_once(message_id, text).await {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        tracing::warn!(
                            target: "telegram",
                            "edit attempt {attempt}/{} failed: {error}",
                            self.max_attempts
                        );
                        if !error.retryable || attempt >= self.max_attempts {
                            return Err(error.source);
                        }
                    }
                }
            }
        })
    }
}

pub struct TelegramRuntime {
    token: String,
    max_attempts: u32,
}

impl TelegramRuntime {
    pub fn new(token: impl Into<String>, max_attempts: u32) -> Self {
        Self {
            token: token.into(),
            max_attempts,
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
            self.max_attempts,
        )))
    }
}

/// A failed delivery attempt: `retryable` marks transport-level failures (the
/// request never got an answer), which are worth retrying.
struct AttemptError {
    retryable: bool,
    source: anyhow::Error,
}

impl AttemptError {
    fn transport(source: anyhow::Error) -> Self {
        Self {
            retryable: true,
            source,
        }
    }

    fn rejected(source: anyhow::Error) -> Self {
        Self {
            retryable: false,
            source,
        }
    }
}

impl std::fmt::Display for AttemptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.source)
    }
}

fn rich_body(chat_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "chat_id": chat_id, "rich_message": { "markdown": text } })
}

fn plain_body(chat_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "chat_id": chat_id, "text": text })
}

fn rich_edit_body(chat_id: &str, message_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "rich_message": { "markdown": text },
    })
}

fn plain_edit_body(chat_id: &str, message_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "chat_id": chat_id, "message_id": message_id, "text": text })
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
        let runtime = TelegramRuntime::new("token", 3);

        assert_eq!(
            runtime.capabilities(),
            ChannelCapabilities::new(StreamMode::Off, ReplyMode::MessageTool)
        );
    }

    #[test]
    fn telegram_runtime_has_a_sender_for_a_target() {
        let runtime = TelegramRuntime::new("token", 3);
        let target = DeliveryTarget::direct(Channel::Telegram, "default", "123456");

        assert!(runtime.sender(&target).is_some());
    }

    #[test]
    fn rich_body_wraps_the_text_as_markdown() {
        assert_eq!(
            rich_body("123", "**hi**"),
            serde_json::json!({"chat_id": "123", "rich_message": {"markdown": "**hi**"}})
        );
    }

    #[test]
    fn plain_body_sends_the_text_as_is() {
        assert_eq!(
            plain_body("123", "**hi**"),
            serde_json::json!({"chat_id": "123", "text": "**hi**"})
        );
    }

    #[test]
    fn rich_edit_body_targets_a_message() {
        assert_eq!(
            rich_edit_body("123", "42", "**hi**"),
            serde_json::json!({
                "chat_id": "123",
                "message_id": "42",
                "rich_message": {"markdown": "**hi**"},
            })
        );
    }

    #[test]
    fn plain_edit_body_targets_a_message() {
        assert_eq!(
            plain_edit_body("123", "42", "**hi**"),
            serde_json::json!({"chat_id": "123", "message_id": "42", "text": "**hi**"})
        );
    }

    #[test]
    fn transport_failures_are_retryable() {
        let error = AttemptError::transport(anyhow::anyhow!("connection reset"));

        assert!(error.retryable);
        assert_eq!(error.to_string(), "connection reset");
    }

    #[test]
    fn api_rejections_are_not_retryable() {
        let error = AttemptError::rejected(anyhow::anyhow!(
            "telegram sendMessage failed: chat not found"
        ));

        assert!(!error.retryable);
        assert_eq!(
            error.to_string(),
            "telegram sendMessage failed: chat not found"
        );
    }
}
