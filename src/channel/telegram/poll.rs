//! Telegram long-poll transport.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use crate::channel::Channel;
use crate::channel::adapter::{ChannelAdapter, RawUpdate};
use crate::channel::telegram::TelegramAdapter;
use crate::event::{Event, EventOrigin, Part};
use crate::output::OutputSink;
use crate::session::manager::{IngestOutcome, SessionManager};

const API_BASE: &str = "https://api.telegram.org";
/// Long-poll timeout handed to Telegram, in seconds.
const POLL_TIMEOUT_SECS: u32 = 30;
/// Backoff after a failed poll before trying again.
const ERROR_BACKOFF: Duration = Duration::from_secs(5);

pub type BoxedUpdatesFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait UpdateSource: Send + Sync {
    fn get_updates(&self, offset: i32) -> BoxedUpdatesFuture<'_, Result<Vec<Value>>>;
}

pub struct TelegramUpdates {
    client: reqwest::Client,
    token: String,
}

impl TelegramUpdates {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.into(),
        }
    }
}

impl UpdateSource for TelegramUpdates {
    fn get_updates(&self, offset: i32) -> BoxedUpdatesFuture<'_, Result<Vec<Value>>> {
        Box::pin(async move {
            let url = format!(
                "{API_BASE}/bot{}/getUpdates?offset={offset}&timeout={POLL_TIMEOUT_SECS}",
                self.token
            );

            let body = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(|error| anyhow!("calling telegram getUpdates: {}", error.without_url()))?
                .text()
                .await
                .map_err(|error| {
                    anyhow!(
                        "reading telegram getUpdates response: {}",
                        error.without_url()
                    )
                })?;

            parse_updates(&body)
        })
    }
}

pub struct TelegramPoller {
    source: Arc<dyn UpdateSource>,
    adapter: TelegramAdapter,
}

impl TelegramPoller {
    pub fn new(token: impl Into<String>, account_id: impl Into<String>) -> Self {
        Self::with_source(Arc::new(TelegramUpdates::new(token)), account_id)
    }

    pub fn with_source(source: Arc<dyn UpdateSource>, account_id: impl Into<String>) -> Self {
        Self {
            source,
            adapter: TelegramAdapter::new(account_id),
        }
    }

    pub fn normalize_batch(&self, updates: &[Value]) -> Vec<Event> {
        let mut events = Vec::new();

        for update in updates {
            let raw = RawUpdate::new(Channel::Telegram, update.clone());
            match self.adapter.normalize(&raw) {
                Ok(mut produced) => events.append(&mut produced),
                Err(error) => warn!(target: "telegram", "update dropped: {error}"),
            }
        }

        events
    }

    pub async fn poll_once(&self, offset: i32) -> Result<(i32, Vec<Event>)> {
        let updates = self.source.get_updates(offset).await?;

        if updates.is_empty() {
            return Ok((offset, Vec::new()));
        }

        let next = next_offset(&updates, offset);
        Ok((next, self.normalize_batch(&updates)))
    }

    pub async fn run(self, mut manager: SessionManager, sink: Arc<dyn OutputSink>) -> Result<()> {
        let mut offset: i32 = 0;

        info!(target: "telegram", "poller started");

        loop {
            match self.poll_once(offset).await {
                Ok((next, events)) => {
                    offset = next;
                    for event in events {
                        info!(target: "telegram", "received {}", summarize(&event));
                        deliver(&mut manager, event, &sink).await;
                    }
                }
                Err(error) => {
                    warn!(target: "telegram", "poll failed: {error:#}");
                    tokio::time::sleep(ERROR_BACKOFF).await;
                }
            }
        }
    }
}

async fn deliver(manager: &mut SessionManager, event: Event, sink: &Arc<dyn OutputSink>) {
    let (reply, reply_rx) = oneshot::channel();

    match manager.ingest(event, sink.clone(), reply).await {
        Ok(IngestOutcome::Turn { .. }) => match reply_rx.await {
            Ok(Ok(())) => info!(target: "telegram", "turn finished"),
            Ok(Err(error)) => error!(target: "telegram", "turn failed: {error:#}"),
            Err(_) => error!(target: "telegram", "session task ended before replying"),
        },
        Ok(IngestOutcome::Duplicate { .. }) => {
            debug!(target: "telegram", "duplicate update skipped");
        }
        Err(error) => error!(target: "telegram", "ingest failed: {error:#}"),
    }
}

fn summarize(event: &Event) -> String {
    let EventOrigin::Platform(platform) = &event.origin else {
        return "runtime event".to_string();
    };

    let text = platform
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");

    let event_id = platform
        .dedup
        .as_ref()
        .map(|dedup| dedup.platform_event_id.as_str())
        .unwrap_or("-");

    format!(
        "{}:{} update {event_id} `{text}`",
        platform.target.channel(),
        platform.target.target_id()
    )
}

fn next_offset(updates: &[Value], current: i32) -> i32 {
    updates
        .iter()
        .filter_map(update_id)
        .max()
        .map(|highest| highest.saturating_add(1) as i32)
        .unwrap_or(current)
}

fn update_id(update: &Value) -> Option<i64> {
    update.get("update_id").and_then(Value::as_i64)
}

fn parse_updates(body: &str) -> Result<Vec<Value>> {
    let response: UpdatesResponse =
        serde_json::from_str(body).context("decoding telegram getUpdates response")?;

    if !response.ok {
        return Err(anyhow!(
            "telegram getUpdates failed: {}",
            response
                .description
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }

    Ok(response.result.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
struct UpdatesResponse {
    ok: bool,
    result: Option<Vec<Value>>,
    description: Option<String>,
}

#[cfg(test)]
pub mod testing {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    pub struct ScriptedUpdates {
        batches: Mutex<VecDeque<Result<Vec<Value>>>>,
        offsets: Mutex<Vec<i32>>,
    }

    impl ScriptedUpdates {
        pub fn new(batches: Vec<Result<Vec<Value>>>) -> Arc<Self> {
            Arc::new(Self {
                batches: Mutex::new(batches.into()),
                offsets: Mutex::new(Vec::new()),
            })
        }

        pub fn offsets(&self) -> Vec<i32> {
            self.offsets.lock().unwrap().clone()
        }
    }

    impl UpdateSource for ScriptedUpdates {
        fn get_updates(&self, offset: i32) -> BoxedUpdatesFuture<'_, Result<Vec<Value>>> {
            Box::pin(async move {
                self.offsets.lock().unwrap().push(offset);
                self.batches
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("ScriptedUpdates has no more scripted batches")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use serde_json::json;

    use super::testing::ScriptedUpdates;
    use super::*;
    use crate::agent_engine::AgentEngine;
    use crate::channel::DeliveryTarget;
    use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
    use crate::channel::registry::{ChannelRegistry, ChannelRuntime};
    use crate::channel::sender::ChannelSender;
    use crate::channel::sender::testing::{MockChannelSender, MockMessage};
    use crate::config::model_provider::testing::{ScriptedModel, text_events, tool_call_events};
    use crate::config::system_prompt::SystemPromptConfig;
    use crate::event::EventOrigin;
    use crate::event_store::EventStore;
    use crate::output::NullSink;
    use crate::permissions::Permissions;
    use crate::session::resolver::SessionResolver;
    use crate::system_prompt::SystemPromptManager;
    use crate::tools::ToolRouter;

    fn private_text_update() -> Value {
        json!({
            "update_id": 500,
            "message": {
                "message_id": 1,
                "from": {"id": 123456, "is_bot": false, "first_name": "Ali"},
                "date": 1_700_000_000,
                "chat": {"id": 123456, "type": "private", "first_name": "Ali"},
                "text": "hi"
            }
        })
    }

    fn poller(batches: Vec<Result<Vec<Value>>>) -> (TelegramPoller, Arc<ScriptedUpdates>) {
        let source = ScriptedUpdates::new(batches);
        (
            TelegramPoller::with_source(source.clone(), "default"),
            source,
        )
    }

    #[test]
    fn normalize_batch_turns_raw_updates_into_events() {
        let (poller, _) = poller(vec![]);

        let events = poller.normalize_batch(&[private_text_update()]);

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(
            platform.target,
            DeliveryTarget::direct(Channel::Telegram, "default", "123456")
        );
    }

    #[test]
    fn normalize_batch_skips_updates_without_a_message() {
        let (poller, _) = poller(vec![]);

        let events = poller.normalize_batch(&[json!({"update_id": 1, "poll": {}})]);

        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn poll_once_returns_the_next_offset_and_events() {
        let (poller, source) = poller(vec![Ok(vec![private_text_update()])]);

        let (next, events) = poller.poll_once(0).await.unwrap();

        assert_eq!(next, 501);
        assert_eq!(events.len(), 1);
        assert_eq!(source.offsets(), vec![0]);
    }

    #[tokio::test]
    async fn an_empty_batch_keeps_the_offset() {
        let (poller, _) = poller(vec![Ok(vec![])]);

        let (next, events) = poller.poll_once(7).await.unwrap();

        assert_eq!(next, 7);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn a_source_error_propagates_so_the_loop_can_back_off() {
        let (poller, _) = poller(vec![Err(anyhow!("connection reset"))]);

        let error = poller.poll_once(0).await.unwrap_err();

        assert_eq!(error.to_string(), "connection reset");
    }

    #[test]
    fn next_offset_is_one_past_the_highest_update_id() {
        let updates = vec![json!({"update_id": 10}), json!({"update_id": 12})];
        assert_eq!(next_offset(&updates, 5), 13);
    }

    #[test]
    fn next_offset_keeps_the_current_offset_for_an_empty_batch() {
        assert_eq!(next_offset(&[], 5), 5);
    }

    #[test]
    fn parse_updates_reads_an_ok_result() {
        let updates = parse_updates(r#"{"ok":true,"result":[{"update_id":1}]}"#).unwrap();
        assert_eq!(updates, vec![json!({"update_id": 1})]);
    }

    #[test]
    fn parse_updates_treats_a_missing_result_as_empty() {
        let updates = parse_updates(r#"{"ok":true}"#).unwrap();
        assert!(updates.is_empty());
    }

    #[test]
    fn parse_updates_rejects_an_error_response() {
        let error = parse_updates(r#"{"ok":false,"description":"Unauthorized"}"#).unwrap_err();

        assert_eq!(
            error.to_string(),
            "telegram getUpdates failed: Unauthorized"
        );
    }

    #[test]
    fn parse_updates_reports_a_non_json_body() {
        let error = parse_updates("<html>502 Bad Gateway</html>").unwrap_err();

        assert_eq!(
            format!("{error:#}"),
            "decoding telegram getUpdates response: expected value at line 1 column 1"
        );
    }

    struct MockTelegramRuntime {
        sender: Arc<MockChannelSender>,
    }

    impl ChannelRuntime for MockTelegramRuntime {
        fn capabilities(&self) -> ChannelCapabilities {
            ChannelCapabilities::new(StreamMode::Off, ReplyMode::MessageTool)
        }

        fn sender(&self, _target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>> {
            Some(self.sender.clone())
        }
    }

    fn telegram_manager(
        model: Arc<ScriptedModel>,
        sender: Arc<MockChannelSender>,
    ) -> SessionManager {
        let config = SystemPromptConfig {
            character_path: PathBuf::from("./characters/default.md"),
        };
        let system_prompt_manager =
            SystemPromptManager::new(&config, ["telegram".to_string()]).unwrap();

        let mut tool_router = ToolRouter::new();
        tool_router.register_builtin_tools().unwrap();

        let engine = Arc::new(AgentEngine::new(
            model,
            tool_router,
            system_prompt_manager,
            100,
        ));

        let mut templates = HashMap::new();
        templates.insert(Channel::Telegram, "telegram".to_string());

        let mut registry = ChannelRegistry::new();
        registry.register(
            Channel::Telegram,
            Arc::new(MockTelegramRuntime {
                sender: sender.clone(),
            }),
        );

        let permissions = Permissions {
            owner: vec!["telegram:123456".to_string()],
        };
        let resolver = SessionResolver::new(
            PathBuf::from("/work"),
            templates,
            Arc::new(registry),
            permissions,
        );

        SessionManager::new(engine, resolver, "main", EventStore::in_memory().unwrap())
    }

    #[tokio::test]
    async fn a_telegram_update_runs_a_turn_that_replies_through_the_message_tool() {
        let model = ScriptedModel::new(vec![
            tool_call_events(
                "call_1",
                "message",
                json!({"action": "send", "text": "hi back"}),
            ),
            text_events("done"),
        ]);
        let sender = MockChannelSender::new();
        let mut manager = telegram_manager(model, sender.clone());

        let (poller, _) = poller(vec![Ok(vec![private_text_update()])]);
        let (_, events) = poller.poll_once(0).await.unwrap();
        let sink: Arc<dyn OutputSink> = Arc::new(NullSink);

        for event in events {
            deliver(&mut manager, event, &sink).await;
        }

        assert_eq!(
            sender.sent(),
            vec![MockMessage {
                id: "msg_1".to_string(),
                text: "hi back".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn a_replayed_update_does_not_trigger_a_second_turn() {
        let model = ScriptedModel::new(vec![
            tool_call_events(
                "call_1",
                "message",
                json!({"action": "send", "text": "hi back"}),
            ),
            text_events("done"),
        ]);
        let sender = MockChannelSender::new();
        let mut manager = telegram_manager(model.clone(), sender.clone());

        let (poller, _) = poller(vec![
            Ok(vec![private_text_update()]),
            Ok(vec![private_text_update()]),
        ]);
        let sink: Arc<dyn OutputSink> = Arc::new(NullSink);

        for _ in 0..2 {
            let (_, events) = poller.poll_once(0).await.unwrap();
            for event in events {
                deliver(&mut manager, event, &sink).await;
            }
        }

        assert_eq!(model.requests().len(), 2);
        assert_eq!(sender.sent().len(), 1);
    }

    #[tokio::test]
    async fn a_text_only_reply_is_delivered_by_the_fallback() {
        let model = ScriptedModel::text_reply("hi back");
        let sender = MockChannelSender::new();
        let mut manager = telegram_manager(model, sender.clone());

        let (poller, _) = poller(vec![Ok(vec![private_text_update()])]);
        let (_, events) = poller.poll_once(0).await.unwrap();
        let sink: Arc<dyn OutputSink> = Arc::new(NullSink);

        for event in events {
            deliver(&mut manager, event, &sink).await;
        }

        assert_eq!(
            sender.sent(),
            vec![MockMessage {
                id: "msg_1".to_string(),
                text: "hi back".to_string(),
            }]
        );
    }
}
