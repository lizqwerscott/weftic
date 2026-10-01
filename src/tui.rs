use colored::Colorize;
use genai::chat::{ToolCall, ToolResponse};

pub struct StreamRenderer {
    in_think: bool,
    line_buf: String,
    content_started: bool,
}

impl StreamRenderer {
    pub fn new() -> Self {
        Self {
            in_think: true,
            line_buf: String::new(),
            content_started: false,
        }
    }

    pub fn render_think(&mut self, chunk: &str) {
        if !self.in_think {
            self.flush_line();
            self.in_think = true;
        }
        self.append(chunk);
    }

    pub fn render_content(&mut self, chunk: &str) {
        if self.in_think {
            self.flush_line();
            self.in_think = false;
            println!();
        }
        self.append(chunk);
    }

    pub fn render_tool_calls(
        &mut self,
        tool_calls: &Vec<ToolCall>,
        tool_responses: &Vec<ToolResponse>,
    ) {
        const MAX_LINES: usize = 5;
        for (tool_call, tool_res) in tool_calls.iter().zip(tool_responses.iter()) {
            println!(
                "{} {} {}",
                "Tool".yellow().bold(),
                tool_call.fn_name.yellow().bold(),
                tool_call.fn_arguments.to_string().cyan()
            );
            println!();
            let total = tool_res.content.lines().count();
            for line in tool_res.content.lines().take(MAX_LINES) {
                if line.is_empty() {
                    println!("{}", "▏".bright_black());
                } else {
                    println!("{} {}", "▏".bright_black(), line);
                }
            }

            if total > MAX_LINES {
                println!(
                    "{}",
                    format!("▏ ... {} line", total - MAX_LINES).bright_black()
                );
            };
        }
    }

    fn append(&mut self, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                self.flush_line();
            } else {
                self.line_buf.push(ch);
            }
        }
    }

    fn flush_line(&mut self) {
        let line = std::mem::take(&mut self.line_buf);

        if self.in_think {
            if line.is_empty() {
                return;
            }
            println!(" {}", line.truecolor(128, 128, 128).italic());
        } else {
            if line.is_empty() {
                println!();
                return;
            }

            if !self.content_started {
                println!(" {}", line);
                self.content_started = true;
            } else {
                println!(" {}", line);
            }
        }
    }

    pub fn finish(&mut self) {
        if !self.line_buf.is_empty() {
            self.flush_line();
        }

        self.in_think = false;
        self.content_started = false;
        println!();
    }
}
