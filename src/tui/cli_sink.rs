use std::sync::Mutex;

use genai::chat::{StopReason, ToolCall, ToolResponse, Usage};

use crate::output::OutputSink;
use crate::tui::render::{StreamRenderer, render_stop_reason, render_usage};

pub struct CliSink {
    renderer: Mutex<StreamRenderer>,
}

impl CliSink {
    pub fn new() -> Self {
        Self {
            renderer: Mutex::new(StreamRenderer::new()),
        }
    }
}

impl Default for CliSink {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputSink for CliSink {
    fn on_content(&self, chunk: &str) {
        self.renderer.lock().unwrap().render_content(chunk);
    }

    fn on_reasoning(&self, chunk: &str) {
        self.renderer.lock().unwrap().render_think(chunk);
    }

    fn on_tool_calls(&self, calls: &[ToolCall], responses: &[ToolResponse]) {
        self.renderer
            .lock()
            .unwrap()
            .render_tool_calls(&calls.to_vec(), &responses.to_vec());
    }

    fn on_usage(&self, usage: &Usage, iteration: usize) {
        render_usage(usage.clone(), iteration);
    }

    fn on_stop_reason(&self, reason: &Option<StopReason>) {
        render_stop_reason(reason);
    }

    fn finish(&self) {
        self.renderer.lock().unwrap().finish();
    }
}
