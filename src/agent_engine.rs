use futures::StreamExt;

use anyhow::Result;

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
}

impl AgentEngine {
    pub fn new(client: Client, model: String, system_prompt: String) -> Self {
        AgentEngine {
            client: client,
            model: model,
            chat_request: ChatRequest::default().with_system(system_prompt),
            tool_router: ToolRouter::new(),
            max_iterations: 100,
            stream_render: StreamRenderer::new(),
        }
    }

    pub fn register_buildin_tools(&mut self) -> Result<()> {
        self.tool_router.register_buildin_tools()?;

        self.chat_request = self
            .chat_request
            .clone()
            .with_tools(self.tool_router.declarations());

        println!("registered tools: {:?}", self.tool_router.names());

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

        for _ in 1..=self.max_iterations {
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

            let mut assistant_msg = match assistant_content.take() {
                Some(content) => ChatMessage::assistant(content),
                None => ChatMessage::assistant(MessageContent::from_parts(vec![])),
            };

            assistant_msg = assistant_msg.with_reasoning_content(captured_reasoning.take());

            chat_req = chat_req.append_message(assistant_msg);

            if !tool_calls.is_empty() {
                let tool_responses = self.tool_router.dispatch_all(&tool_calls).await;
                self.stream_render
                    .render_tool_calls(&tool_calls, &tool_responses);

                chat_req = chat_req.append_messages(tool_responses);
            } else {
                render_stop_reason(stop_reason);

                if let Some(usage) = usage {
                    render_usage(usage);
                }
                break;
            }
        }

        self.chat_request = chat_req;

        Ok(())
    }
}
