use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::access::Admission;
use crate::agent_engine::AgentEngine;
use crate::channel::SessionKey;
use crate::context;
use crate::database::Database;
use crate::database::events::{AppendOutcome, StoredEvent};
use crate::event::{Event, EventId, EventOrigin};
use crate::identity::ObservedIdentity;
use crate::media::{self, ImageInliner, MEDIA_FETCH_TIMEOUT, MediaFetcher, MediaStore};
use crate::output::OutputSink;
use crate::projection::{self, InlineMedia, Projected, Roster};
use crate::session::Session;
use crate::session::history::TurnStatus;
use crate::session::resolver::SessionResolver;

const SESSION_QUEUE: usize = 16;
/// Upper bound on how long shutdown waits for queued turns to finish.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

pub enum SessionInput {
    Turn {
        events: Vec<StoredEvent>,
        roster: Roster,
        sink: Arc<dyn OutputSink>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    Turn {
        event_id: EventId,
    },
    Duplicate {
        event_id: EventId,
    },
    /// Recorded (for audit/dedup) but the access gate refused to open a turn.
    Ignored {
        event_id: EventId,
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

/// A routed session: the cloneable submit handle plus the task that owns it.
struct SessionEntry {
    handle: SessionHandle,
    task: JoinHandle<()>,
}

/// Shared routing state. Every field is either immutable or internally locked,
/// so the manager can be called concurrently from many drivers (`&self`).
pub struct SessionManager {
    engine: Arc<AgentEngine>,
    resolver: SessionResolver,
    sessions: Mutex<HashMap<SessionKey, SessionEntry>>,
    database: Arc<Database>,
    media: Arc<MediaStore>,
    agent_id: String,
}

impl SessionManager {
    pub fn new(
        engine: Arc<AgentEngine>,
        resolver: SessionResolver,
        agent_id: impl Into<String>,
        database: Arc<Database>,
        media: Arc<MediaStore>,
    ) -> Self {
        Self {
            engine,
            resolver,
            sessions: Mutex::new(HashMap::new()),
            database,
            media,
            agent_id: agent_id.into(),
        }
    }

    /// Append an inbound event (deduping it), then route it to its session.
    pub async fn ingest(
        &self,
        mut event: Event,
        sink: Arc<dyn OutputSink>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    ) -> Result<IngestOutcome> {
        // Give every media part its stable `media://` handle before storing it;
        // the bytes are downloaded later, in the session task.
        media::tag_event(&mut event);

        let key = session_key_for(&event, &self.agent_id)?;
        // The gate runs before any turn is created; a denied event is still
        // recorded below (audit + dedup), it just never reaches a session.
        let admission = self.resolver.admit(&event);

        match self.database.append_event(&event).await? {
            AppendOutcome::Duplicate(event_id) => Ok(IngestOutcome::Duplicate { event_id }),
            AppendOutcome::Appended(event_id) => {
                self.record_identity(&event).await;

                if admission == Admission::Denied {
                    tracing::info!(target: "access", "dropped event for `{key}` (not admitted)");
                    return Ok(IngestOutcome::Ignored { event_id });
                }

                let stored = self
                    .database
                    .get_event(event_id)
                    .await?
                    .ok_or_else(|| anyhow!("event {event_id:?} vanished right after append"))?;

                let roster = self.roster_for(std::slice::from_ref(&stored)).await;

                self.submit(
                    &key,
                    SessionInput::Turn {
                        events: vec![stored],
                        roster,
                        sink,
                        reply,
                    },
                )
                .await?;

                Ok(IngestOutcome::Turn { event_id })
            }
        }
    }

    /// Best-effort: record the sender's current name. The event itself is the
    /// authority; a name we fail to record is re-observed on the next message.
    async fn record_identity(&self, event: &Event) {
        let Some(observed) = ObservedIdentity::from_event(event) else {
            return;
        };

        if let Err(error) = self.database.observe_identity(&observed).await {
            tracing::warn!(target: "identity", "recording the sender name failed: {error:#}");
        }
    }

    /// The current sender names for a batch, as the projection's roster. A
    /// failed lookup degrades to no names rather than dropping the turn.
    async fn roster_for(&self, events: &[StoredEvent]) -> Roster {
        let refs = projection::sender_refs(events);

        match self.database.current_names(&refs).await {
            Ok(names) => Roster::from_names(names),
            Err(error) => {
                tracing::warn!(target: "identity", "resolving sender names failed: {error:#}");
                Roster::default()
            }
        }
    }

    /// Get or create the session task for `key`. The lock is held only for the
    /// map lookup / insert — never across an `await`.
    async fn ensure(&self, key: &SessionKey) -> Result<SessionHandle> {
        {
            let sessions = self
                .sessions
                .lock()
                .expect("session registry mutex poisoned");
            if let Some(entry) = sessions.get(key) {
                return Ok(entry.handle.clone());
            }
        }

        // Load the transcript before spawning the session task. This is DB I/O
        // and must not run while holding the registry mutex (§5.1).
        let spec = self.resolver.spec_for(key)?;
        let history = self.database.load_history(key).await?;
        let session = Session::new_with_history(spec, history)?;

        let fetcher = key.parse().and_then(|target| {
            let channel = *target.channel();
            self.resolver.media_fetcher(channel)
        });
        let (tx, rx) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(run_session(
            self.engine.clone(),
            session,
            rx,
            self.media.clone(),
            fetcher,
            self.database.clone(),
        ));
        let handle = SessionHandle { tx };

        // Re-check under the lock: a concurrent first-touch may have won the
        // race. The loser's task sees its channel close and exits.
        let mut sessions = self
            .sessions
            .lock()
            .expect("session registry mutex poisoned");
        match sessions.entry(key.clone()) {
            Entry::Occupied(existing) => Ok(existing.get().handle.clone()),
            Entry::Vacant(vacant) => {
                vacant.insert(SessionEntry {
                    handle: handle.clone(),
                    task,
                });
                Ok(handle)
            }
        }
    }

    async fn submit(&self, key: &SessionKey, input: SessionInput) -> Result<()> {
        let handle = self.ensure(key).await?;
        handle.submit(input).await
    }

    /// Stop accepting input and wait for every session task to finish the turns
    /// already queued. Draining the map drops each `SessionHandle`, closing its
    /// channel, so a `run_session` loop exits once its queue is empty.
    ///
    /// Call this only after the drivers have stopped, otherwise a concurrent
    /// `submit` could still be holding a sender when the map is drained.
    pub async fn shutdown(&self) {
        let tasks: Vec<JoinHandle<()>> = {
            let mut sessions = self
                .sessions
                .lock()
                .expect("session registry mutex poisoned");
            sessions.drain().map(|(_, entry)| entry.task).collect()
        };

        for mut task in tasks {
            match tokio::time::timeout(DRAIN_TIMEOUT, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!("session task failed: {error}"),
                Err(_) => {
                    tracing::warn!("session task did not drain within {DRAIN_TIMEOUT:?}; aborting");
                    task.abort();
                }
            }
        }
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
    media: Arc<MediaStore>,
    fetcher: Option<Arc<dyn MediaFetcher>>,
    database: Arc<Database>,
) {
    while let Some(input) = rx.recv().await {
        match input {
            SessionInput::Turn {
                events,
                roster,
                sink,
                reply,
            } => {
                // Download any media the batch references before the turn, so
                // `read` finds it. Off the poll loop, and best-effort.
                if let Some(fetcher) = &fetcher {
                    media::ensure_loaded(&events, fetcher.as_ref(), &media, MEDIA_FETCH_TIMEOUT)
                        .await;
                }

                // A vision-capable model gets images inlined; anything else (or
                // a model without the flag) keeps the `media://` text reference.
                let inliner: Option<Box<dyn InlineMedia>> = engine
                    .vision()
                    .then(|| Box::new(ImageInliner::new(media.clone())) as Box<dyn InlineMedia>);

                let Projected {
                    message,
                    source_event_id,
                } = projection::project_with(&events, &roster, inliner.as_deref());

                tracing::debug!(
                    target: "projection",
                    "projected message ({} part(s)):\n{}",
                    message.content.len(),
                    message.content.texts().join("\n")
                );

                // Nothing model-facing in the batch (e.g. a service message):
                // record it, but do not open a turn.
                let Some(source_event_id) = source_event_id else {
                    let _ = reply.send(Ok(()));
                    continue;
                };

                let carrier = context::carrier(&events, &roster, session.turn_count());
                if let Some(carrier) = &carrier {
                    tracing::debug!(
                        target: "context",
                        "runtime-context:\n{}",
                        carrier.content.texts().join("")
                    );
                }

                match engine
                    .run_turn(&session, message, carrier, Some(source_event_id), &sink)
                    .await
                {
                    Ok(turn) => {
                        let outcome = match turn.status() {
                            TurnStatus::Failed(err) => Err(anyhow::Error::new(err.clone())),
                            _ => Ok(()),
                        };

                        let turn_idx = session.turn_count();
                        if let Err(error) =
                            database.append_turn(session.key(), turn_idx, &turn).await
                        {
                            tracing::error!(
                                target: "transcript",
                                "persisting turn {turn_idx} for `{}` failed: {error:#}",
                                session.key().as_str()
                            );
                        }

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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::access::AccessPolicy;
    use crate::agent_engine::AgentEngine;
    use crate::channel::registry::ChannelRegistry;
    use crate::channel::registry::testing::MockRuntime;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::config::channel::ChannelTemplates;
    use crate::config::model_provider::testing::{ScriptedModel, text_events};
    use crate::config::system_prompt::SystemPromptConfig;
    use crate::event::{ActorRef, DedupKey, Media, MediaHandle, Part};
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
            "webui".to_string(),
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

    async fn session_manager(model: Arc<ScriptedModel>) -> SessionManager {
        let database = Arc::new(Database::in_memory().await.unwrap());
        session_manager_with_database(model, database).await
    }

    async fn session_manager_with_database(
        model: Arc<ScriptedModel>,
        database: Arc<Database>,
    ) -> SessionManager {
        let config = SystemPromptConfig {
            character_path: PathBuf::from("./characters/default.md"),
        };
        // Any loadable template works for these flow tests.
        let system_prompt_manager =
            SystemPromptManager::new(&config, ["telegram_direct".to_string()]).unwrap();

        let mut tool_router = ToolRouter::new();
        tool_router.register_builtin_tools().unwrap();

        let media =
            Arc::new(MediaStore::new(crate::workspace::temp_base("media-manager")).unwrap());

        let engine = Arc::new(AgentEngine::new(
            model,
            media.clone(),
            tool_router,
            system_prompt_manager,
            100,
        ));

        let mut templates = HashMap::new();
        templates.insert(Channel::Webui, ChannelTemplates::uniform("telegram_direct"));

        let mut registry = ChannelRegistry::new();
        registry.register(Channel::Webui, MockRuntime::live());

        let resolver = SessionResolver::new(
            crate::workspace::temp_base("session"),
            templates,
            Arc::new(registry),
            Permissions::default(),
            AccessPolicy::default(),
        );

        SessionManager::new(engine, resolver, "main", database, media)
    }

    async fn enqueue(
        manager: &SessionManager,
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
        let manager = session_manager(model.clone()).await;

        let (reply, sink) = enqueue(&manager, event("hello")).await;
        let outcome = reply.await.unwrap();

        assert!(outcome.is_ok(), "turn failed: {:?}", outcome.err());
        assert_eq!(sink.content(), "hello back");
        assert!(sink.finished());
    }

    #[tokio::test]
    async fn distinct_keys_keep_their_histories_separate() {
        let model = ScriptedModel::new(vec![text_events("REPLY_ALPHA"), text_events("REPLY_BETA")]);
        let manager = session_manager(model.clone()).await;

        let (alpha, _) = enqueue(&manager, event_to("alpha", "USER_ALPHA")).await;
        assert!(alpha.await.unwrap().is_ok());
        let (beta, _) = enqueue(&manager, event_to("beta", "USER_BETA")).await;
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
        let manager = session_manager(model.clone()).await;

        let (first, _) = enqueue(&manager, event("USER_ONE")).await;
        let (second, _) = enqueue(&manager, event("USER_TWO")).await;
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
    async fn the_runtime_context_carrier_is_turn_scoped() {
        let model = ScriptedModel::new(vec![text_events("ONE"), text_events("TWO")]);
        let manager = session_manager(model.clone()).await;

        let (first, _) = enqueue(&manager, event("USER_ONE")).await;
        assert!(first.await.unwrap().is_ok());
        let (second, _) = enqueue(&manager, event("USER_TWO")).await;
        assert!(second.await.unwrap().is_ok());

        // The carrier reaches the model on its own turn...
        assert!(request_json(&model, 0).contains("turn=\\\"t0\\\""));
        // ...and is stripped the next turn: it rides outside the persisted steps.
        let second = request_json(&model, 1);
        assert!(!second.contains("turn=\\\"t0\\\""), "{second}");
        assert!(second.contains("turn=\\\"t1\\\""), "{second}");
    }

    #[tokio::test]
    async fn a_replayed_event_is_dropped_and_starts_no_turn() {
        let model = ScriptedModel::new(vec![text_events("REPLY")]);
        let manager = session_manager(model.clone()).await;

        let (reply, _) = enqueue(&manager, replayed_event("hi", "42")).await;
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

    #[tokio::test]
    async fn an_unlisted_dm_is_dropped_without_a_turn() {
        let model = ScriptedModel::new(vec![text_events("should not run")]);
        let manager = session_manager(model.clone()).await;

        let event = Event::platform_text(
            DeliveryTarget::direct(Channel::Telegram, "default", "999"),
            ActorRef::new("member_stranger"),
            "999".to_string(),
            "hello",
        );
        let (reply, _reply_rx) = oneshot::channel();

        let outcome = manager
            .ingest(event, Arc::new(RecordingSink::default()), reply)
            .await
            .unwrap();

        assert!(matches!(outcome, IngestOutcome::Ignored { .. }));
        assert_eq!(model.requests().len(), 0);
    }

    #[tokio::test]
    async fn shutdown_drains_a_queued_turn_before_returning() {
        let model = ScriptedModel::text_reply("hello back");
        let manager = session_manager(model.clone()).await;

        let (reply, sink) = enqueue(&manager, event("hello")).await;

        manager.shutdown().await;

        assert!(reply.await.unwrap().is_ok());
        assert_eq!(sink.content(), "hello back");
    }

    #[tokio::test]
    async fn a_restarted_session_replays_its_transcript() {
        let database = Arc::new(Database::in_memory().await.unwrap());

        let first_model = ScriptedModel::new(vec![text_events("REPLY_ONE")]);
        let first = session_manager_with_database(first_model.clone(), database.clone()).await;
        let (first_reply, _) = enqueue(&first, event("USER_ONE")).await;
        assert!(first_reply.await.unwrap().is_ok());
        first.shutdown().await;

        let second_model = ScriptedModel::new(vec![text_events("REPLY_TWO")]);
        let second = session_manager_with_database(second_model.clone(), database.clone()).await;
        let (second_reply, _) = enqueue(&second, event("USER_TWO")).await;
        assert!(second_reply.await.unwrap().is_ok());

        let second_request = request_json(&second_model, 0);
        assert!(second_request.contains("USER_ONE"), "{second_request}");
        assert!(second_request.contains("REPLY_ONE"), "{second_request}");
        assert!(second_request.contains("USER_TWO"), "{second_request}");
    }

    #[tokio::test]
    async fn ingest_records_the_sender_name() {
        let model = ScriptedModel::text_reply("hello back");
        let manager = session_manager(model.clone()).await;

        let mut event = event("hello");
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.envelope.sender_name = Some("Local User".to_string());

        let (reply, _sink) = enqueue(&manager, event).await;
        assert!(reply.await.unwrap().is_ok());

        let names = manager
            .database
            .current_names(&[ActorRef::new(LOCAL_SENDER)])
            .await
            .unwrap();

        assert_eq!(
            names,
            HashMap::from([(ActorRef::new(LOCAL_SENDER), "Local User".to_string())])
        );
    }

    #[tokio::test]
    async fn ingest_tags_media_parts_with_a_media_uri() {
        let model = ScriptedModel::text_reply("ok");
        let manager = session_manager(model).await;

        let mut event = event("look");
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.parts = vec![Part::Photo {
            media: Media::new(
                MediaHandle("file".to_string()),
                MediaHandle("stable".to_string()),
                Some("image/jpeg".to_string()),
            ),
            caption: None,
        }];

        let (reply, _) = enqueue(&manager, event).await;
        assert!(reply.await.unwrap().is_ok());

        let stored = manager
            .database
            .get_event(EventId(1))
            .await
            .unwrap()
            .unwrap();
        let EventOrigin::Platform(platform) = &stored.event.origin else {
            panic!("expected a platform event");
        };
        let Part::Photo { media, .. } = &platform.parts[0] else {
            panic!("expected a photo part");
        };

        // The store key comes from the stable id, but the download handle is the
        // original `file_id`.
        let expected = MediaStore::uri(&MediaHandle("stable".to_string()), Some("image/jpeg"));
        assert_eq!(media.handle, MediaHandle("file".to_string()));
        assert_eq!(media.uri.as_deref(), Some(expected.as_str()));
    }
}
