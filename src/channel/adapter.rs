//! Inbound normalization port: a platform update becomes domain [`Event`]s.

use std::fmt;

use crate::channel::Channel;
use crate::event::Event;

#[derive(Debug, Clone, PartialEq)]
pub struct RawUpdate {
    pub channel: Channel,
    pub payload: serde_json::Value,
}

impl RawUpdate {
    pub fn new(channel: Channel, payload: serde_json::Value) -> Self {
        Self { channel, payload }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    Malformed { reason: String },
    MissingField { field: String },
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { reason } => write!(f, "malformed platform update: {reason}"),
            Self::MissingField { field } => {
                write!(f, "platform update is missing required field `{field}`")
            }
        }
    }
}

impl std::error::Error for AdapterError {}

pub trait ChannelAdapter: Send + Sync {
    fn normalize(&self, raw: &RawUpdate) -> Result<Vec<Event>, AdapterError>;
}

#[cfg(test)]
pub mod testing {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    pub struct MockChannelAdapter {
        results: Mutex<VecDeque<Result<Vec<Event>, AdapterError>>>,
        seen: Mutex<Vec<RawUpdate>>,
    }

    impl MockChannelAdapter {
        pub fn new(results: Vec<Result<Vec<Event>, AdapterError>>) -> Arc<Self> {
            Arc::new(Self {
                results: Mutex::new(results.into()),
                seen: Mutex::new(Vec::new()),
            })
        }

        pub fn seen(&self) -> Vec<RawUpdate> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl ChannelAdapter for MockChannelAdapter {
        fn normalize(&self, raw: &RawUpdate) -> Result<Vec<Event>, AdapterError> {
            self.seen.lock().unwrap().push(raw.clone());
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockChannelAdapter has no more scripted results")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::testing::MockChannelAdapter;
    use super::*;
    use crate::channel::DeliveryTarget;
    use crate::event::ActorRef;

    fn text_event(target: &str, text: &str) -> Event {
        Event::platform_text(
            DeliveryTarget::direct(Channel::Cli, "default", target),
            ActorRef::new("member_cli"),
            text,
        )
    }

    #[test]
    fn raw_update_new_carries_its_channel_and_payload() {
        let raw = RawUpdate::new(Channel::Telegram, json!({"update_id": 7}));

        assert_eq!(raw.channel, Channel::Telegram);
        assert_eq!(raw.payload, json!({"update_id": 7}));
    }

    #[test]
    fn malformed_error_renders_its_reason() {
        let error = AdapterError::Malformed {
            reason: "expected a JSON object".to_string(),
        };

        assert_eq!(
            error.to_string(),
            "malformed platform update: expected a JSON object"
        );
    }

    #[test]
    fn missing_field_error_renders_the_field() {
        let error = AdapterError::MissingField {
            field: "message.text".to_string(),
        };

        assert_eq!(
            error.to_string(),
            "platform update is missing required field `message.text`"
        );
    }

    #[test]
    fn mock_returns_scripted_events_and_records_the_input() {
        let events = vec![text_event("cli", "one"), text_event("cli", "two")];
        let adapter = MockChannelAdapter::new(vec![Ok(events.clone())]);

        let raw = RawUpdate::new(Channel::Cli, json!({"text": "one"}));
        let produced = adapter.normalize(&raw).unwrap();

        assert_eq!(produced, events);
        assert_eq!(adapter.seen(), vec![raw]);
    }

    #[test]
    fn mock_returns_results_in_scripted_order() {
        let adapter = MockChannelAdapter::new(vec![
            Ok(vec![text_event("cli", "first")]),
            Ok(vec![text_event("cli", "second")]),
        ]);

        let first = adapter
            .normalize(&RawUpdate::new(Channel::Cli, json!({})))
            .unwrap();
        let second = adapter
            .normalize(&RawUpdate::new(Channel::Cli, json!({})))
            .unwrap();

        assert_eq!(first, vec![text_event("cli", "first")]);
        assert_eq!(second, vec![text_event("cli", "second")]);
    }

    #[test]
    fn mock_propagates_a_scripted_error() {
        let adapter = MockChannelAdapter::new(vec![Err(AdapterError::MissingField {
            field: "chat.id".to_string(),
        })]);

        let error = adapter
            .normalize(&RawUpdate::new(Channel::Telegram, json!({})))
            .unwrap_err();

        assert_eq!(
            error,
            AdapterError::MissingField {
                field: "chat.id".to_string()
            }
        );
    }

    #[test]
    fn mock_can_report_a_dropped_update() {
        let adapter = MockChannelAdapter::new(vec![Ok(vec![])]);

        let produced = adapter
            .normalize(&RawUpdate::new(Channel::Telegram, json!({})))
            .unwrap();

        assert!(produced.is_empty());
    }

    #[test]
    fn adapter_is_object_safe() {
        let adapter = MockChannelAdapter::new(vec![]);
        let _erased: Arc<dyn ChannelAdapter> = adapter;
    }
}
