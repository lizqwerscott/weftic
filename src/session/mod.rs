use std::fmt;
use std::sync::Arc;

use genai::chat::ChatRequest;

use crate::channel::SessionKey;
use crate::channel::capabilities::ChannelCapabilities;
use crate::channel::sender::ChannelSender;
use crate::permissions::{Mode, Role};
use crate::session::history::{SessionHistory, SessionTurn};
use crate::session::spec::SessionSpec;
use crate::workspace::Workspace;

pub mod history;
pub mod manager;
pub mod resolver;
pub mod spec;

pub struct ChannelBinding {
    pub capabilities: ChannelCapabilities,
    pub sender: Option<Arc<dyn ChannelSender>>,
}

impl fmt::Debug for ChannelBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelBinding")
            .field("capabilities", &self.capabilities)
            .field("has_sender", &self.sender.is_some())
            .finish()
    }
}

pub struct Session {
    key: SessionKey,
    history: SessionHistory,
    workspace: Arc<Workspace>,
    template: String,
    mode: Mode,
    role: Role,
    binding: ChannelBinding,
}

impl Session {
    pub fn new(spec: SessionSpec) -> anyhow::Result<Self> {
        Ok(Self {
            key: spec.key,
            history: SessionHistory::default(),
            workspace: Arc::new(Workspace::new(&spec.workspace_policy)?),
            template: spec.template,
            mode: spec.mode,
            role: spec.role,
            binding: spec.binding,
        })
    }

    pub fn workspace(&self) -> Arc<Workspace> {
        Arc::clone(&self.workspace)
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

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn binding(&self) -> &ChannelBinding {
        &self.binding
    }
}
