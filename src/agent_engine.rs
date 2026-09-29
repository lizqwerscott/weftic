use futures::StreamExt;

use anyhow::{Result, anyhow};

use genai::{
    Client, ClientBuilder,
    chat::{ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, Tool, ToolCall},
};

pub struct AgentEngine {
    client: Client,
    model: String,
    chat_request: ChatRequest,
}

impl AgentEngine {
    pub fn new(client: Client, model: String, system_prompt: String) -> Self {
        AgentEngine {
            client: client,
            model: model,
            chat_request: ChatRequest::default().with_system(system_prompt),
        }
    }

    pub async fn run_turn(&mut self, input: String) -> Result<()> {
        let chat_req = self
            .chat_request
            .clone()
            .append_message(ChatMessage::user(input));

        let chat_options = ChatOptions::default()
            .with_capture_tool_calls(true)
            .with_capture_usage(true)
            .with_capture_content(true);

        let mut chat_stream = self
            .client
            .exec_chat_stream(&self.model, chat_req, Some(&chat_options))
            .await?;

        let mut tool_calls: Vec<ToolCall> = [].to_vec();
        let mut captured_thoughts: Option<Vec<String>> = None;
        let mut contents: Option<Vec<String>> = None;

        while let Some(result) = chat_stream.stream.next().await {
            match result? {
                ChatStreamEvent::Start => {
                    println!("Stream started");
                }
                ChatStreamEvent::Chunk(chunk) => {
                    print!("{}", chunk.content);
                }
                ChatStreamEvent::ToolCallChunk(chunk) => {
                    println!("  ToolCallChunk: {:?}", chunk.tool_call);
                }
                ChatStreamEvent::ReasoningChunk(chunk) => {
                    println!("  ReasoningChunk: {:?}", chunk.content);
                }
                ChatStreamEvent::ThoughtSignatureChunk(chunk) => {
                    println!("  ThoughtSignatureChunk: {:?}", chunk.content);
                }
                ChatStreamEvent::End(end) => {
                    println!("\nStream ended");

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

                        if !extracted_content.is_empty() {
                            contents = Some(extracted_content);
                        }
                    }
                }
            }
        }

        if tool_calls.is_empty() {
            return Ok(());
        }

        // call tool call

        // send tool call result
        Ok(())
    }
}
