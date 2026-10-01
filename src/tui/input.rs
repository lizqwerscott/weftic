use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Editor, Helper};

use anyhow::Result;

#[derive(Clone)]
pub struct AgentHelper {
    commands: Vec<String>,
}

impl Helper for AgentHelper {}

impl Completer for AgentHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let start = line[..pos].rfind(' ').map_or(0, |i| i + 1);
        let word = &line[start..pos];

        let matches: Vec<Pair> = self
            .commands
            .iter()
            .filter(|cmd| cmd.starts_with(word))
            .map(|cmd| Pair {
                display: cmd.clone(),
                replacement: cmd.clone(),
            })
            .collect();

        Ok((start, matches))
    }
}

impl Highlighter for AgentHelper {}

impl Hinter for AgentHelper {
    type Hint = String;
}

impl Validator for AgentHelper {}

pub fn build_input() -> Result<(AgentHelper, Editor<AgentHelper, DefaultHistory>)> {
    let config = rustyline::config::Config::builder()
        .completion_type(CompletionType::List)
        .build();
    let mut rl: Editor<AgentHelper, _> = Editor::with_config(config)?;
    let helper = AgentHelper {
        commands: vec!["/exit".to_string()],
    };
    rl.set_helper(Some(helper.clone()));
    Ok((helper, rl))
}
