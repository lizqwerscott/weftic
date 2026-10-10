use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use ignore::{WalkBuilder, overrides::OverrideBuilder};
use serde::Deserialize;

use anyhow::{Result, anyhow};
use serde_json::json;

use crate::permissions::ToolGroup;

use super::{Tool, ToolContext};

const GLOB_VCS_EXCLUDES: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];
const GLOB_MAX_RESULTS: usize = 100;

#[derive(Deserialize)]
pub struct GlobToolArgs {
    pattern: String,
    path: Option<String>,
}

pub struct GlobTool;

impl Tool for GlobTool {
    type Args = GlobToolArgs;
    const NAME: &'static str = "glob";
    const GROUP: ToolGroup = ToolGroup::File;

    fn description(&self) -> &str {
        "Find files whose path matches a glob pattern. Returns matching file paths only, never directories, \
         and includes hidden and ignored files (the VCS metadata directories are excluded). \
         Results are sorted by modification time, newest first."
    }

    fn system_description(&self) -> &str {
        "Use the glob tool — not shell find — to discover files by path pattern. A pattern with no \"/\" matches
  basenames at any depth, so \"*\" matches every file in the tree rather than its top level. Results are files
   only, never directories, and include hidden and ignored files: results come back in modification-time
  order, newest first, and at most 100 paths are returned, with a footer reporting how many were omitted."
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
              "description": "Directory to search in. Optional; defaults to the workspace root."
            }
          },
            "required": ["pattern"],
          "additionalProperties": false
        })
    }

    fn call<'a>(
        &'a self,
        args: Self::Args,
        ctx: ToolContext,
    ) -> super::BoxedFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { tokio::task::spawn_blocking(move || glob_dir(args, ctx)).await? })
    }
}

fn top_level_segment(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let first = rel
        .components()
        .find(|c| matches!(c, Component::Normal(_)))?;
    Some(first.as_os_str().to_string_lossy().into_owned())
}

fn sample_across_buckets(buckets: &[Vec<usize>], max: usize) -> (Vec<usize>, usize, usize) {
    let mut cursors = vec![0usize; buckets.len()];
    let mut taken = 0usize;

    while taken < max {
        let mut progressed = false;
        for (i, bucket) in buckets.iter().enumerate() {
            if taken >= max {
                break;
            }
            if cursors[i] < bucket.len() {
                cursors[i] += 1;
                taken += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    let picked: Vec<usize> = buckets
        .iter()
        .enumerate()
        .flat_map(|(i, b)| b[..cursors[i]].iter().copied())
        .collect();

    let shown = cursors.iter().filter(|&&c| c > 0).count();
    (picked, shown, buckets.len())
}

fn glob_dir(args: GlobToolArgs, ctx: ToolContext) -> Result<String> {
    if args.pattern.is_empty() {
        return Err(anyhow!("pattern is empty"));
    }

    let path = match &args.path {
        Some(raw) => ctx.workspace.resolve(raw)?,
        None => ctx.workspace.cwd().to_path_buf(),
    };

    if !path.exists() {
        return Err(anyhow!("{} does not exist", path.display()));
    }

    if !path.is_dir() {
        return Err(anyhow!("Path is a file, not a dir: {}", path.display()));
    }

    let mut overrides = OverrideBuilder::new(".");

    overrides.add(&args.pattern)?;
    overrides.add(&format!("!**/{{{}}}", GLOB_VCS_EXCLUDES.join(",")))?;
    overrides.add(&format!("!**/{{{}}}/**", GLOB_VCS_EXCLUDES.join(",")))?;
    let override_matcher = overrides.build()?;

    let walker = WalkBuilder::new(&path)
        .standard_filters(false)
        .overrides(override_matcher)
        .build();

    let mut res: Vec<(PathBuf, SystemTime)> = Vec::new();

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

        let current_path = entry.into_path();

        if !current_path.is_dir() {
            res.push((current_path, mtime));
        }
    }

    Ok(format_paths(res, &path))
}

fn format_paths(mut res: Vec<(PathBuf, SystemTime)>, root: &Path) -> String {
    res.sort_by_key(|a| std::cmp::Reverse(a.1));

    if res.is_empty() {
        return String::from("No files found");
    }

    let seen = res.len();
    if seen <= GLOB_MAX_RESULTS {
        return res
            .iter()
            .map(|(path, _)| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
    }

    let mut buckets: Vec<Vec<usize>> = Vec::new();
    let mut bucket_index: HashMap<String, usize> = HashMap::new();

    for (i, (path, _)) in res.iter().enumerate() {
        let Some(key) = top_level_segment(path, root) else {
            continue;
        };
        let bucket = *bucket_index.entry(key).or_insert_with(|| {
            buckets.push(Vec::new());
            buckets.len() - 1
        });
        buckets[bucket].push(i);
    }

    let (picked, shown, total) = sample_across_buckets(&buckets, GLOB_MAX_RESULTS);

    let mut lines: Vec<String> = picked
        .iter()
        .map(|&i| res[i].0.to_string_lossy().into_owned())
        .collect();

    lines.push(String::new());
    if total == seen {
        lines.push(format!(
            "(Showing {} of {} paths; narrow pattern or path to see more.)",
            picked.len(),
            seen
        ));
    } else {
        let mut footer = format!(
            "Showing {} of {} paths, sampled across {} of the {} top-level entries this pattern matched instead of taken in modification-time order.",
            picked.len(),
            seen,
            shown,
            total
        );
        if shown < total {
            footer.push_str(" Narrow path to inspect a specific subtree.");
        }
        lines.push(format!("({footer})"));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn entry(path: &str, secs: u64) -> (PathBuf, SystemTime) {
        (
            PathBuf::from(path),
            SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        )
    }

    #[test]
    fn empty_reports_none() {
        assert_eq!(format_paths(Vec::new(), Path::new(".")), "No files found");
    }

    #[test]
    fn sorts_newest_first_without_footer_under_cap() {
        let out = format_paths(
            vec![entry("old.rs", 1), entry("new.rs", 9), entry("mid.rs", 5)],
            Path::new("."),
        );
        assert_eq!(out, "new.rs\nmid.rs\nold.rs");
    }

    #[test]
    fn exactly_at_cap_has_no_footer() {
        let res = (0..GLOB_MAX_RESULTS as u64)
            .map(|i| entry(&format!("f{i}.rs"), i))
            .collect();
        let out = format_paths(res, Path::new("."));
        assert_eq!(out.lines().count(), GLOB_MAX_RESULTS);
        assert!(!out.contains("Showing"));
    }

    #[test]
    fn over_cap_without_subtrees_keeps_plain_footer() {
        let total = GLOB_MAX_RESULTS + 10;
        let res = (0..total as u64)
            .map(|i| entry(&format!("f{i}.rs"), i))
            .collect();
        let out = format_paths(res, Path::new("."));
        assert_eq!(out.lines().count(), GLOB_MAX_RESULTS + 2);
        assert!(out.ends_with(&format!(
            "(Showing {} of {} paths; narrow pattern or path to see more.)",
            GLOB_MAX_RESULTS, total
        )));
    }

    #[test]
    fn over_cap_samples_across_top_level_entries() {
        let mut res = Vec::new();
        for (dir, base) in [("a", 200u64), ("b", 100), ("c", 0)] {
            for i in 0..60u64 {
                res.push(entry(&format!("src/{dir}/f{i}.rs"), base + i));
            }
        }

        let out = format_paths(res, Path::new("src"));
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(lines.len(), GLOB_MAX_RESULTS + 2);
        assert_eq!(lines[0], "src/a/f59.rs");
        assert_eq!(lines.iter().filter(|l| l.starts_with("src/a/")).count(), 34);
        assert_eq!(lines.iter().filter(|l| l.starts_with("src/b/")).count(), 33);
        assert_eq!(lines.iter().filter(|l| l.starts_with("src/c/")).count(), 33);
        assert!(out.contains("sampled across 3 of the 3 top-level entries"));
    }

    #[test]
    fn narrow_hint_when_some_entries_are_not_reached() {
        let mut res = Vec::new();
        for d in 0..150u64 {
            res.push(entry(&format!("src/d{d}/f0.rs"), d * 2));
            res.push(entry(&format!("src/d{d}/f1.rs"), d * 2 + 1));
        }

        let out = format_paths(res, Path::new("src"));
        assert!(out.contains("sampled across 100 of the 150 top-level entries"));
        assert!(out.contains("Narrow path to inspect a specific subtree."));
    }
}
