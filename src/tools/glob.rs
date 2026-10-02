use std::{path::Path, time::SystemTime};

use ignore::{WalkBuilder, overrides::OverrideBuilder};
use serde::Deserialize;

use anyhow::{Result, anyhow};
use serde_json::json;

use super::Tool;

const GLOB_VCS_EXCLUDES: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];
const GLOB_MAX_RESULTS: usize = 100;

#[derive(Deserialize)]
pub struct GlobToolArgs {
    pattern: String,
    path: String,
}

pub struct GlobTool;

impl Tool for GlobTool {
    type Args = GlobToolArgs;
    const NAME: &'static str = "glob";

    fn description(&self) -> &str {
        "Find files whose path matches a glob pattern. Returns matching file paths only, never directories, \
         and includes hidden and ignored files (the VCS metadata directories are excluded). \
         Results are sorted by modification time, newest first."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
          "type": "object",
          "properties": {
            "pattern": {
              "type": "string",
              "description": "Glob pattern to match file paths against."
            },
            "path": {
              "type": "string",
              "description": "Directory to search in."
            }
          },
            "required": ["pattern", "path"],
          "additionalProperties": false
        })
    }

    fn call<'a>(&'a self, args: Self::Args) -> super::BoxedFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { tokio::task::spawn_blocking(move || glob_dir(args)).await? })
    }
}

fn glob_dir(args: GlobToolArgs) -> Result<String> {
    if args.path.is_empty() {
        return Err(anyhow!("path is empty"));
    }

    if args.pattern.is_empty() {
        return Err(anyhow!("pattern is empty"));
    }

    let path = Path::new(&args.path);

    if !path.exists() {
        return Err(anyhow!("{} is not exists", path.display()));
    }

    if !path.is_dir() {
        return Err(anyhow!("Path is a file, not a dir: {}", path.display()));
    }

    let mut overrides = OverrideBuilder::new(".");

    overrides.add(&args.pattern)?;
    overrides.add(&format!("!**/{{{}}}", GLOB_VCS_EXCLUDES.join(",")))?;
    overrides.add(&format!("!**/{{{}}}/**", GLOB_VCS_EXCLUDES.join(",")))?;
    let override_matcher = overrides.build()?;

    let walker = WalkBuilder::new(path)
        .standard_filters(false)
        .overrides(override_matcher)
        .build();

    let mut res: Vec<(String, SystemTime)> = Vec::new();

    for result in walker {
        let Ok(entry) = result else {
            continue;
        };

        let Ok(metadata) = entry.metadata() else {
            continue;
        };

        let Ok(mtime) = metadata.modified() else {
            continue;
        };

        let path = entry.into_path();

        if !path.is_dir()
            && let Some(text) = path.to_str()
        {
            res.push((text.to_string(), mtime));
        }
    }
    Ok(format_paths(res))
}

fn format_paths(mut res: Vec<(String, SystemTime)>) -> String {
    res.sort_by_key(|a| std::cmp::Reverse(a.1));

    if res.is_empty() {
        return String::from("No files found");
    }

    let seen = res.len();
    let shown = seen.min(GLOB_MAX_RESULTS);
    let mut lines: Vec<String> = res[..shown].iter().map(|(path, _)| path.clone()).collect();

    if seen > shown {
        lines.push(String::new());
        lines.push(format!(
            "(Showing {} of {} paths; narrow pattern or path to see more.)",
            shown, seen
        ));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn entry(path: &str, secs: u64) -> (String, SystemTime) {
        (
            path.to_string(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        )
    }

    #[test]
    fn empty_reports_none() {
        assert_eq!(format_paths(Vec::new()), "No files found");
    }

    #[test]
    fn sorts_newest_first_without_footer_under_cap() {
        let out = format_paths(vec![
            entry("old.rs", 1),
            entry("new.rs", 9),
            entry("mid.rs", 5),
        ]);
        assert_eq!(out, "new.rs\nmid.rs\nold.rs");
    }

    #[test]
    fn exactly_at_cap_has_no_footer() {
        let res = (0..GLOB_MAX_RESULTS as u64)
            .map(|i| entry(&format!("f{i}.rs"), i))
            .collect();
        let out = format_paths(res);
        assert_eq!(out.lines().count(), GLOB_MAX_RESULTS);
        assert!(!out.contains("Showing"));
    }

    #[test]
    fn over_cap_truncates_and_reports_footer() {
        let total = GLOB_MAX_RESULTS + 10;
        let res = (0..total as u64)
            .map(|i| entry(&format!("f{i}.rs"), i))
            .collect();
        let out = format_paths(res);
        assert_eq!(out.lines().count(), GLOB_MAX_RESULTS + 2);
        assert!(out.ends_with(&format!(
            "(Showing {} of {} paths; narrow pattern or path to see more.)",
            GLOB_MAX_RESULTS, total
        )));
    }
}
