use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use genai::chat::ChatMessage;
use tokio::sync::{mpsc, oneshot};

use crate::agent_engine::AgentEngine;
use crate::channel::SessionKey;
use crate::event::{Event, EventOrigin, Part};
use crate::output::OutputSink;
use crate::session::Session;
use crate::session::history::TurnStatus;
use crate::session::resolver::SessionResolver;

const SESSION_QUEUE: usize = 16;

pub enum SessionInput {
    Turn {
        events: Vec<Event>,
        sink: Arc<dyn OutputSink>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
}

#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::Sender<SessionInput>,
}

impl SessionHandle {
    pub async fn submit(&self, input: SessionInput) -> Result<()> {
        self.tx
            .send(input)
            .await
            .map_err(|_| anyhow!("session task is gone"))
    }
}

pub struct SessionManager {
    engine: Arc<AgentEngine>,
    resolver: SessionResolver,
    sessions: HashMap<SessionKey, SessionHandle>,
}

impl SessionManager {
    pub fn new(engine: Arc<AgentEngine>, resolver: SessionResolver) -> Self {
        Self {
            engine,
            resolver,
            sessions: HashMap::new(),
        }
    }

    pub fn ensure(&mut self, key: &SessionKey) -> Result<SessionHandle> {
        match self.sessions.entry(key.clone()) {
            Entry::Occupied(existing) => Ok(existing.get().clone()),
            Entry::Vacant(vacant) => {
                let session = Session::new(self.resolver.spec_for(key)?);
                let (tx, rx) = mpsc::channel(SESSION_QUEUE);
                tokio::spawn(run_session(self.engine.clone(), session, rx));
                let handle = SessionHandle { tx };
                vacant.insert(handle.clone());
                Ok(handle)
            }
        }
    }

    pub async fn submit(&mut self, key: &SessionKey, input: SessionInput) -> Result<()> {
        let handle = self.ensure(key)?;
        handle.submit(input).await
    }
}

async fn run_session(
    engine: Arc<AgentEngine>,
    mut session: Session,
    mut rx: mpsc::Receiver<SessionInput>,
) {
    while let Some(input) = rx.recv().await {
        match input {
            SessionInput::Turn {
                events,
                sink,
                reply,
            } => {
                let projected = project(&events);

                match engine.run_turn(&session, projected, &sink).await {
                    Ok(turn) => {
                        let outcome = match turn.status() {
                            TurnStatus::Failed(err) => Err(anyhow::Error::new(err.clone())),
                            _ => Ok(()),
                        };

                        session.append_turn(turn);

                        let _ = reply.send(outcome);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(anyhow::Error::new(error)));
                    }
                }
            }
        }
    }
}

fn project(events: &[Event]) -> ChatMessage {
    let mut text = String::new();
    for event in events {
        let EventOrigin::Platform(platform) = &event.origin else {
            continue;
        };
        for part in &platform.parts {
            if let Part::Text { text: chunk, .. } = part {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(chunk);
            }
        }
    }
    ChatMessage::user(text)
}
