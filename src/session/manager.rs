use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use genai::chat::ChatMessage;
use tokio::sync::{mpsc, oneshot};

use crate::agent_engine::AgentEngine;
use crate::channel::SessionKey;
use crate::event::{Event, EventId, EventOrigin, Part};
use crate::event_store::{AppendOutcome, EventStore, StoredEvent};
use crate::output::OutputSink;
use crate::session::Session;
use crate::session::history::TurnStatus;
use crate::session::resolver::SessionResolver;

const SESSION_QUEUE: usize = 16;

pub enum SessionInput {
    Turn {
        events: Vec<StoredEvent>,
        sink: Arc<dyn OutputSink>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    Turn { event_id: EventId },
    Duplicate { event_id: EventId },
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
    event_store: EventStore,
    agent_id: String,
}

impl SessionManager {
    pub fn new(
        engine: Arc<AgentEngine>,
        resolver: SessionResolver,
        agent_id: impl Into<String>,
        event_store: EventStore,
    ) -> Self {
        Self {
            engine,
            resolver,
            sessions: HashMap::new(),
            event_store,
            agent_id: agent_id.into(),
        }
    }

    /// Append an inbound event (deduping it), then route it to its session.
    pub async fn ingest(
        &mut self,
        event: Event,
        sink: Arc<dyn OutputSink>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    ) -> Result<IngestOutcome> {
        let key = session_key_for(&event, &self.agent_id)?;

        match self.event_store.append(&event)? {
            AppendOutcome::Duplicate(event_id) => Ok(IngestOutcome::Duplicate { event_id }),
            AppendOutcome::Appended(event_id) => {
                let stored = self
                    .event_store
                    .get(event_id)?
                    .ok_or_else(|| anyhow!("event {event_id:?} vanished right after append"))?;

                self.submit(
                    &key,
                    SessionInput::Turn {
                        events: vec![stored],
                        sink,
                        reply,
                    },
                )
                .await?;

                Ok(IngestOutcome::Turn { event_id })
            }
        }
    }

    fn ensure(&mut self, key: &SessionKey) -> Result<SessionHandle> {
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

    async fn submit(&mut self, key: &SessionKey, input: SessionInput) -> Result<()> {
        let handle = self.ensure(key)?;
        handle.submit(input).await
    }
}

fn session_key_for(event: &Event, agent_id: &str) -> Result<SessionKey> {
    match &event.origin {
        EventOrigin::Platform(platform) => Ok(platform.target.to_session_key(agent_id)),
        EventOrigin::Runtime(runtime) => Err(anyhow!(
            "runtime event `{:?}` has no delivery target yet",
            runtime.kind
        )),
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
                let (message, source_event_id) = project(&events);

                match engine
                    .run_turn(&session, message, source_event_id, &sink)
                    .await
                {
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

fn project(events: &[StoredEvent]) -> (ChatMessage, Option<EventId>) {
    let mut text = String::new();
    let mut source_event_id = None;

    for stored in events {
        let EventOrigin::Platform(platform) = &stored.event.origin else {
            continue;
        };

        if source_event_id.is_none() {
            source_event_id = Some(stored.id);
        }

        for part in &platform.parts {
            if let Part::Text { text: chunk, .. } = part {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(chunk);
            }
        }
    }

    (ChatMessage::user(text), source_event_id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::agent_engine::AgentEngine;
    use crate::channel::registry::ChannelRegistry;
    use crate::channel::registry::testing::MockRuntime;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::config::model_provider::testing::{ScriptedModel, text_events};
    use crate::config::system_prompt::SystemPromptConfig;
    use crate::event::{ActorRef, DedupKey};
    use crate::permissions::Permissions;
    use crate::system_prompt::SystemPromptManager;
    use crate::tools::ToolRouter;

    const LOCAL_SENDER: &str = "member_local";

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

    fn event_to(target: &str, text: &str) -> Event {
        Event::platform_text(
            DeliveryTarget::direct(Channel::Webui, "default", target),
            ActorRef::new(LOCAL_SENDER),
            text,
        )
    }

    fn event(text: &str) -> Event {
        event_to("webui", text)
    }

    fn replayed_event(text: &str, platform_event_id: &str) -> Event {
        let mut built = event(text);

        let EventOrigin::Platform(platform) = &mut built.origin else {
            panic!("helper must build a platform event");
        };
        platform.dedup = Some(DedupKey {
            platform: Channel::Webui,
            platform_event_id: platform_event_id.to_string(),
        });

        built
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
        templates.insert(Channel::Webui, "agent".to_string());

        let mut registry = ChannelRegistry::new();
        registry.register(Channel::Webui, MockRuntime::live());

        let resolver = SessionResolver::new(
            PathBuf::from("/work"),
            templates,
            Arc::new(registry),
            Permissions::default(),
        );

        SessionManager::new(engine, resolver, "main", EventStore::in_memory().unwrap())
    }

    async fn enqueue(
        manager: &mut SessionManager,
        event: Event,
    ) -> (oneshot::Receiver<anyhow::Result<()>>, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let (reply, reply_rx) = oneshot::channel();

        manager.ingest(event, sink.clone(), reply).await.unwrap();

        (reply_rx, sink)
    }

    fn request_json(model: &ScriptedModel, index: usize) -> String {
        serde_json::to_string(&model.requests()[index]).unwrap()
    }

    #[tokio::test]
    async fn turn_streams_content_to_the_sink() {
        let model = ScriptedModel::text_reply("hello back");
        let mut manager = session_manager(model.clone());

        let (reply, sink) = enqueue(&mut manager, event("hello")).await;
        let outcome = reply.await.unwrap();

        assert!(outcome.is_ok(), "turn failed: {:?}", outcome.err());
        assert_eq!(sink.content(), "hello back");
        assert!(sink.finished());
    }

    #[tokio::test]
    async fn distinct_keys_keep_their_histories_separate() {
        let model = ScriptedModel::new(vec![text_events("REPLY_ALPHA"), text_events("REPLY_BETA")]);
        let mut manager = session_manager(model.clone());

        let (alpha, _) = enqueue(&mut manager, event_to("alpha", "USER_ALPHA")).await;
        assert!(alpha.await.unwrap().is_ok());
        let (beta, _) = enqueue(&mut manager, event_to("beta", "USER_BETA")).await;
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

        let (first, _) = enqueue(&mut manager, event("USER_ONE")).await;
        let (second, _) = enqueue(&mut manager, event("USER_TWO")).await;
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

    #[tokio::test]
    async fn a_replayed_event_is_dropped_and_starts_no_turn() {
        let model = ScriptedModel::new(vec![text_events("REPLY")]);
        let mut manager = session_manager(model.clone());

        let (reply, _) = enqueue(&mut manager, replayed_event("hi", "42")).await;
        assert!(reply.await.unwrap().is_ok());

        let (replay_tx, _replay_rx) = oneshot::channel();
        let outcome = manager
            .ingest(
                replayed_event("hi", "42"),
                Arc::new(RecordingSink::default()),
                replay_tx,
            )
            .await
            .unwrap();

        assert_eq!(
            outcome,
            IngestOutcome::Duplicate {
                event_id: EventId(1)
            }
        );
        assert_eq!(model.requests().len(), 1);
    }
}
