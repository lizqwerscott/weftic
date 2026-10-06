use std::fmt;
use std::sync::Arc;

use futures::StreamExt;

use crate::{
    config::model_provider::ChatModel,
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

use crate::tools::ToolRouter;

#[derive(Default)]
struct TurnMeta {
    stop_reason: Option<StopReason>,
    usage: Option<Usage>,
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
        println!("registered tools: {:?}", self.tool_router.names());
        Ok(())
    }

    pub async fn run_turn(
        &self,
        session: &Session,
        input: ChatMessage,
        sink: &Arc<dyn OutputSink>,
    ) -> Result<SessionTurn, TurnStartError> {
        let mut steps = ChatRequest::default().append_message(input);

        let system = self
            .system_prompt_manager
            .render(
                session.get_template(),
                &self.tool_router.get_tool_system_descriptions(),
            )
            .map_err(|e| TurnStartError::Render(format!("{e:#}")))?;

        let history = session.get_history();
        let mut meta = TurnMeta::default();

        let error = self
            .drive(system, &history, &mut steps, sink, &mut meta)
            .await
            .err();

        let status = match error {
            Some(source) => TurnStatus::Failed(source),
            None => TurnStatus::Complete,
        };

        Ok(SessionTurn::new(
            &steps,
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
    ) -> Result<(), TurnError> {
        let chat_req = ChatRequest::from_system(system.clone())
            .with_tools(self.tool_router.declarations())
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

            let mut assistant_msg = match assistant_content.take() {
                Some(content) => ChatMessage::assistant(content),
                None => ChatMessage::assistant(MessageContent::from_parts(vec![])),
            };

            assistant_msg = assistant_msg.with_reasoning_content(captured_reasoning.take());

            if !tool_calls.is_empty() {
                let tool_responses = self.tool_router.dispatch_all(&tool_calls).await;
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

            return Ok(());
        }

        Err(TurnError::MaxIterations {
            limit: self.max_iterations,
        })
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
