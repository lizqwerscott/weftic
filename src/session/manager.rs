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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::agent_engine::AgentEngine;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::config::model_provider::testing::{ScriptedModel, text_events};
    use crate::config::system_prompt::SystemPromptConfig;
    use crate::event::ActorRef;
    use crate::system_prompt::SystemPromptManager;
    use crate::tools::ToolRouter;

    const CLI_SENDER: &str = "member_cli";

    #[derive(Default)]
    struct RecordingSink {
        content: Mutex<String>,
        finished: Mutex<bool>,
    }

    impl RecordingSink {
        fn content(&self) -> String {
            self.content.lock().unwrap().clone()
        }

        fn finished(&self) -> bool {
            *self.finished.lock().unwrap()
        }
    }

    impl OutputSink for RecordingSink {
        fn on_content(&self, chunk: &str) {
            self.content.lock().unwrap().push_str(chunk);
        }

        fn finish(&self) {
            *self.finished.lock().unwrap() = true;
        }
    }

    fn cli_target() -> DeliveryTarget {
        DeliveryTarget::direct(Channel::Cli, "default", "cli")
    }

    fn cli_key() -> SessionKey {
        cli_target().to_session_key("main")
    }

    fn key_for(target: &str) -> SessionKey {
        DeliveryTarget::direct(Channel::Cli, "default", target).to_session_key("main")
    }

    fn event(text: &str) -> Event {
        Event::platform_text(cli_target(), ActorRef::new(CLI_SENDER), text)
    }

    fn session_manager(model: Arc<ScriptedModel>) -> SessionManager {
        let config = SystemPromptConfig {
            character_path: PathBuf::from("./characters/default.md"),
        };
        let system_prompt_manager =
            SystemPromptManager::new(&config, ["agent".to_string()]).unwrap();

        let mut tool_router = ToolRouter::new();
        tool_router.register_builtin_tools().unwrap();

        let engine = Arc::new(AgentEngine::new(
            model,
            tool_router,
            system_prompt_manager,
            100,
        ));

        let mut templates = HashMap::new();
        templates.insert(Channel::Cli, "agent".to_string());
        let resolver = SessionResolver::new(PathBuf::from("/work"), templates);

        SessionManager::new(engine, resolver)
    }

    async fn enqueue(
        manager: &mut SessionManager,
        key: &SessionKey,
        text: &str,
    ) -> (oneshot::Receiver<anyhow::Result<()>>, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let (reply, reply_rx) = oneshot::channel();

        manager
            .submit(
                key,
                SessionInput::Turn {
                    events: vec![event(text)],
                    sink: sink.clone(),
                    reply,
                },
            )
            .await
            .unwrap();

        (reply_rx, sink)
    }

    fn request_json(model: &ScriptedModel, index: usize) -> String {
        serde_json::to_string(&model.requests()[index]).unwrap()
    }

    #[tokio::test]
    async fn turn_streams_content_to_the_sink() {
        let model = ScriptedModel::text_reply("hello back");
        let mut manager = session_manager(model.clone());

        let (reply, sink) = enqueue(&mut manager, &cli_key(), "hello").await;
        let outcome = reply.await.unwrap();

        assert!(outcome.is_ok(), "turn failed: {:?}", outcome.err());
        assert_eq!(sink.content(), "hello back");
        assert!(sink.finished());
    }

    #[tokio::test]
    async fn distinct_keys_keep_their_histories_separate() {
        let model = ScriptedModel::new(vec![text_events("REPLY_ALPHA"), text_events("REPLY_BETA")]);
        let mut manager = session_manager(model.clone());

        let (alpha, _) = enqueue(&mut manager, &key_for("alpha"), "USER_ALPHA").await;
        assert!(alpha.await.unwrap().is_ok());
        let (beta, _) = enqueue(&mut manager, &key_for("beta"), "USER_BETA").await;
        assert!(beta.await.unwrap().is_ok());

        assert_eq!(model.requests().len(), 2);

        let second = request_json(&model, 1);
        assert!(second.contains("USER_BETA"));
        assert!(!second.contains("USER_ALPHA"));
        assert!(!second.contains("REPLY_ALPHA"));
    }

    #[tokio::test]
    async fn same_key_processes_turns_in_order_and_continues_the_history() {
        let model = ScriptedModel::new(vec![text_events("REPLY_ONE"), text_events("REPLY_TWO")]);
        let mut manager = session_manager(model.clone());
        let key = cli_key();

        let (first, _) = enqueue(&mut manager, &key, "USER_ONE").await;
        let (second, _) = enqueue(&mut manager, &key, "USER_TWO").await;
        assert!(first.await.unwrap().is_ok());
        assert!(second.await.unwrap().is_ok());

        assert_eq!(model.requests().len(), 2);

        let first_request = request_json(&model, 0);
        assert!(first_request.contains("USER_ONE"));
        assert!(!first_request.contains("USER_TWO"));

        let second_request = request_json(&model, 1);
        assert!(second_request.contains("USER_ONE"));
        assert!(second_request.contains("REPLY_ONE"));
        assert!(second_request.contains("USER_TWO"));
    }
}
