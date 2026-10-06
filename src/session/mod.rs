use std::path::PathBuf;

use genai::chat::ChatRequest;

use crate::channel::SessionKey;
use crate::session::history::{SessionHistory, SessionTurn};
use crate::session::spec::SessionSpec;

pub mod history;
pub mod manager;
pub mod resolver;
pub mod spec;

pub struct Session {
    key: SessionKey,
    history: SessionHistory,
    workspace_root: PathBuf,
    template: String,
}

impl Session {
    pub fn new(spec: SessionSpec) -> Self {
        Self {
            key: spec.key,
            history: SessionHistory::default(),
            workspace_root: spec.workspace_root,
            template: spec.template,
        }
    }

    pub fn append_turn(&mut self, turn: SessionTurn) {
        self.history.turns.push(turn);
    }

    pub fn get_template(&self) -> &str {
        &self.template
    }

    pub fn get_history(&self) -> ChatRequest {
        self.history.to_chat_request()
    }
}
