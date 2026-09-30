use colored::Colorize;

use futures::StreamExt;

use anyhow::Result;

use genai::{
    Client,
    chat::{ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, StopReason, ToolCall, Usage},
};

use crate::tools::ToolRouter;
use crate::tools::files::ReadTool;

pub struct AgentEngine {
    client: Client,
    model: String,
    chat_request: ChatRequest,
    tool_router: ToolRouter,
    max_iterations: usize,
}

impl AgentEngine {
    pub fn new(client: Client, model: String, system_prompt: String) -> Self {
        AgentEngine {
            client: client,
            model: model,
            chat_request: ChatRequest::default().with_system(system_prompt),
            tool_router: ToolRouter::new(),
            max_iterations: 100,
        }
    }

    pub fn register_buildin_tools(&mut self) -> Result<()> {
        self.tool_router.register(ReadTool)?;

        self.chat_request = self
            .chat_request
            .clone()
            .with_tools(self.tool_router.declarations());

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

            let mut tool_calls: Vec<ToolCall> = [].to_vec();
            let mut captured_thoughts: Option<Vec<String>> = None;
            // let mut captured_reasoning: Option<String> = None;
            // let mut contents: Option<Vec<String>> = None;
            let mut usage: Option<Usage> = None;
            let mut reasoning_start = false;
            let mut stop_reason: Option<StopReason> = None;

            while let Some(result) = chat_stream.stream.next().await {
                match result? {
                    ChatStreamEvent::Start => {
                        println!("Stream started");
                    }
                    ChatStreamEvent::Chunk(chunk) => {
                        if reasoning_start {
                            println!("</think>");
                            reasoning_start = false;
                        }
                        print!("{}", chunk.content);
                    }

                    ChatStreamEvent::ReasoningChunk(chunk) => {
                        if !reasoning_start {
                            println!("<think>");
                            reasoning_start = true;
                        }
                        print!("{}", chunk.content.truecolor(128, 128, 128));
                    }
                    ChatStreamEvent::End(end) => {
                        println!("\nStream ended");

                        // captured_reasoning = end.captured_reasoning_content;
                        usage = end.captured_usage;
                        stop_reason = end.captured_stop_reason;

                        if let Some(content) = end.captured_content {
                            let parts = content.into_parts();
                            let mut extracted_tool_calls = Vec::new();
                            let mut extracted_thoughts = Vec::new();
                            let mut extracted_content = Vec::new();

                            for part in parts {
                                match part {
                                    genai::chat::ContentPart::ToolCall(tc) => {
                                        extracted_tool_calls.push(tc)
                                    }
                                    genai::chat::ContentPart::ThoughtSignature(t) => {
                                        extracted_thoughts.push(t)
                                    }
                                    genai::chat::ContentPart::Text(text) => {
                                        extracted_content.push(text);
                                    }
                                    _ => {}
                                }
                            }

                            if !extracted_tool_calls.is_empty() {
                                tool_calls = extracted_tool_calls;
                            }

                            if !extracted_thoughts.is_empty() {
                                captured_thoughts = Some(extracted_thoughts);
                            }

                            // if !extracted_content.is_empty() {
                            //     contents = Some(extracted_content);
                            // }
                        }
                    }
                    _ => {}
                }
            }

            if let Some(stop_reason) = stop_reason {
                match stop_reason {
                    StopReason::ToolCall(_) => {
                        let tool_responses = self.tool_router.dispatch_all(&tool_calls).await;
                        let mut assistant_msg = ChatMessage::from(tool_calls);
                        if let Some(thoughts) = captured_thoughts {
                            let mut parts = assistant_msg.content.into_parts();
                            for thought in thoughts.into_iter().rev() {
                                parts
                                    .insert(0, genai::chat::ContentPart::ThoughtSignature(thought));
                            }
                            assistant_msg.content = genai::chat::MessageContent::from_parts(parts);
                        }

                        chat_req = chat_req
                            .append_message(assistant_msg)
                            .append_messages(tool_responses);
                    }
                    _ => {
                        if let Some(usage) = usage {
                            println!(
                                "Input: {}, Output: {}, Total: {}",
                                usage.prompt_tokens.unwrap_or(-1),
                                usage.completion_tokens.unwrap_or(-1),
                                usage.total_tokens.unwrap_or(-1)
                            );
                        }
                        break;
                    }
                }
            }
        }

        self.chat_request = chat_req;

        Ok(())
    }
}
