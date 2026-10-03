use futures::StreamExt;

use anyhow::{Result, anyhow};

use crate::{config::Config, system_prompt::SystemPrompt};

use genai::{
    Client,
    chat::{
        ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, MessageContent, StopReason,
        ToolCall, Usage,
    },
};

use crate::tui::render::{StreamRenderer, render_stop_reason};
use crate::{tools::ToolRouter, tui::render::render_usage};

pub struct AgentEngine {
    client: Client,
    model: String,
    chat_request: ChatRequest,
    tool_router: ToolRouter,
    max_iterations: usize,
    stream_render: StreamRenderer,
    system_prompt: SystemPrompt,
}

impl AgentEngine {
    pub fn new(client: Client, model: String, config: &Config) -> Result<Self> {
        let mut tool_router = ToolRouter::new();

        tool_router.register_builtin_tools()?;

        let system_prompt = SystemPrompt::build(
            &config.system_prompt,
            &tool_router.get_tool_system_descriptions(),
        )?;

        Ok(AgentEngine {
            client,
            model,
            chat_request: ChatRequest::default(),
            max_iterations: 100,
            tool_router,
            stream_render: StreamRenderer::new(),
            system_prompt,
        })
    }

    pub fn init(&mut self) -> Result<()> {
        self.chat_request = self
            .chat_request
            .clone()
            .with_tools(self.tool_router.declarations());

        println!("registered tools: {:?}", self.tool_router.names());
        let Some(system_prompt) = self.system_prompt.render_system_prompt("agent") else {
            return Err(anyhow!("agent prompt not found"));
        };
        self.chat_request = self.chat_request.clone().with_system(system_prompt);

        Ok(())
    }

    pub async fn run_turn(&mut self, input: String) -> Result<()> {
        let mut chat_req = self
            .chat_request
            .clone()
            .append_message(ChatMessage::user(input));

        let chat_options = ChatOptions::default()
            .with_capture_tool_calls(true)
            .with_capture_usage(true)
            .with_capture_reasoning_content(true)
            .with_capture_content(true);

        let mut iterations = 0;

        for iteration in 1..=self.max_iterations {
            let mut chat_stream = self
                .client
                .exec_chat_stream(&self.model, chat_req.clone(), Some(&chat_options))
                .await?;

            let mut captured_reasoning: Option<String> = None;
            let mut assistant_content: Option<MessageContent> = None;
            let mut usage: Option<Usage> = None;
            let mut stop_reason: Option<StopReason> = None;

            while let Some(result) = chat_stream.stream.next().await {
                match result? {
                    ChatStreamEvent::Start => {}
                    ChatStreamEvent::Chunk(chunk) => {
                        self.stream_render.render_content(&chunk.content);
                    }

                    ChatStreamEvent::ReasoningChunk(chunk) => {
                        self.stream_render.render_think(&chunk.content);
                    }
                    ChatStreamEvent::End(end) => {
                        self.stream_render.finish();
                        captured_reasoning = end.captured_reasoning_content;
                        assistant_content = end.captured_content;
                        stop_reason = end.captured_stop_reason;

                        usage = end.captured_usage;
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
                self.stream_render
                    .render_tool_calls(&tool_calls, &tool_responses);

                chat_req = chat_req
                    .append_message(assistant_msg)
                    .append_messages(tool_responses);
                continue;
            }

            render_stop_reason(&stop_reason);

            if !has_text {
                self.chat_request = chat_req;

                if matches!(
                    stop_reason,
                    Some(StopReason::ContentFilter(_)) | Some(StopReason::MaxTokens(_))
                ) {
                    return Ok(());
                }

                return Err(anyhow!("empty assistant response (no text, no tool calls)"));
            }

            if let Some(usage) = usage {
                render_usage(usage, iteration);
            }

            chat_req = chat_req.append_message(assistant_msg);
            iterations = iteration;
            break;
        }

        self.chat_request = chat_req;

        if iterations == 0 {
            return Err(anyhow!(
                "agent did not finish within {} iterations",
                self.max_iterations
            ));
        }

        Ok(())
    }
}
