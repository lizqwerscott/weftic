use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::channel::{Channel, DeliveryTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub delivery: Delivery,
    pub origin: EventOrigin,
}

impl Event {
    pub fn platform_text(
        target: DeliveryTarget,
        sender: ActorRef,
        text: impl Into<String>,
    ) -> Self {
        Self {
            delivery: Delivery::Durable,
            origin: EventOrigin::Platform(Box::new(PlatformEvent {
                kind: PlatformKind::Message,
                target,
                envelope: Envelope {
                    sender,
                    sender_name: None,
                    thread: None,
                    date: Timestamp::now(),
                    edit_date: None,
                    reply_to: None,
                    quote: None,
                    forward: None,
                    mentions: vec![],
                    reply_markup: None,
                },
                parts: vec![Part::Text {
                    text: text.into(),
                    entities: vec![],
                }],
                dedup: None,
                raw: None,
            })),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventOrigin {
    Platform(Box<PlatformEvent>),
    Runtime(RuntimeEvent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformEvent {
    pub kind: PlatformKind,
    pub target: DeliveryTarget,
    pub envelope: Envelope,
    pub parts: Vec<Part>,
    pub dedup: Option<DedupKey>,
    pub raw: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub kind: RuntimeKind,
    pub scope: InternalScope,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlatformKind {
    Message,
    EditedMessage,
    Service(ServiceKind),
    CallbackQuery,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuntimeKind {
    Cron,
    Heartbeat,
    WorkPlan,
    ToolDone,
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceKind {
    MemberJoined,
    MemberLeft,
    TitleChanged,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Delivery {
    Durable,
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InternalScope {
    Cron { job: String, run: String },
    Heartbeat,
    WorkPlan { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    pub platform: Channel,
    pub platform_event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CorrelationId(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub sender: ActorRef,
    pub sender_name: Option<String>,
    pub thread: Option<String>,
    pub date: Timestamp,
    pub edit_date: Option<Timestamp>,
    pub reply_to: Option<MsgRef>,
    pub quote: Option<Quote>,
    pub forward: Option<ForwardOrigin>,
    pub mentions: Vec<Mention>,
    pub reply_markup: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActorRef(String);

impl ActorRef {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MsgRef(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Quote {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForwardOrigin {
    pub sender_name: Option<String>,
    pub actor: Option<ActorRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mention {
    pub actor: ActorRef,
    pub name: String,
    pub is_bot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Part {
    Text {
        text: String,
        entities: Vec<Entity>,
    },
    Photo {
        handle: MediaHandle,
        caption: Option<String>,
    },
    Voice {
        handle: MediaHandle,
        duration_s: Option<u32>,
        caption: Option<String>,
    },
    Document {
        handle: MediaHandle,
        file_name: Option<String>,
        caption: Option<String>,
    },
    Sticker {
        handle: MediaHandle,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaHandle(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entity {
    pub kind: EntityKind,
    pub offset: usize,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityKind {
    Bold,
    Italic,
    Code,
    Pre,
    Link,
    Mention,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timestamp(i64);

impl Timestamp {
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0);
        Self(millis)
    }

    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    pub fn as_millis(&self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_message(text: &str) -> Event {
        Event {
            delivery: Delivery::Durable,
            origin: EventOrigin::Platform(Box::new(PlatformEvent {
                kind: PlatformKind::Message,
                target: DeliveryTarget::direct(Channel::Cli, "default", "cli"),
                envelope: Envelope {
                    sender: ActorRef::new("member_0a1b"),
                    sender_name: None,
                    thread: None,
                    date: Timestamp::from_millis(1_700_000_000_000),
                    edit_date: None,
                    reply_to: None,
                    quote: None,
                    forward: None,
                    mentions: vec![],
                    reply_markup: None,
                },
                parts: vec![Part::Text {
                    text: text.to_string(),
                    entities: vec![],
                }],
                dedup: None,
                raw: None,
            })),
        }
    }

    #[test]
    fn cli_text_message_carries_its_facts() {
        let event = cli_message("hi");
        assert_eq!(event.delivery, Delivery::Durable);

        let EventOrigin::Platform(platform) = event.origin else {
            panic!("CLI input must be a platform event");
        };

        assert_eq!(platform.kind, PlatformKind::Message);
        assert_eq!(
            platform.target,
            DeliveryTarget::direct(Channel::Cli, "default", "cli")
        );
        assert_eq!(
            platform.parts,
            vec![Part::Text {
                text: "hi".to_string(),
                entities: vec![],
            }]
        );
        assert!(platform.dedup.is_none());
    }

    #[test]
    fn runtime_tool_done_links_via_correlation() {
        let event = Event {
            delivery: Delivery::Durable,
            origin: EventOrigin::Runtime(RuntimeEvent {
                kind: RuntimeKind::ToolDone,
                scope: InternalScope::Heartbeat,
                correlation_id: Some(CorrelationId("call_1".to_string())),
            }),
        };

        let EventOrigin::Runtime(runtime) = event.origin else {
            panic!("runtime origin must stay runtime");
        };

        assert_eq!(runtime.kind, RuntimeKind::ToolDone);
        assert_eq!(
            runtime.correlation_id,
            Some(CorrelationId("call_1".to_string()))
        );
    }

    #[test]
    fn direct_and_group_targets_are_distinct() {
        assert_ne!(
            DeliveryTarget::direct(Channel::Telegram, "default", "1"),
            DeliveryTarget::group(Channel::Telegram, "default", "1")
        );
    }

    #[test]
    fn dedup_key_is_scoped_to_its_platform() {
        let again = DedupKey {
            platform: Channel::Telegram,
            platform_event_id: "42".to_string(),
        };
        let first = DedupKey {
            platform: Channel::Telegram,
            platform_event_id: "42".to_string(),
        };
        let same_id_other_platform = DedupKey {
            platform: Channel::QQ,
            platform_event_id: "42".to_string(),
        };

        assert_eq!(first, again);
        assert_ne!(first, same_id_other_platform);
    }

    #[test]
    fn timestamp_round_trips_millis() {
        assert_eq!(
            Timestamp::from_millis(1_700_000_000_000).as_millis(),
            1_700_000_000_000
        );
    }
}
