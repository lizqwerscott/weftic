use std::{fs::File, io::BufRead, io::BufReader, path::Path};

use serde::Deserialize;
use serde_json::json;

use anyhow::{Result, anyhow};

use super::Tool;

// read
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

    fn descrption(&self) -> &str {
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
              "description": "The maximum number of lines to read. Defaults to and is capped by the configured readLimit (2000)."
            }
          },
          "required": ["file_path"]
        })
    }

    fn call<'a>(&'a self, args: ReadToolArgs) -> super::BoxedFuture<'a, Result<String>> {
        Box::pin(async move {
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
                return Err(anyhow!("{} is not exists", path.display()));
            }

            if path.is_dir() {
                return Err(anyhow!(
                    "Path is a directory, not a file: {}",
                    path.display()
                ));
            }

            let file = File::open(path)?;
            let reader = BufReader::new(file);

            let offset: usize = usize::try_from(args.offset.unwrap_or(1).max(1))?;
            let limit: usize = usize::try_from(args.limit.unwrap_or(2000).min(2000))? + offset;

            let mut line_index: usize = 0;

            let mut data: Vec<String> = Vec::new();

            for line in reader.lines() {
                line_index += 1;

                let line = line?;
                if line_index < offset {
                    continue;
                }

                if line_index >= limit {
                    break;
                }

                data.push(format!("{}|{}", line_index, line));
            }

            Ok(data.join("\n"))
        })
    }
}
