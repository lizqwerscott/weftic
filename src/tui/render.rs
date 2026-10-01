use colored::Colorize;
use genai::chat::{StopReason, ToolCall, ToolResponse, Usage};

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

pub fn render_stop_reason(stop_reason: &Option<StopReason>) {
    let Some(reason) = stop_reason else {
        return;
    };

    match reason {
        StopReason::Completed(_) | StopReason::ToolCall(_) => {}
        StopReason::MaxTokens(raw) => {
            eprintln!(
                "{} response truncated by max_tokens ({}); increase max_tokens and retry",
                "⚠".yellow().bold(),
                raw.bright_black()
            );
        }
        StopReason::ContentFilter(raw) => {
            eprintln!(
                "{} response stopped by content filter ({})",
                "⚠".red().bold(),
                raw.bright_black()
            );
        }
        StopReason::StopSequence(raw) | StopReason::Other(raw) => {
            tracing::debug!(reason = %raw, "stream stop reason");
        }
    }
}

fn humanize(n: i64) -> String {
    let n = n as f64;
    let (v, unit) = if n >= 1_000_000.0 {
        (n / 1_000_000.0, "m")
    } else if n >= 1_000.0 {
        (n / 1_000.0, "k")
    } else {
        (n, "")
    };

    if v.fract() == 0.0 {
        format!("{v:.0}{unit}")
    } else {
        format!("{v:.1}{unit}")
    }
}

pub fn render_usage(usage: Usage, iterations: usize) {
    let dim = |s: &str| s.bright_black().to_string();
    let num = |n: i64| humanize(n).cyan().to_string();
    let mut segs: Vec<String> = Vec::new();

    if iterations > 1 {
        segs.push(format!("{} {}", dim("⟳"), format!("{iterations}").cyan()));
    }

    if let Some(prompt) = usage.prompt_tokens {
        let cached = usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);
        if cached > 0 {
            segs.push(format!(
                "{} {} ({} {} + {} {})",
                dim("↑"),
                num(prompt as i64),
                dim("U"),
                num((prompt - cached) as i64),
                dim("R"),
                num(cached as i64),
            ));
        } else {
            segs.push(format!("{} {}", dim("↑"), num(prompt as i64)));
        }
    }

    if let Some(out) = usage.completion_tokens {
        segs.push(format!("{} {}", dim("↓"), num(out as i64)));
    }

    // reasoning tokens (only some providers)
    if let Some(reasoning) = usage
        .completion_tokens_details
        .as_ref()
        .and_then(|d| d.reasoning_tokens)
    {
        segs.push(format!("{} {}", dim("∴"), num(reasoning as i64)));
    }

    if segs.is_empty() {
        return; // provider returned no usage
    }

    println!("{}", segs.join(&dim(" | ")));
    println!();
}
