use std::collections::HashMap;

use crate::event::{DedupKey, Event, EventId, EventOrigin};

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: EventId,
    pub event: Event,
}

#[derive(Default)]
pub struct EventStore {
    next_id: u64,
    seen: HashMap<DedupKey, EventId>,
    log: Vec<StoredEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended(EventId),
    Duplicate(EventId),
}

impl EventStore {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            seen: HashMap::new(),
            log: Vec::new(),
        }
    }
    pub fn append(&mut self, event: Event) -> AppendOutcome {
        let key = match &event.origin {
            EventOrigin::Platform(platform) => platform.dedup.clone(),
            EventOrigin::Runtime(_) => None,
        };

        if let Some(key) = &key
            && let Some(&existing) = self.seen.get(key)
        {
            return AppendOutcome::Duplicate(existing);
        }

        self.next_id += 1;
        let id = EventId(self.next_id);
        if let Some(key) = key {
            self.seen.insert(key, id);
        }

        self.log.push(StoredEvent { id, event });
        AppendOutcome::Appended(id)
    }
    pub fn get(&self, id: EventId) -> Option<&StoredEvent> {
        self.log.iter().find(|e| e.id.eq(&id))
    }
    pub fn len(&self) -> usize {
        self.log.len()
    }
    pub fn is_empty(&self) -> bool {
        self.log.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::event::{ActorRef, Delivery, InternalScope, RuntimeEvent, RuntimeKind};

    fn message(text: &str) -> Event {
        Event::platform_text(
            DeliveryTarget::direct(Channel::Cli, "default", "cli"),
            ActorRef::new("member_cli"),
            text,
        )
    }

    fn message_with_dedup(text: &str, platform: Channel, platform_event_id: &str) -> Event {
        let mut event = Event::platform_text(
            DeliveryTarget::direct(platform, "default", "1"),
            ActorRef::new("member_cli"),
            text,
        );

        let EventOrigin::Platform(platform_event) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform_event.dedup = Some(DedupKey {
            platform,
            platform_event_id: platform_event_id.to_string(),
        });

        event
    }

    fn heartbeat_notice() -> Event {
        Event {
            delivery: Delivery::Ephemeral,
            origin: EventOrigin::Runtime(RuntimeEvent {
                kind: RuntimeKind::Notice,
                scope: InternalScope::Heartbeat,
                correlation_id: None,
            }),
        }
    }

    #[test]
    fn append_assigns_increasing_ids_in_order() {
        let mut store = EventStore::new();

        assert_eq!(
            store.append(message("one")),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store.append(message("two")),
            AppendOutcome::Appended(EventId(2))
        );
        assert_eq!(
            store.append(message("three")),
            AppendOutcome::Appended(EventId(3))
        );

        assert_eq!(store.len(), 3);
        assert!(!store.is_empty());
    }

    #[test]
    fn a_replayed_event_returns_the_original_id_without_growing_the_log() {
        let mut store = EventStore::new();
        let event = message_with_dedup("hi", Channel::Telegram, "42");

        assert_eq!(
            store.append(event.clone()),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(store.append(event), AppendOutcome::Duplicate(EventId(1)));

        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_duplicate_does_not_consume_an_id() {
        let mut store = EventStore::new();
        let first = message_with_dedup("hi", Channel::Telegram, "42");

        assert_eq!(
            store.append(first.clone()),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(store.append(first), AppendOutcome::Duplicate(EventId(1)));
        assert_eq!(
            store.append(message_with_dedup("other", Channel::Telegram, "43")),
            AppendOutcome::Appended(EventId(2))
        );
    }

    #[test]
    fn a_dedup_key_is_scoped_to_its_platform() {
        let mut store = EventStore::new();

        assert_eq!(
            store.append(message_with_dedup("hi", Channel::Telegram, "42")),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store.append(message_with_dedup("hi", Channel::QQ, "42")),
            AppendOutcome::Appended(EventId(2))
        );

        assert_eq!(store.len(), 2);
    }

    #[test]
    fn events_without_a_dedup_key_are_never_deduped() {
        let mut store = EventStore::new();

        assert_eq!(
            store.append(message("same")),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store.append(message("same")),
            AppendOutcome::Appended(EventId(2))
        );
        assert_eq!(
            store.append(heartbeat_notice()),
            AppendOutcome::Appended(EventId(3))
        );

        assert_eq!(store.len(), 3);
    }

    #[test]
    fn get_returns_the_stored_event_and_unknown_ids_are_none() {
        let mut store = EventStore::new();
        let event = message("hi");
        store.append(event.clone());

        assert_eq!(store.get(EventId(1)).unwrap().event, event);
        assert!(store.get(EventId(999)).is_none());
    }
}
