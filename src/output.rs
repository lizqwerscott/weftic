use genai::chat::{StopReason, ToolCall, ToolResponse, Usage};

pub trait OutputSink: Send + Sync {
    fn on_content(&self, _chunk: &str) {}

    fn on_reasoning(&self, _chunk: &str) {}

    fn on_tool_calls(&self, _calls: &[ToolCall], _responses: &[ToolResponse]) {}

    fn on_usage(&self, _usage: &Usage, _iteration: usize) {}

    fn on_stop_reason(&self, _reason: &Option<StopReason>) {}

    fn finish(&self) {}
}

pub struct NullSink;

impl OutputSink for NullSink {}
