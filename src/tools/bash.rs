use std::{
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use serde::Deserialize;

use anyhow::{Result, anyhow};
use serde_json::json;
use tokio::process::Command;

use super::Tool;

const BASH_DEFAULT_TIMEOUT_MS: u64 = 120_000;
const BASH_MAX_TIMEOUT_MS: u64 = 600_000;
const BASH_MAX_OUTPUT_BYTES: usize = 64_000;

#[derive(Deserialize)]
pub struct BashToolArgs {
    description: String,
    command: String,
    workdir: String,
    timeout_ms: Option<u64>,
}

pub struct BashTool;

impl Tool for BashTool {
    type Args = BashToolArgs;
    const NAME: &'static str = "bash";

    fn description(&self) -> &str {
        "Execute a bash command (`bash -c`) and return its stdout/stderr. Each call runs in a fresh shell; pass `workdir` instead of using `cd`. Long output is truncated to its tail. Provide `description` before `command` in the arguments. Before any delete or move, verify that the resolved absolute target path is the intended one; never run it against a computed path you have not checked. An unset variable expands to an empty string, so guard variables in such paths with `${VAR:?}`."
    }

    fn system_description(&self) -> &str {
        "Check the [exit code: N] marker on every bash result; investigate failures before moving on."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
          "type": "object",
          "properties": {
            "description": {
              "type": "string",
              "description": "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI). Examples: \"ls\" → \"List files in current directory\"; \"git status\" → \"Show working tree status\"; \"npm install\" → \"Install package dependencies\"."
            },
            "command": {
              "type": "string",
              "description": "The bash command to execute."
            },
            "timeout_ms": {
              "type": "number",
              "description": "Timeout in milliseconds for this command; defaults to the tool's built-in limit. The command is killed on expiry."
            },
            "workdir": {
              "type": "string",
              "description": "Absolute working directory for this command."
            },
          },
          "required": [
            "description",
            "command",
            "workdir"
          ]
        })
    }

    fn call<'a>(&'a self, args: Self::Args) -> super::BoxedFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { bash(args).await })
    }
}

async fn bash(args: BashToolArgs) -> Result<String> {
    if args.description.is_empty() {
        return Err(anyhow!("description is empty"));
    }

    if args.command.is_empty() {
        return Err(anyhow!("command is empty"));
    }

    if args.workdir.is_empty() {
        return Err(anyhow!("workdir is empty"));
    }

    let path = Path::new(&args.workdir);

    if !path.is_absolute() {
        return Err(anyhow!(
            "workdir must be an absolute path, got: {}",
            path.display()
        ));
    }

    if !path.exists() {
        return Err(anyhow!("{} does not exist", path.display()));
    }

    if !path.is_dir() {
        return Err(anyhow!(
            "workdir is a file, not a directory: {}",
            path.display()
        ));
    }

    let timeout_ms = args
        .timeout_ms
        .unwrap_or(BASH_DEFAULT_TIMEOUT_MS)
        .min(BASH_MAX_TIMEOUT_MS);

    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&args.command)
        .current_dir(&args.workdir)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.spawn()?;

    match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await {
        Ok(result) => {
            let output = result?;
            Ok(render_output(
                &output.stdout,
                &output.stderr,
                Some(output.status),
                None,
            ))
        }
        Err(_) => Ok(render_output(&[], &[], None, Some(timeout_ms))),
    }
}

fn tail(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() <= max {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    } else {
        (
            String::from_utf8_lossy(&bytes[bytes.len() - max..]).into_owned(),
            true,
        )
    }
}

fn render_output(
    stdout: &[u8],
    stderr: &[u8],
    status: Option<ExitStatus>,
    timeout_ms: Option<u64>,
) -> String {
    let (out_text, out_truncated) = tail(stdout, BASH_MAX_OUTPUT_BYTES);
    let (err_text, err_truncated) = tail(stderr, BASH_MAX_OUTPUT_BYTES);

    let mut body = String::new();
    if !out_text.is_empty() {
        body.push_str(&out_text);
    }
    if !err_text.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&err_text);
    }
    if body.is_empty() {
        body.push_str("(no output)");
    }

    if out_truncated || err_truncated {
        body.push_str("\n[output truncated; full output unavailable]");
    }

    if let Some(timeout_ms) = timeout_ms {
        body.push_str(&format!("\n[timed out after {timeout_ms}ms]"));
    } else if let Some(status) = status {
        match status.code() {
            Some(0) => {}
            Some(code) => body.push_str(&format!("\n[exit code: {code}]")),
            None => {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    body.push_str(&format!("\n[killed by signal: {signal}]"));
                }
            }
        }
    }

    body
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn tail_keeps_short_output_intact() {
        let (text, truncated) = tail(b"hello", 10);
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn tail_keeps_the_end_of_long_output() {
        let (text, truncated) = tail(b"0123456789", 4);
        assert_eq!(text, "6789");
        assert!(truncated);
    }

    #[test]
    fn empty_output_reports_no_output() {
        assert_eq!(render_output(b"", b"", None, None), "(no output)");
    }

    #[test]
    fn stderr_gets_its_own_section() {
        let out = render_output(b"hi\n", b"warn\n", None, None);
        assert_eq!(out, "hi\n[stderr]\nwarn\n");
    }

    #[test]
    fn stdout_without_trailing_newline_is_separated_from_stderr() {
        let out = render_output(b"hi", b"warn", None, None);
        assert_eq!(out, "hi\n[stderr]\nwarn");
    }

    #[test]
    fn clean_exit_has_no_marker() {
        let out = render_output(b"done\n", b"", Some(ExitStatus::from_raw(0)), None);
        assert_eq!(out, "done\n");
    }

    #[test]
    fn nonzero_exit_reports_the_code() {
        let out = render_output(b"", b"", Some(ExitStatus::from_raw(3 << 8)), None);
        assert_eq!(out, "(no output)\n[exit code: 3]");
    }

    #[test]
    fn signal_kill_is_reported() {
        let out = render_output(b"", b"", Some(ExitStatus::from_raw(9)), None);
        assert_eq!(out, "(no output)\n[killed by signal: 9]");
    }

    #[test]
    fn timeout_reports_the_limit() {
        let out = render_output(b"", b"", None, Some(1500));
        assert_eq!(out, "(no output)\n[timed out after 1500ms]");
    }

    #[test]
    fn oversized_output_is_truncated_to_its_tail() {
        let stdout = vec![b'x'; BASH_MAX_OUTPUT_BYTES + 1];
        let out = render_output(&stdout, b"", None, None);
        assert_eq!(out.matches('x').count(), BASH_MAX_OUTPUT_BYTES);
        assert!(out.ends_with("[output truncated; full output unavailable]"));
    }
}
