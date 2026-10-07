use std::sync::Arc;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use crate::channel::sender::ChannelSender;
use crate::permissions::ToolGroup;

use super::Tool;

#[derive(Deserialize)]
pub struct MessageToolArgs {
    action: String,
    text: Option<String>,
    message_id: Option<String>,
}

pub struct MessageTool {
    sender: Arc<dyn ChannelSender>,
}

impl MessageTool {
    pub fn new(sender: Arc<dyn ChannelSender>) -> Self {
        Self { sender }
    }
}

impl Tool for MessageTool {
    type Args = MessageToolArgs;
    const NAME: &'static str = "message";
    const GROUP: ToolGroup = ToolGroup::Outbound;

    fn description(&self) -> &str {
        "Send or edit a message in the current conversation. `action=\"send\"` posts `text`; `action=\"edit\"` replaces the `text` of the `message_id` returned by an earlier send. The destination is fixed to the conversation this turn came from; you cannot target another chat."
    }

    fn system_description(&self) -> &str {
        "Reply to the user by calling the message tool; on this channel the assistant's plain output is a private draft and is never delivered on its own."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
          "type": "object",
          "properties": {
            "action": {
              "type": "string",
              "enum": ["send", "edit"],
              "description": "`send` posts a new message; `edit` updates a message you already sent."
            },
            "text": {
              "type": "string",
              "description": "Message body. Required for both actions and must be non-empty."
            },
            "message_id": {
              "type": "string",
              "description": "Id returned by an earlier `send`. Required for `edit`, ignored for `send`."
            }
          },
          "required": ["action", "text"]
        })
    }

    fn call<'a>(&'a self, args: Self::Args) -> super::BoxedFuture<'a, Result<String>> {
        Box::pin(async move {
            let text = args.text.unwrap_or_default();
            if text.is_empty() {
                return Err(anyhow!("text is empty"));
            }

            match args.action.as_str() {
                "send" => {
                    let sent = self.sender.send_message(&text).await?;
                    Ok(format!("sent message {}", sent.id))
                }
                "edit" => {
                    let message_id = args
                        .message_id
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| anyhow!("message_id is required for action `edit`"))?;
                    self.sender.edit_message(&message_id, &text).await?;
                    Ok(format!("edited message {message_id}"))
                }
                other => Err(anyhow!(
                    "unknown action `{other}` (expected `send` or `edit`)"
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use genai::chat::{ToolCall, ToolResponse};
    use serde_json::json;

    use super::*;
    use crate::channel::sender::testing::MockChannelSender;
    use crate::tools::ToolRouter;

    fn router(sender: Arc<MockChannelSender>) -> ToolRouter {
        let mut router = ToolRouter::new();
        router.register(MessageTool::new(sender)).unwrap();
        router
    }

    async fn dispatch(sender: &Arc<MockChannelSender>, args: serde_json::Value) -> ToolResponse {
        let router = router(sender.clone());
        router
            .dispatch(&ToolCall {
                call_id: "call_1".to_string(),
                fn_name: MessageTool::NAME.to_string(),
                fn_arguments: args,
                thought_signatures: None,
            })
            .await
    }

    #[tokio::test]
    async fn send_delivers_through_the_sender() {
        let sender = MockChannelSender::new();

        let response = dispatch(&sender, json!({"action": "send", "text": "hello there"})).await;

        assert_eq!(response.content, "sent message msg_1");
        assert_eq!(sender.sent().len(), 1);
        assert_eq!(sender.sent()[0].id, "msg_1");
        assert_eq!(sender.sent()[0].text, "hello there");
    }

    #[tokio::test]
    async fn send_returns_the_platform_id_for_reuse() {
        let sender = MockChannelSender::new();

        let first = dispatch(&sender, json!({"action": "send", "text": "one"})).await;
        let second = dispatch(&sender, json!({"action": "send", "text": "two"})).await;

        assert_eq!(first.content, "sent message msg_1");
        assert_eq!(second.content, "sent message msg_2");
        let texts: Vec<String> = sender.sent().into_iter().map(|m| m.text).collect();
        assert_eq!(texts, vec!["one".to_string(), "two".to_string()]);
    }

    #[tokio::test]
    async fn edit_updates_an_existing_message() {
        let sender = MockChannelSender::new();

        let response = dispatch(
            &sender,
            json!({"action": "edit", "message_id": "msg_9", "text": "rewritten"}),
        )
        .await;

        assert_eq!(response.content, "edited message msg_9");
        assert_eq!(
            sender.edits(),
            vec![("msg_9".to_string(), "rewritten".to_string())]
        );
        assert!(sender.sent().is_empty());
    }

    #[tokio::test]
    async fn empty_text_is_rejected_before_reaching_the_sender() {
        let sender = MockChannelSender::new();

        let response = dispatch(&sender, json!({"action": "send", "text": ""})).await;

        assert_eq!(
            response.content,
            r#"{"error":"execution failed: text is empty","tool":"message"}"#
        );
        assert!(sender.sent().is_empty());
    }

    #[tokio::test]
    async fn edit_without_message_id_is_rejected() {
        let sender = MockChannelSender::new();

        let response = dispatch(&sender, json!({"action": "edit", "text": "x"})).await;

        assert_eq!(
            response.content,
            r#"{"error":"execution failed: message_id is required for action `edit`","tool":"message"}"#
        );
        assert!(sender.edits().is_empty());
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let sender = MockChannelSender::new();

        let response = dispatch(&sender, json!({"action": "delete", "text": "x"})).await;

        assert_eq!(
            response.content,
            r#"{"error":"execution failed: unknown action `delete` (expected `send` or `edit`)","tool":"message"}"#
        );
        assert!(sender.sent().is_empty());
    }

    #[test]
    fn declaration_exposes_the_message_tool_name() {
        let router = router(MockChannelSender::new());

        assert_eq!(router.names(), vec!["message"]);
        assert_eq!(router.declarations()[0].name.as_str(), "message");
    }
}
