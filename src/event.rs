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
        sender_id: String,
        text: impl Into<String>,
    ) -> Self {
        Self {
            delivery: Delivery::Durable,
            origin: EventOrigin::Platform(Box::new(PlatformEvent {
                kind: PlatformKind::Message,
                target,
                envelope: Envelope {
                    sender,
                    sender_platform_id: sender_id,
                    sender_name: None,
                    bot: None,
                    date: Timestamp::now(),
                    edit_date: None,
                    reply_to: None,
                    external_reply: None,
                    quote: None,
                    forward: None,
                    mentions: vec![],
                    reply_markup: None,
                },
                message_id: None,
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
    /// This message's own platform id — distinct from the dedup `update_id`.
    #[serde(default)]
    pub message_id: Option<String>,
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
    pub sender_platform_id: String,
    pub sender_name: Option<String>,
    pub bot: Option<BotRef>,
    pub date: Timestamp,
    pub edit_date: Option<Timestamp>,
    pub reply_to: Option<ReplyRef>,
    /// A reply to a message from another chat (Telegram `external_reply`), kept
    /// separate from `reply_to` because the original is never inlined.
    pub external_reply: Option<ExternalReply>,
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
pub struct ReplyRef {
    pub message_id: String,
    pub sender: ActorRef,
    pub sender_name: String,
    pub sender_platform_id: String,
    pub snippet: Option<String>,
}

impl Envelope {
    /// Whether this message replies to `platform_id`, in-chat or externally.
    pub fn replies_to(&self, platform_id: &str) -> bool {
        self.reply_to
            .as_ref()
            .is_some_and(|reply| reply.sender_platform_id == platform_id)
            || self
                .external_reply
                .as_ref()
                .and_then(ExternalReply::sender_platform_id)
                == Some(platform_id)
    }
}

/// A reply to a message from another chat or forum topic (Telegram
/// `external_reply`): the original is never inlined, so there is no snippet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalReply {
    /// The replied message's id, when the platform exposes it (supergroup /
    /// channel origins only).
    pub message_id: Option<String>,
    pub chat_id: Option<String>,
    pub origin: ReplyOrigin,
}

impl ExternalReply {
    /// The replied message's sender platform id, if it has one: a hidden user
    /// exposes no id.
    pub fn sender_platform_id(&self) -> Option<&str> {
        match &self.origin {
            ReplyOrigin::User(peer) | ReplyOrigin::Chat(peer) | ReplyOrigin::Channel(peer) => {
                Some(peer.platform_id.as_str())
            }
            ReplyOrigin::HiddenUser { .. } => None,
        }
    }
}

/// Where the message an external reply points at came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReplyOrigin {
    User(ReplyPeer),
    Chat(ReplyPeer),
    Channel(ReplyPeer),
    HiddenUser { name: String },
}

/// A named, keyable sender behind an external reply.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplyPeer {
    pub actor: ActorRef,
    pub platform_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BotRef {
    pub actor: ActorRef,
    pub platform_id: String,
    pub username: Option<String>,
}

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
    pub actor: Option<ActorRef>,
    pub platform_id: Option<String>,
    pub username: Option<String>,
    pub name: String,
    pub is_bot: bool,
    pub is_self: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Part {
    Text {
        text: String,
        entities: Vec<Entity>,
    },
    Photo {
        media: Media,
        caption: Option<String>,
    },
    Voice {
        media: Media,
        duration_s: Option<u32>,
        caption: Option<String>,
    },
    Document {
        media: Media,
        file_name: Option<String>,
        caption: Option<String>,
    },
    Sticker {
        media: Media,
    },
}

/// A media part: the raw platform handle for fetching plus the gateway's own
/// stable reference and mime, once known (§11.5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Media {
    /// The platform's raw handle (e.g. a Telegram `file_id`).
    pub handle: MediaHandle,
    /// `media://inbound/<id>`, filled at ingest; `None` until then.
    pub uri: Option<String>,
    pub mime: Option<String>,
}

impl Media {
    pub fn new(handle: MediaHandle, mime: Option<String>) -> Self {
        Self {
            handle,
            uri: None,
            mime,
        }
    }
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

    fn local_message(text: &str) -> Event {
        Event {
            delivery: Delivery::Durable,
            origin: EventOrigin::Platform(Box::new(PlatformEvent {
                kind: PlatformKind::Message,
                target: DeliveryTarget::direct(Channel::Webui, "default", "webui"),
                envelope: Envelope {
                    sender: ActorRef::new("member_0a1b"),
                    sender_platform_id: "webui".to_string(),
                    sender_name: None,
                    bot: None,
                    date: Timestamp::from_millis(1_700_000_000_000),
                    edit_date: None,
                    reply_to: None,
                    external_reply: None,
                    quote: None,
                    forward: None,
                    mentions: vec![],
                    reply_markup: None,
                },
                parts: vec![Part::Text {
                    text: text.to_string(),
                    entities: vec![],
                }],
                message_id: None,
                dedup: None,
                raw: None,
            })),
        }
    }

    #[test]
    fn local_text_message_carries_its_facts() {
        let event = local_message("hi");
        assert_eq!(event.delivery, Delivery::Durable);

        let EventOrigin::Platform(platform) = event.origin else {
            panic!("local input must be a platform event");
        };

        assert_eq!(platform.kind, PlatformKind::Message);
        assert_eq!(
            platform.target,
            DeliveryTarget::direct(Channel::Webui, "default", "webui")
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
