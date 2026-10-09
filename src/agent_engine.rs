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
use crate::tools::ToolContext;
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

/// Everything one turn operates on: the rendered prompt, the history it is
/// replayed against, the accumulating steps, and the tool/runtime handles.
struct TurnState<'a> {
    system: String,
    history: &'a ChatRequest,
    steps: ChatRequest,
    sink: &'a Arc<dyn OutputSink>,
    meta: TurnMeta,
    router: &'a ToolRouter,
    ctx: &'a ToolContext,
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

        let system = self
            .system_prompt_manager
            .render(
                session.get_template(),
                &router.get_tool_system_descriptions(),
            )
            .map_err(|e| TurnStartError::Render(format!("{e:#}")))?;

        let history = session.get_history();
        let ctx = ToolContext {
            workspace: session.workspace(),
        };

        let mut state = TurnState {
            system,
            history: &history,
            steps: ChatRequest::default().append_message(input),
            sink,
            meta: TurnMeta::default(),
            router: &router,
            ctx: &ctx,
        };

        let error = self.drive(&mut state).await.err();

        let status = match error {
            Some(source) => TurnStatus::Failed(source),
            None => TurnStatus::Complete,
        };

        if matches!(status, TurnStatus::Complete) {
            deliver_fallback(session, &state.meta).await;
        }

        Ok(SessionTurn::new(
            &state.steps,
            source_event_id,
            status,
            state.meta.stop_reason,
            state.meta.usage,
        ))
    }

    async fn drive(&self, state: &mut TurnState<'_>) -> Result<(), TurnError> {
        let chat_req = ChatRequest::from_system(state.system.clone())
            .with_tools(state.router.declarations())
            .append_messages(state.history.messages.clone());

        for iteration in 1..=self.max_iterations {
            let chat_req = chat_req
                .clone()
                .append_messages(state.steps.messages.clone());

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
                        state.sink.on_content(&chunk.content);
                    }

                    ChatStreamEvent::ReasoningChunk(chunk) => {
                        state.sink.on_reasoning(&chunk.content);
                    }
                    ChatStreamEvent::End(end) => {
                        state.sink.finish();
                        captured_reasoning = end.captured_reasoning_content;
                        assistant_content = end.captured_content;

                        state.meta.stop_reason = end.captured_stop_reason;
                        state.meta.usage = end.captured_usage;
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
                    state.meta.delivered = true;
                }

                let tool_responses = state.router.dispatch_all(state.ctx, &tool_calls).await;
                state.sink.on_tool_calls(&tool_calls, &tool_responses);

                let current = std::mem::take(&mut state.steps);
                state.steps = current
                    .append_message(assistant_msg)
                    .append_messages(tool_responses);

                continue;
            }

            state.sink.on_stop_reason(&state.meta.stop_reason);

            if !has_text {
                if matches!(
                    state.meta.stop_reason,
                    Some(StopReason::ContentFilter(_)) | Some(StopReason::MaxTokens(_))
                ) {
                    return Ok(());
                }

                return Err(TurnError::EmptyResponse);
            }

            if let Some(usage) = &state.meta.usage {
                state.sink.on_usage(usage, iteration);
            }

            let current = std::mem::take(&mut state.steps);
            state.steps = current.append_message(assistant_msg);

            state.meta.final_text = final_text;

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
        let error = TurnStartError::Render("template `webui` not found".to_string());
        assert_eq!(
            error.to_string(),
            "system prompt render failed: template `webui` not found"
        );
    }
}
