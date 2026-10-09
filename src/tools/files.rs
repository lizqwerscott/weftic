use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
};

use serde::Deserialize;
use serde_json::json;

use anyhow::{Result, anyhow};

use crate::permissions::ToolGroup;
use crate::tools::MAX_LINE_LENGTH;

use super::{Tool, ToolContext, truncate_line};

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
    const GROUP: ToolGroup = ToolGroup::File;

    fn description(&self) -> &str {
        "Read a UTF-8 text file and return line-numbered content."
    }

    fn system_description(&self) -> &str {
        "Use the read tool — not shell commands like cat — to inspect text files. Results include line numbers.
  Use offset and limit to continue reading large files."
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

    fn call<'a>(&'a self, args: ReadToolArgs, ctx: ToolContext) -> super::BoxedFuture<'a, Result<String>> {
        Box::pin(async move { tokio::task::spawn_blocking(move || read_file(args, ctx)).await? })
    }
}

fn read_file(args: ReadToolArgs, ctx: ToolContext) -> Result<String> {
    if args.file_path.is_empty() {
        return Err(anyhow!("file_path is empty"));
    }

    let path = ctx.workspace.resolve(&args.file_path)?;

    if !path.exists() {
        return Err(anyhow!("{} does not exist", path.display()));
    }

    if path.is_dir() {
        return Err(anyhow!(
            "Path is a directory, not a file: {}",
            path.display()
        ));
    }

    let file = File::open(&path)?;
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
    const GROUP: ToolGroup = ToolGroup::File;

    fn description(&self) -> &str {
        "Create or fully replace a UTF-8 text file."
    }

    fn system_description(&self) -> &str {
        "Use the write tool to create files or completely replace file contents. Existing files are overwritten,
  so read an existing file first and prefer edit for targeted changes."
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

    fn call<'a>(&'a self, args: Self::Args, ctx: ToolContext) -> super::BoxedFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { tokio::task::spawn_blocking(move || write_file(args, ctx)).await? })
    }
}

fn write_file(args: WriteToolArgs, ctx: ToolContext) -> Result<String> {
    if args.file_path.is_empty() {
        return Err(anyhow!("file_path is empty"));
    }

    let path = ctx.workspace.resolve(&args.file_path)?;

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

    fs::write(&path, args.content)?;

    Ok(output)
}

// edit
#[derive(Deserialize)]
pub struct EditToolArgs {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

pub struct EditTool;

impl Tool for EditTool {
    type Args = EditToolArgs;
    const NAME: &'static str = "edit";
    const GROUP: ToolGroup = ToolGroup::File;

    fn description(&self) -> &str {
        "Edit an existing UTF-8 text file by replacing literal text."
    }

    fn system_description(&self) -> &str {
        "Read a file before editing it, unless you just created or edited it in this session."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
          "type": "object",
          "properties": {
            "file_path": {
              "type": "string",
              "description": "Path to edit, resolved by the filesystem backend. Provide `file_path` before `old_string` and `new_string` in the arguments."
            },
            "old_string": {
              "type": "string",
              "description": "Literal text to replace."
            },
            "new_string": {
              "type": "string",
              "description": "Literal replacement text. Use an empty string to delete the match."
            },
            "replace_all": {
              "type": "boolean",
              "description": "Replace all matches. Defaults to false; when false, old_string must appear exactly once."
            }
          },
          "required": [
            "file_path",
            "old_string",
            "new_string"
          ]
        })
    }

    fn call<'a>(&'a self, args: Self::Args, ctx: ToolContext) -> super::BoxedFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { tokio::task::spawn_blocking(move || edit_file(args, ctx)).await? })
    }
}

fn edit_file(args: EditToolArgs, ctx: ToolContext) -> Result<String> {
    if args.file_path.is_empty() {
        return Err(anyhow!("file_path is empty"));
    }

    let path = ctx.workspace.resolve(&args.file_path)?;

    if !path.exists() {
        return Err(anyhow!("{} does not exist", path.display()));
    }

    if path.is_dir() {
        return Err(anyhow!(
            "Path is a directory, not a file: {}",
            path.display()
        ));
    }

    if args.old_string.is_empty() {
        return Err(anyhow!("old_string is empty"));
    }

    let content = fs::read_to_string(&path)?;
    let count = content.matches(&args.old_string).count();

    if count == 0 {
        return Err(anyhow!("old_string not found"));
    }

    let replace_all = args.replace_all.unwrap_or(false);

    let new_content = if replace_all {
        content.replace(&args.old_string, &args.new_string)
    } else {
        if count > 1 {
            return Err(anyhow!(
                "old_string matched {} times in {}",
                count,
                path.display()
            ));
        }
        content.replacen(&args.old_string, &args.new_string, 1)
    };

    fs::write(&path, new_content)?;
    let out = if replace_all {
        format!(
            "The file {} has been updated. All occurrences were successfully replaced.",
            path.display()
        )
    } else {
        format!("The file {} has been updated successfully.", path.display())
    };

    Ok(out)
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
        read_file(
            ReadToolArgs {
                file_path: path.to_string_lossy().into_owned(),
                offset,
                limit,
            },
            crate::tools::test_context(),
        )
    }

    fn edit(
        path: &Path,
        old_string: &str,
        new_string: &str,
        replace_all: Option<bool>,
    ) -> Result<String> {
        edit_file(
            EditToolArgs {
                file_path: path.to_string_lossy().into_owned(),
                old_string: old_string.to_string(),
                new_string: new_string.to_string(),
                replace_all,
            },
            crate::tools::test_context(),
        )
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

    #[test]
    fn edit_replaces_single_match() {
        let path = write_temp("edit-single", "alpha\nbeta\ngamma");
        let out = edit(&path, "beta", "BETA", None).unwrap();
        assert_eq!(
            out,
            format!("The file {} has been updated successfully.", path.display())
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nBETA\ngamma");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_deletes_match_with_empty_new_string() {
        let path = write_temp("edit-delete", "keep\ndrop\nkeep");
        edit(&path, "drop\n", "", None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep\nkeep");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_replace_all_replaces_every_occurrence() {
        let path = write_temp("edit-all", "x x x");
        let out = edit(&path, "x", "y", Some(true)).unwrap();
        assert_eq!(
            out,
            format!(
                "The file {} has been updated. All occurrences were successfully replaced.",
                path.display()
            )
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "y y y");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_rejects_ambiguous_match_without_replace_all() {
        let path = write_temp("edit-ambiguous", "x x");
        let err = edit(&path, "x", "y", None).unwrap_err().to_string();
        assert_eq!(
            err,
            format!("old_string matched 2 times in {}", path.display())
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "x x");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_rejects_missing_match() {
        let path = write_temp("edit-missing", "hello");
        let err = edit(&path, "absent", "x", None).unwrap_err().to_string();
        assert_eq!(err, "old_string not found");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_rejects_empty_old_string() {
        let path = write_temp("edit-empty-old", "hello");
        let err = edit(&path, "", "x", None).unwrap_err().to_string();
        assert!(err.contains("empty"), "unexpected error: {err}");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_rejects_empty_file_path() {
        let err = edit(Path::new(""), "a", "b", None).unwrap_err().to_string();
        assert_eq!(err, "file_path is empty");
    }

    #[test]
    fn edit_rejects_directory() {
        let err = edit(&std::env::temp_dir(), "a", "b", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("directory"), "unexpected error: {err}");
    }

    #[test]
    fn edit_rejects_missing_file() {
        let path = temp_path("edit-does-not-exist");
        let _ = fs::remove_file(&path);
        let err = edit(&path, "a", "b", None).unwrap_err().to_string();
        assert!(err.contains("does not exist"), "unexpected error: {err}");
    }
}
