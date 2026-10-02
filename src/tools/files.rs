use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::Path,
};

use serde::Deserialize;
use serde_json::json;

use anyhow::{Result, anyhow};

use crate::tools::MAX_LINE_LENGTH;

use super::{Tool, truncate_line};

// read

const READ_MAX_BYTES: usize = 50 * 1024;
const READ_MAX_LINES: usize = 2000;

enum ReadStopReason {
    EndOfFile,
    LineLimit { last_line: usize },
    ByteLimit { last_line: usize },
}

#[derive(Deserialize)]
pub struct ReadToolArgs {
    file_path: String,
    offset: Option<u64>,
    limit: Option<u64>,
}

pub struct ReadTool;

impl Tool for ReadTool {
    type Args = ReadToolArgs;
    const NAME: &'static str = "read";

    fn description(&self) -> &str {
        "Read a UTF-8 text file and return line-numbered content."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
          "type": "object",
          "properties": {
            "file_path": {
              "type": "string",
              "description": "The absolute path to the file to read."
            },
            "offset": {
              "type": "number",
              "description": "The 1-based line number to start reading from. Defaults to 1."
            },
            "limit": {
              "type": "number",
              "description": format!(
                "The maximum number of lines to read. Defaults to {READ_MAX_LINES}; must be between 1 and {READ_MAX_LINES}."
              )
            }
          },
          "required": ["file_path"]
        })
    }

    fn call<'a>(&'a self, args: ReadToolArgs) -> super::BoxedFuture<'a, Result<String>> {
        Box::pin(async move { tokio::task::spawn_blocking(move || read_file(args)).await? })
    }
}

fn read_file(args: ReadToolArgs) -> Result<String> {
    if args.file_path.is_empty() {
        return Err(anyhow!("file_path is empty"));
    }

    let path = Path::new(&args.file_path);

    if !path.is_absolute() {
        return Err(anyhow!(
            "file_path must be an absolute path, got: {}",
            path.display()
        ));
    }

    if !path.exists() {
        return Err(anyhow!("{} does not exist", path.display()));
    }

    if path.is_dir() {
        return Err(anyhow!(
            "Path is a directory, not a file: {}",
            path.display()
        ));
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let offset: usize = usize::try_from(args.offset.unwrap_or(1))?;
    if offset < 1 {
        return Err(anyhow!("offset must be a positive integer"));
    }

    let mut limit: usize = usize::try_from(args.limit.unwrap_or(READ_MAX_LINES as u64))?;
    if !(1..=READ_MAX_LINES).contains(&limit) {
        return Err(anyhow!("limit must be between 1 and {READ_MAX_LINES}"));
    }

    limit = limit.saturating_add(offset);

    let mut stop_collect = false;
    let mut line_index: usize = 0;

    let mut data: Vec<String> = Vec::new();

    let mut data_bytes: usize = 0;

    let mut stop_reason: ReadStopReason = ReadStopReason::EndOfFile;

    for line in reader.lines() {
        line_index += 1;

        let line = line?;

        if (line_index < offset) || stop_collect {
            continue;
        }

        if line_index >= limit {
            stop_reason = ReadStopReason::LineLimit {
                last_line: line_index - 1,
            };
            stop_collect = true;
            continue;
        }

        let text = truncate_line(&line, MAX_LINE_LENGTH);
        let res_line = format!("{}: {}", line_index, text);

        if (data_bytes + res_line.len() + 1) > READ_MAX_BYTES {
            stop_reason = ReadStopReason::ByteLimit {
                last_line: line_index - 1,
            };
            stop_collect = true;
            continue;
        }

        data_bytes += res_line.len() + 1;

        data.push(res_line);
    }

    match stop_reason {
        ReadStopReason::EndOfFile => {
            if offset > line_index && !(line_index == 0 && offset == 1) {
                return Err(anyhow!(
                    "offset {} is out of range for \"{}\" ({} lines)",
                    offset,
                    path.display(),
                    line_index
                ));
            }
            data.push(format!("(End of file - total {} lines)", line_index,));
        }
        ReadStopReason::ByteLimit { last_line } => {
            data.push(format!(
                "(Output capped. Showing lines {}-{}. Use offset={} to continue.)",
                offset,
                last_line,
                last_line + 1
            ));
        }
        ReadStopReason::LineLimit { last_line } => {
            data.push(format!(
                "(Showing lines {}-{} of {}. Use offset={} to continue.)",
                offset,
                last_line,
                line_index,
                last_line + 1
            ));
        }
    }

    Ok(data.join("\n"))
}

// write
#[derive(Deserialize)]
pub struct WriteToolArgs {
    file_path: String,
    content: String,
}

pub struct WriteTool;

impl Tool for WriteTool {
    type Args = WriteToolArgs;
    const NAME: &'static str = "write";
    fn description(&self) -> &str {
        "Create or fully replace a UTF-8 text file."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
          "type": "object",
          "properties": {
            "file_path": {
              "type": "string",
              "description": "The absolute path to write."
            },
            "content": {
              "type": "string",
              "description": "Content to write to the file."
            }
          },
          "required": ["file_path", "content"]
        })
    }

    fn call<'a>(&'a self, args: Self::Args) -> super::BoxedFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { tokio::task::spawn_blocking(move || write_file(args)).await? })
    }
}

fn write_file(args: WriteToolArgs) -> Result<String> {
    if args.file_path.is_empty() {
        return Err(anyhow!("file_path is empty"));
    }

    let path = Path::new(&args.file_path);

    if !path.is_absolute() {
        return Err(anyhow!(
            "file_path must be an absolute path, got: {}",
            path.display()
        ));
    }

    if path.is_dir() {
        return Err(anyhow!(
            "Path is a directory, not a file: {}",
            path.display()
        ));
    }

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }

    let oper = if path.exists() { "Updated" } else { "Created" };

    let output = format!("{}, {}", oper, path.display());

    fs::write(path, args.content)?;

    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("weftic-read-{}-{name}", std::process::id()))
    }

    fn write_temp(name: &str, content: &str) -> PathBuf {
        let path = temp_path(name);
        fs::write(&path, content).unwrap();
        path
    }

    fn read(path: &Path, offset: Option<u64>, limit: Option<u64>) -> Result<String> {
        read_file(ReadToolArgs {
            file_path: path.to_string_lossy().into_owned(),
            offset,
            limit,
        })
    }

    #[test]
    fn reads_whole_small_file() {
        let path = write_temp("whole", "a\nb\nc\nd\ne");
        let out = read(&path, None, None).unwrap();
        assert!(out.starts_with("1: a\n2: b\n3: c\n4: d\n5: e"));
        assert!(out.ends_with("(End of file - total 5 lines)"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn applies_offset() {
        let path = write_temp("offset", "a\nb\nc\nd\ne");
        let out = read(&path, Some(3), None).unwrap();
        assert!(out.starts_with("3: c"));
        assert!(!out.contains("1: a"));
        assert!(out.ends_with("(End of file - total 5 lines)"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn offset_and_limit_window() {
        let path = write_temp("offset-limit", "a\nb\nc\nd\ne");
        let out = read(&path, Some(2), Some(2)).unwrap();
        assert!(out.starts_with("2: b\n3: c"));
        assert!(out.ends_with("(Showing lines 2-3 of 5. Use offset=4 to continue.)"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn line_limit_reports_continuation() {
        let path = write_temp("limit", "a\nb\nc\nd\ne");
        let out = read(&path, None, Some(2)).unwrap();
        assert!(out.starts_with("1: a\n2: b"));
        assert!(out.ends_with("(Showing lines 1-2 of 5. Use offset=3 to continue.)"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn limit_at_or_beyond_total_reads_to_end() {
        let path = write_temp("limit-exact", "a\nb\nc\nd\ne");
        for limit in [5, 10] {
            let out = read(&path, None, Some(limit)).unwrap();
            assert!(out.ends_with("(End of file - total 5 lines)"));
            assert!(!out.contains("Showing lines"));
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_file_is_not_an_error() {
        let path = write_temp("empty", "");
        let out = read(&path, None, None).unwrap();
        assert_eq!(out, "(End of file - total 0 lines)");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn offset_past_eof_errors() {
        let path = write_temp("past-eof", "a\nb\nc");
        let err = read(&path, Some(4), None).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "offset 4 is out of range for \"{}\" (3 lines)",
                path.display()
            )
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_file_with_offset_past_eof_errors() {
        let path = write_temp("empty-past-eof", "");
        let err = read(&path, Some(2), None).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "offset 2 is out of range for \"{}\" (0 lines)",
                path.display()
            )
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn truncates_overlong_lines() {
        let path = write_temp("long-line", &"x".repeat(2500));
        let out = read(&path, None, None).unwrap();
        assert!(out.contains(" ... (line truncated to 2000 chars)"));
        assert!(out.ends_with("(End of file - total 1 lines)"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn byte_limit_reports_capped_and_counts_match() {
        let content = (0..2000)
            .map(|i| format!("{i:0>100}"))
            .collect::<Vec<_>>()
            .join("\n");
        let path = write_temp("byte-cap", &content);
        let out = read(&path, None, None).unwrap();
        assert!(out.contains("(Output capped."));
        let shown = out.lines().count() - 1; // minus the footer
        assert!(shown < 2000);
        assert!(
            out.contains(&format!("Showing lines 1-{shown}.")),
            "footer did not match shown line count {shown}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn missing_file_errors() {
        let path = temp_path("does-not-exist");
        let _ = fs::remove_file(&path);
        assert!(read(&path, None, None).is_err());
    }

    #[test]
    fn rejects_relative_path() {
        let err = read(Path::new("relative.txt"), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("absolute"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_directory() {
        let err = read(&std::env::temp_dir(), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("directory"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_non_positive_offset() {
        let path = write_temp("offset-zero", "a\nb\nc");
        let err = read(&path, Some(0), None).unwrap_err().to_string();
        assert_eq!(err, "offset must be a positive integer");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_limit_below_one() {
        let path = write_temp("limit-zero", "a\nb\nc");
        let err = read(&path, None, Some(0)).unwrap_err().to_string();
        assert_eq!(err, "limit must be between 1 and 2000");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_limit_above_max() {
        let path = write_temp("limit-over", "a\nb\nc");
        let err = read(&path, None, Some(2001)).unwrap_err().to_string();
        assert_eq!(err, "limit must be between 1 and 2000");
        let _ = fs::remove_file(path);
    }
}
