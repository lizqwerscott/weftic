//! Inbound intake: normalize a raw platform update and append its events.

use crate::channel::adapter::{ChannelAdapter, RawUpdate};
use crate::event::{Event, EventId, EventOrigin, Part};
use crate::event_store::{AppendOutcome, EventStore};

#[derive(Debug, Clone, PartialEq)]
pub enum EventOutcome {
    Stored { id: EventId, event: Event },
    Duplicate { id: EventId },
    Rejected { reason: String },
}

impl EventOutcome {
    pub fn describe(&self) -> String {
        match self {
            Self::Stored { id, event } => format!("stored event {} ({})", id.0, summarize(event)),
            Self::Duplicate { id } => format!("duplicate of event {}", id.0),
            Self::Rejected { reason } => format!("rejected update: {reason}"),
        }
    }
}

pub struct EventIntake<'a> {
    adapter: &'a dyn ChannelAdapter,
    store: &'a EventStore,
}

impl<'a> EventIntake<'a> {
    pub fn new(adapter: &'a dyn ChannelAdapter, store: &'a EventStore) -> Self {
        Self { adapter, store }
    }

    pub fn ingest(&self, raw: &RawUpdate) -> Vec<EventOutcome> {
        let events = match self.adapter.normalize(raw) {
            Ok(events) => events,
            Err(error) => {
                return vec![EventOutcome::Rejected {
                    reason: error.to_string(),
                }];
            }
        };

        events
            .into_iter()
            .map(|event| match self.store.append(&event) {
                Ok(AppendOutcome::Appended(id)) => EventOutcome::Stored { id, event },
                Ok(AppendOutcome::Duplicate(id)) => EventOutcome::Duplicate { id },
                Err(error) => EventOutcome::Rejected {
                    reason: error.to_string(),
                },
            })
            .collect()
    }
}

fn summarize(event: &Event) -> String {
    match &event.origin {
        EventOrigin::Platform(platform) => {
            let text = platform
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");

            format!("{} `{text}`", platform.target.channel())
        }
        EventOrigin::Runtime(runtime) => format!("runtime {:?}", runtime.kind),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::channel::adapter::AdapterError;
    use crate::channel::adapter::testing::MockChannelAdapter;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::event::{ActorRef, DedupKey};

    fn memory_store() -> EventStore {
        EventStore::in_memory().unwrap()
    }

    fn message(text: &str, platform_event_id: &str) -> Event {
        let mut event = Event::platform_text(
            DeliveryTarget::direct(Channel::Telegram, "default", "123"),
            ActorRef::new("member_abc"),
            text,
        );

        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.dedup = Some(DedupKey {
            platform: Channel::Telegram,
            platform_event_id: platform_event_id.to_string(),
        });

        event
    }

    fn raw(update_id: u64) -> RawUpdate {
        RawUpdate::new(
            Channel::Telegram,
            serde_json::json!({ "update_id": update_id }),
        )
    }

    #[test]
    fn a_new_event_is_stored() {
        let store = memory_store();
        let event = message("hi", "42");
        let adapter: Arc<dyn ChannelAdapter> =
            MockChannelAdapter::new(vec![Ok(vec![event.clone()])]);
        let intake = EventIntake::new(adapter.as_ref(), &store);

        let outcomes = intake.ingest(&raw(1));

        assert_eq!(
            outcomes,
            vec![EventOutcome::Stored {
                id: EventId(1),
                event,
            }]
        );
        assert_eq!(store.len().unwrap(), 1);
    }

    #[test]
    fn a_replayed_event_is_reported_as_a_duplicate() {
        let store = memory_store();
        let adapter: Arc<dyn ChannelAdapter> = MockChannelAdapter::new(vec![
            Ok(vec![message("hi", "42")]),
            Ok(vec![message("hi", "42")]),
        ]);
        let intake = EventIntake::new(adapter.as_ref(), &store);

        intake.ingest(&raw(1));
        let outcomes = intake.ingest(&raw(1));

        assert_eq!(outcomes, vec![EventOutcome::Duplicate { id: EventId(1) }]);
        assert_eq!(store.len().unwrap(), 1);
    }

    #[test]
    fn an_adapter_failure_is_reported_as_rejected() {
        let store = memory_store();
        let adapter: Arc<dyn ChannelAdapter> =
            MockChannelAdapter::new(vec![Err(AdapterError::MissingField {
                field: "message".to_string(),
            })]);
        let intake = EventIntake::new(adapter.as_ref(), &store);

        let outcomes = intake.ingest(&raw(1));

        assert_eq!(
            outcomes,
            vec![EventOutcome::Rejected {
                reason: "platform update is missing required field `message`".to_string(),
            }]
        );
        assert_eq!(store.len().unwrap(), 0);
    }

    #[test]
    fn one_update_can_yield_several_outcomes_in_order() {
        let store = memory_store();
        let first = message("first", "1");
        let second = message("second", "2");
        let adapter: Arc<dyn ChannelAdapter> =
            MockChannelAdapter::new(vec![Ok(vec![first.clone(), second.clone()])]);
        let intake = EventIntake::new(adapter.as_ref(), &store);

        let outcomes = intake.ingest(&raw(1));

        assert_eq!(
            outcomes,
            vec![
                EventOutcome::Stored {
                    id: EventId(1),
                    event: first,
                },
                EventOutcome::Stored {
                    id: EventId(2),
                    event: second,
                },
            ]
        );
        assert_eq!(store.len().unwrap(), 2);
    }

    #[test]
    fn describe_summarizes_each_outcome() {
        let stored = EventOutcome::Stored {
            id: EventId(3),
            event: message("hello there", "42"),
        };
        assert_eq!(stored.describe(), "stored event 3 (telegram `hello there`)");

        assert_eq!(
            EventOutcome::Duplicate { id: EventId(3) }.describe(),
            "duplicate of event 3"
        );
        assert_eq!(
            EventOutcome::Rejected {
                reason: "boom".to_string()
            }
            .describe(),
            "rejected update: boom"
        );
    }
}
