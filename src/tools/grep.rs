use std::{path::PathBuf, sync::Mutex};

use anyhow::{Result, anyhow};
use grep::{
    regex::RegexMatcher,
    searcher::{BinaryDetection, Searcher, SearcherBuilder, sinks::UTF8},
};
use ignore::{
    ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState, overrides::OverrideBuilder,
};
use serde::Deserialize;
use serde_json::json;

use crate::permissions::ToolGroup;
use crate::tools::{MAX_LINE_LENGTH, truncate_line};

use super::{Tool, ToolContext};

const GREP_MAX_MATCHES: usize = 250;

struct FileMatches {
    path: PathBuf,
    hits: Vec<(u64, String)>,
}

struct SearchContent {
    matcher: RegexMatcher,
    matches: Mutex<Vec<FileMatches>>,
}

struct SearchVisitorBuilder<'a> {
    ctx: &'a SearchContent,
    searcher: Searcher,
}

impl<'a> ParallelVisitorBuilder<'a> for SearchVisitorBuilder<'a> {
    fn build(&mut self) -> Box<dyn ignore::ParallelVisitor + 'a> {
        Box::new(SearchVisitor {
            ctx: self.ctx,
            searcher: self.searcher.clone(),
        })
    }
}

struct SearchVisitor<'a> {
    ctx: &'a SearchContent,
    searcher: Searcher,
}

impl<'a> ParallelVisitor for SearchVisitor<'a> {
    fn visit(
        &mut self,
        entry: std::prelude::v1::Result<ignore::DirEntry, ignore::Error>,
    ) -> ignore::WalkState {
        let Ok(entry) = entry else {
            return WalkState::Continue;
        };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            return WalkState::Continue;
        }

        let path = entry.path();
        let mut hits: Vec<(u64, String)> = Vec::new();

        let res = self.searcher.search_path(
            &self.ctx.matcher,
            path,
            UTF8(|lnum, line| {
                hits.push((lnum, line.trim_end().to_string()));
                Ok(true)
            }),
        );

        if let Err(e) = res {
            tracing::warn!(path = %path.display(), error = %e, "grep failed");
        }

        if !hits.is_empty() {
            self.ctx.matches.lock().unwrap().push(FileMatches {
                path: path.to_path_buf(),
                hits,
            });
        }

        WalkState::Continue
    }
}

#[derive(Deserialize)]
pub struct GrepToolArgs {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}

pub struct GrepTool;

impl Tool for GrepTool {
    type Args = GrepToolArgs;
    const NAME: &'static str = "grep";
    const GROUP: ToolGroup = ToolGroup::File;

    fn description(&self) -> &str {
        "Search file contents with a ripgrep regex and return grouped matches."
    }

    fn system_description(&self) -> &str {
        "Use the grep tool — not shell grep or rg — to search file contents. Use read on a matched file when you
  need surrounding context."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
          "type": "object",
          "properties": {
            "pattern": {
              "type": "string",
              "description": "Ripgrep regex to search file contents with."
            },
            "path": {
              "type": "string",
              "description": "File or directory target. Optional; defaults to the workspace root."
            },
            "include": {
              "type": "string",
              "description": "One positive glob filter for which files to search (e.g. *.ts). Comma-separated lists and negated (!) values are rejected up front."
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
        Box::pin(async move { tokio::task::spawn_blocking(move || grep_dir(args, ctx)).await? })
    }
}

fn validate_include(include: &str) -> Result<&str> {
    let include = include.trim();
    if include.is_empty() {
        return Err(anyhow!("include must be a non-empty glob when given"));
    }
    if include.starts_with('!') {
        return Err(anyhow!(
            "include must be a positive glob filter; negated patterns (\"!\") are not supported"
        ));
    }

    let mut brace_depth: u32 = 0;
    for ch in include.chars() {
        match ch {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            ',' if brace_depth == 0 => {
                return Err(anyhow!(
                    "include must be one glob, not a comma-separated list (use {{a,b}} alternation instead)"
                ));
            }
            _ => {}
        }
    }
    Ok(include)
}

fn grep_dir(args: GrepToolArgs, tool_ctx: ToolContext) -> Result<String> {
    if args.pattern.is_empty() {
        return Err(anyhow!("pattern is empty"));
    }

    let include = match args.include.as_deref() {
        Some(raw) => Some(validate_include(raw)?.to_string()),
        None => None,
    };

    let path = match &args.path {
        Some(raw) => tool_ctx.workspace.resolve(raw)?,
        None => tool_ctx.workspace.cwd().to_path_buf(),
    };

    if !path.exists() {
        return Err(anyhow!("{} does not exist", path.display()));
    }

    let matcher = RegexMatcher::new(&args.pattern)?;

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();

    let ctx = SearchContent {
        matcher,
        matches: Mutex::new(Vec::new()),
    };

    if path.is_dir() {
        let mut walk = WalkBuilder::new(&path);
        if let Some(include) = include {
            let mut overrides = OverrideBuilder::new(".");
            overrides.add(&include)?;
            walk.overrides(overrides.build()?);
        }

        let mut vbuilder = SearchVisitorBuilder {
            ctx: &ctx,
            searcher,
        };
        walk.build_parallel().visit(&mut vbuilder);
    } else {
        let mut hits: Vec<(u64, String)> = Vec::new();

        searcher.search_path(
            &ctx.matcher,
            &path,
            UTF8(|lnum, line| {
                hits.push((lnum, line.trim_end().to_string()));
                Ok(true)
            }),
        )?;

        if !hits.is_empty() {
            ctx.matches.lock().unwrap().push(FileMatches {
                path: path.to_path_buf(),
                hits,
            });
        }
    }

    Ok(format_matches(ctx.matches.into_inner().unwrap()))
}

fn format_matches(mut all: Vec<FileMatches>) -> String {
    all.sort_by(|a, b| a.path.cmp(&b.path));

    if all.is_empty() {
        return String::from("No matches found");
    }

    let seen: usize = all.iter().map(|f| f.hits.len()).sum();
    let mut kept = 0usize;
    let mut out: Vec<String> = Vec::new();

    for file in &all {
        if kept >= GREP_MAX_MATCHES {
            break;
        }
        let Some(path) = file.path.to_str() else {
            continue;
        };
        out.push(path.to_string());
        for (lnum, line) in &file.hits {
            if kept >= GREP_MAX_MATCHES {
                break;
            }
            out.push(format!(
                "Line {}: {}",
                lnum,
                truncate_line(line, MAX_LINE_LENGTH)
            ));
            kept += 1;
        }
    }

    let truncated = seen > kept;
    let header = if truncated {
        format!("Found {} of {} matches", kept, seen)
    } else {
        format!("Found {} matches", seen)
    };

    if truncated {
        out.push(String::new());
        out.push(format!(
            "(Only the first {} of {} matches are shown; narrow pattern, path, or include to see more.)",
            kept, seen
        ));
    }

    format!("{header}\n\n{}", out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(count: usize) -> Vec<(u64, String)> {
        (1..=count)
            .map(|i| (i as u64, format!("line {i}")))
            .collect()
    }

    fn file(path: &str, count: usize) -> FileMatches {
        FileMatches {
            path: PathBuf::from(path),
            hits: hits(count),
        }
    }

    #[test]
    fn no_matches_reports_plainly() {
        assert_eq!(format_matches(Vec::new()), "No matches found");
    }

    #[test]
    fn under_cap_lists_every_match_without_footer() {
        let out = format_matches(vec![file("b.rs", 2), file("a.rs", 1)]);
        assert_eq!(
            out,
            "Found 3 matches\n\na.rs\nLine 1: line 1\nb.rs\nLine 1: line 1\nLine 2: line 2"
        );
    }

    #[test]
    fn exactly_at_cap_is_not_reported_as_truncated() {
        let out = format_matches(vec![file("a.rs", GREP_MAX_MATCHES)]);
        assert!(out.starts_with(&format!("Found {} matches\n\n", GREP_MAX_MATCHES)));
        assert!(!out.contains("Only the first"));
    }

    #[test]
    fn over_cap_reports_kept_of_seen_and_footer() {
        let total = GREP_MAX_MATCHES + 5;
        let out = format_matches(vec![file("a.rs", total)]);
        assert!(out.starts_with(&format!(
            "Found {} of {} matches\n\n",
            GREP_MAX_MATCHES, total
        )));
        assert!(out.ends_with(&format!(
            "(Only the first {} of {} matches are shown; narrow pattern, path, or include to see more.)",
            GREP_MAX_MATCHES, total
        )));
        let shown = out.lines().filter(|line| line.starts_with("Line ")).count();
        assert_eq!(shown, GREP_MAX_MATCHES);
    }

    #[test]
    fn cap_is_consumed_in_path_order() {
        let out = format_matches(vec![file("a.rs", GREP_MAX_MATCHES), file("b.rs", 3)]);
        assert!(out.contains("a.rs"));
        assert!(!out.contains("b.rs"));
        assert!(out.starts_with(&format!(
            "Found {} of {} matches\n\n",
            GREP_MAX_MATCHES,
            GREP_MAX_MATCHES + 3
        )));
    }

    #[test]
    fn include_accepts_positive_glob_and_trims() {
        assert_eq!(validate_include("  *.rs  ").unwrap(), "*.rs");
    }

    #[test]
    fn include_accepts_brace_alternation() {
        assert_eq!(validate_include("*.{ts,tsx}").unwrap(), "*.{ts,tsx}");
    }

    #[test]
    fn include_rejects_blank() {
        assert_eq!(
            validate_include("   ").unwrap_err().to_string(),
            "include must be a non-empty glob when given"
        );
    }

    #[test]
    fn include_rejects_negation() {
        assert_eq!(
            validate_include("!*.rs").unwrap_err().to_string(),
            "include must be a positive glob filter; negated patterns (\"!\") are not supported"
        );
    }

    #[test]
    fn include_rejects_top_level_comma() {
        assert_eq!(
            validate_include("*.rs,*.ts").unwrap_err().to_string(),
            "include must be one glob, not a comma-separated list (use {a,b} alternation instead)"
        );
    }
}
