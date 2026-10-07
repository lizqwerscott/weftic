use std::fmt;
use std::sync::Arc;

use futures::StreamExt;

use crate::{
    config::model_provider::ChatModel,
    event::EventId,
    output::OutputSink,
    session::{
        Session,
        history::{SessionTurn, TurnError, TurnStatus},
    },
    system_prompt::SystemPromptManager,
};

use genai::chat::{
    ChatMessage, ChatRequest, ChatStreamEvent, MessageContent, StopReason, ToolCall, Usage,
};
use tracing::{error, info};

use crate::channel::capabilities::ReplyMode;
use crate::permissions::allowed_groups;
use crate::tools::Tool;
use crate::tools::ToolRouter;
use crate::tools::message::MessageTool;

#[derive(Default)]
struct TurnMeta {
    stop_reason: Option<StopReason>,
    usage: Option<Usage>,
    /// Whether the outbound `message` tool ran this turn.
    delivered: bool,
    /// The final assistant text, when the turn ended with any.
    final_text: Option<String>,
}

#[derive(Debug, Clone)]
pub enum TurnStartError {
    Render(String),
}

impl fmt::Display for TurnStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TurnStartError::Render(message) => write!(f, "system prompt render failed: {message}"),
        }
    }
}

impl std::error::Error for TurnStartError {}

pub struct AgentEngine {
    model: Arc<dyn ChatModel>,
    tool_router: ToolRouter,
    max_iterations: usize,
    system_prompt_manager: SystemPromptManager,
}

impl AgentEngine {
    pub fn new(
        model: Arc<dyn ChatModel>,
        tool_router: ToolRouter,
        system_prompt_manager: SystemPromptManager,
        max_iterations: usize,
    ) -> Self {
        AgentEngine {
            model,
            max_iterations,
            tool_router,
            system_prompt_manager,
        }
    }

    pub fn init(&self) -> anyhow::Result<()> {
        info!("registered tools: {:?}", self.tool_router.names());
        Ok(())
    }

    pub async fn run_turn(
        &self,
        session: &Session,
        input: ChatMessage,
        source_event_id: Option<EventId>,
        sink: &Arc<dyn OutputSink>,
    ) -> Result<SessionTurn, TurnStartError> {
        let router = self.tool_router.for_session(
            &allowed_groups(session.mode(), session.role()),
            session.binding().capabilities.reply,
            session.binding().sender.clone(),
        );

        let mut steps = ChatRequest::default().append_message(input);

        let system = self
            .system_prompt_manager
            .render(
                session.get_template(),
                &router.get_tool_system_descriptions(),
            )
            .map_err(|e| TurnStartError::Render(format!("{e:#}")))?;

        let history = session.get_history();
        let mut meta = TurnMeta::default();

        let error = self
            .drive(system, &history, &mut steps, sink, &mut meta, &router)
            .await
            .err();

        let status = match error {
            Some(source) => TurnStatus::Failed(source),
            None => TurnStatus::Complete,
        };

        if matches!(status, TurnStatus::Complete) {
            deliver_fallback(session, &meta).await;
        }

        Ok(SessionTurn::new(
            &steps,
            source_event_id,
            status,
            meta.stop_reason,
            meta.usage,
        ))
    }

    async fn drive(
        &self,
        system: String,
        history: &ChatRequest,
        steps: &mut ChatRequest,
        sink: &Arc<dyn OutputSink>,
        meta: &mut TurnMeta,
        router: &ToolRouter,
    ) -> Result<(), TurnError> {
        let chat_req = ChatRequest::from_system(system.clone())
            .with_tools(router.declarations())
            .append_messages(history.messages.clone());

        for iteration in 1..=self.max_iterations {
            let chat_req = chat_req.clone().append_messages(steps.messages.clone());

            let mut chat_stream = self
                .model
                .stream_chat(chat_req)
                .await
                .map_err(|e| TurnError::Request(format!("{e:#}")))?;

            let mut captured_reasoning: Option<String> = None;
            let mut assistant_content: Option<MessageContent> = None;

            while let Some(result) = chat_stream.next().await {
                match result.map_err(|e| TurnError::Stream(format!("{e:#}")))? {
                    ChatStreamEvent::Start => {}
                    ChatStreamEvent::Chunk(chunk) => {
                        sink.on_content(&chunk.content);
                    }

                    ChatStreamEvent::ReasoningChunk(chunk) => {
                        sink.on_reasoning(&chunk.content);
                    }
                    ChatStreamEvent::End(end) => {
                        sink.finish();
                        captured_reasoning = end.captured_reasoning_content;
                        assistant_content = end.captured_content;

                        meta.stop_reason = end.captured_stop_reason;
                        meta.usage = end.captured_usage;
                    }
                    _ => {}
                }
            }

            let tool_calls: Vec<ToolCall> = assistant_content
                .as_ref()
                .map(|c| c.tool_calls().into_iter().cloned().collect())
                .unwrap_or_default();

            let has_text = assistant_content
                .as_ref()
                .is_some_and(|c| !c.texts().is_empty());

            let final_text = assistant_content
                .as_ref()
                .map(|content| content.texts().join("\n"));

            let mut assistant_msg = match assistant_content.take() {
                Some(content) => ChatMessage::assistant(content),
                None => ChatMessage::assistant(MessageContent::from_parts(vec![])),
            };

            assistant_msg = assistant_msg.with_reasoning_content(captured_reasoning.take());

            if !tool_calls.is_empty() {
                if tool_calls
                    .iter()
                    .any(|call| call.fn_name == MessageTool::NAME)
                {
                    meta.delivered = true;
                }

                let tool_responses = router.dispatch_all(&tool_calls).await;
                sink.on_tool_calls(&tool_calls, &tool_responses);

                let current = std::mem::take(steps);
                *steps = current
                    .append_message(assistant_msg)
                    .append_messages(tool_responses);

                continue;
            }

            sink.on_stop_reason(&meta.stop_reason);

            if !has_text {
                if matches!(
                    meta.stop_reason,
                    Some(StopReason::ContentFilter(_)) | Some(StopReason::MaxTokens(_))
                ) {
                    return Ok(());
                }

                return Err(TurnError::EmptyResponse);
            }

            if let Some(usage) = &meta.usage {
                sink.on_usage(usage, iteration);
            }

            let current = std::mem::take(steps);
            *steps = current.append_message(assistant_msg);

            meta.final_text = final_text;

            return Ok(());
        }

        Err(TurnError::MaxIterations {
            limit: self.max_iterations,
        })
    }
}

async fn deliver_fallback(session: &Session, meta: &TurnMeta) {
    if meta.delivered || session.binding().capabilities.reply != ReplyMode::MessageTool {
        return;
    }

    let Some(sender) = &session.binding().sender else {
        return;
    };

    let Some(text) = meta.final_text.as_deref() else {
        return;
    };

    if text.trim().is_empty() {
        return;
    }

    match sender.send_message(text).await {
        Ok(sent) => info!(target: "outbound", "fallback delivered message {}", sent.id),
        Err(error) => error!(target: "outbound", "fallback delivery failed: {error:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_error_renders_its_message() {
        let error = TurnStartError::Render("template `cli` not found".to_string());
        assert_eq!(
            error.to_string(),
            "system prompt render failed: template `cli` not found"
        );
    }
}
