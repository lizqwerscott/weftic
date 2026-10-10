//! Telegram inbound normalization: a raw Telegram update JSON becomes an `Event`.

pub mod client;
pub mod poll;
pub mod runtime;
pub mod send;

use serde::Deserialize;

use crate::channel::adapter::{AdapterError, ChannelAdapter, RawUpdate};
use crate::channel::telegram::client::BotMe;
use crate::channel::{Channel, ChannelChatType, DeliveryTarget};
use crate::event::{
    BotRef, DedupKey, Delivery, Entity, EntityKind, Envelope, Event, EventOrigin, MediaHandle,
    Mention, Part, PlatformEvent, PlatformKind, ReplyRef, ServiceKind, Timestamp,
};
use crate::identity::member_ref;

pub struct TelegramAdapter {
    account_id: String,
    bot: BotRef,
}

impl TelegramAdapter {
    pub fn new(account_id: impl Into<String>, bot: &BotMe) -> Self {
        let account_id = account_id.into();
        let platform_id = bot.id.to_string();
        let actor = member_ref(Channel::Telegram, &account_id, &platform_id);

        Self {
            account_id,
            bot: BotRef {
                actor,
                platform_id,
                username: bot.username.clone(),
            },
        }
    }

    fn event(
        &self,
        update_id: i64,
        message: &Message,
        base_kind: PlatformKind,
        raw: &RawUpdate,
    ) -> Result<Event, AdapterError> {
        let account = self.account_id.as_str();

        let (sender_id, sender_name) = sender_identity(message)?;
        let sender = member_ref(Channel::Telegram, account, &sender_id);

        let parts = build_parts(message);
        let kind = match service_kind(message) {
            Some(service) => PlatformKind::Service(service),
            None if !parts.is_empty() => base_kind,
            None => PlatformKind::Unknown,
        };

        let envelope = Envelope {
            sender,
            sender_platform_id: sender_id,
            sender_name: Some(sender_name),
            bot: Some(self.bot.clone()),
            thread: message.message_thread_id.map(|id| id.to_string()),
            date: Timestamp::from_millis(message.date.saturating_mul(1000)),
            edit_date: message
                .edit_date
                .map(|seconds| Timestamp::from_millis(seconds.saturating_mul(1000))),
            reply_to: message.reply_to_message.as_deref().and_then(|replied| {
                let (sender_id, sender_name) = sender_identity(replied).ok()?;
                let sender = member_ref(Channel::Telegram, account, &sender_id);

                Some(ReplyRef {
                    message_id: replied.message_id.to_string(),
                    sender,
                    sender_platform_id: sender_id,
                    sender_name,
                    snippet: reply_snippet(replied),
                })
            }),
            quote: None,
            forward: None,
            mentions: map_mentions(message, account, &self.bot),
            reply_markup: None,
        };

        Ok(Event {
            delivery: Delivery::Durable,
            origin: EventOrigin::Platform(Box::new(PlatformEvent {
                kind,
                target: target_for(account, message),
                envelope,
                message_id: Some(message.message_id.to_string()),
                parts,
                dedup: Some(DedupKey {
                    platform: Channel::Telegram,
                    platform_event_id: update_id.to_string(),
                }),
                raw: Some(raw.payload.clone()),
            })),
        })
    }
}

impl ChannelAdapter for TelegramAdapter {
    fn normalize(&self, raw: &RawUpdate) -> Result<Vec<Event>, AdapterError> {
        let update: Update = serde_json::from_value(raw.payload.clone()).map_err(|error| {
            AdapterError::Malformed {
                reason: error.to_string(),
            }
        })?;

        let Some((message, base_kind)) = select_message(&update) else {
            return Ok(Vec::new());
        };

        Ok(vec![self.event(
            update.update_id,
            message,
            base_kind,
            raw,
        )?])
    }
}

fn select_message(update: &Update) -> Option<(&Message, PlatformKind)> {
    if let Some(message) = &update.message {
        Some((message, PlatformKind::Message))
    } else if let Some(message) = &update.edited_message {
        Some((message, PlatformKind::EditedMessage))
    } else if let Some(message) = &update.channel_post {
        Some((message, PlatformKind::Message))
    } else {
        update
            .edited_channel_post
            .as_ref()
            .map(|message| (message, PlatformKind::EditedMessage))
    }
}

fn sender_identity(message: &Message) -> Result<(String, String), AdapterError> {
    if let Some(user) = &message.from {
        Ok((user.id.to_string(), display_name(user)))
    } else if let Some(chat) = &message.sender_chat {
        Ok((
            chat.id.to_string(),
            chat.title
                .clone()
                .unwrap_or_else(|| format!("chat {}", chat.id)),
        ))
    } else {
        Err(AdapterError::MissingField {
            field: "message.from".to_string(),
        })
    }
}

fn target_for(account: &str, message: &Message) -> DeliveryTarget {
    let target_id = message.chat.id.to_string();
    match chat_type(&message.chat) {
        ChannelChatType::Direct => DeliveryTarget::direct(Channel::Telegram, account, target_id),
        ChannelChatType::Group => DeliveryTarget::group(Channel::Telegram, account, target_id),
    }
}

fn chat_type(chat: &Chat) -> ChannelChatType {
    match chat.kind.as_str() {
        "private" => ChannelChatType::Direct,
        _ => ChannelChatType::Group,
    }
}

fn service_kind(message: &Message) -> Option<ServiceKind> {
    if message.new_chat_members.is_some() {
        Some(ServiceKind::MemberJoined)
    } else if message.left_chat_member.is_some() {
        Some(ServiceKind::MemberLeft)
    } else if message.new_chat_title.is_some() {
        Some(ServiceKind::TitleChanged)
    } else {
        None
    }
}

const REPLY_SNIPPET_MAX_CHARS: usize = 200;

fn reply_snippet(replied: &Message) -> Option<String> {
    let raw = match (&replied.text, &replied.caption) {
        (Some(text), _) => text.clone(),
        (None, Some(caption)) => caption.clone(),
        (None, None) => media_placeholder(replied)?, // 纯媒体 → 占位符
    };
    Some(truncate_chars(&raw, REPLY_SNIPPET_MAX_CHARS))
}

fn media_placeholder(message: &Message) -> Option<String> {
    if message.photo.is_some() {
        Some("[photo]".to_string())
    } else if message.voice.is_some() {
        Some("[voice]".to_string())
    } else if message.document.is_some() {
        Some("[document]".to_string())
    } else if message.sticker.is_some() {
        Some("[sticker]".to_string())
    } else {
        None
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn build_parts(message: &Message) -> Vec<Part> {
    let mut parts = Vec::new();
    let caption = message.caption.clone();

    if let Some(text) = &message.text {
        parts.push(Part::Text {
            text: text.clone(),
            entities: map_entities(message.entities.as_deref().unwrap_or(&[])),
        });
    }

    if let Some(photos) = &message.photo
        && let Some(largest) = photos.iter().max_by_key(|photo| photo.width * photo.height)
    {
        parts.push(Part::Photo {
            handle: MediaHandle(largest.file_id.clone()),
            caption: caption.clone(),
        });
    }

    if let Some(voice) = &message.voice {
        parts.push(Part::Voice {
            handle: MediaHandle(voice.file_id.clone()),
            duration_s: Some(voice.duration.max(0) as u32),
            caption: caption.clone(),
        });
    }

    if let Some(document) = &message.document {
        parts.push(Part::Document {
            handle: MediaHandle(document.file_id.clone()),
            file_name: document.file_name.clone(),
            caption: caption.clone(),
        });
    }

    if let Some(sticker) = &message.sticker {
        parts.push(Part::Sticker {
            handle: MediaHandle(sticker.file_id.clone()),
        });
    }

    parts
}

fn map_entities(entities: &[MessageEntity]) -> Vec<Entity> {
    entities
        .iter()
        .map(|entity| Entity {
            kind: entity_kind(&entity.kind),
            offset: entity.offset.max(0) as usize,
            length: entity.length.max(0) as usize,
        })
        .collect()
}

fn entity_kind(kind: &str) -> EntityKind {
    match kind {
        "bold" => EntityKind::Bold,
        "italic" => EntityKind::Italic,
        "code" => EntityKind::Code,
        "pre" => EntityKind::Pre,
        "text_link" | "url" => EntityKind::Link,
        "mention" | "text_mention" => EntityKind::Mention,
        other => EntityKind::Other(other.to_string()),
    }
}

fn map_mentions(message: &Message, account: &str, bot: &BotRef) -> Vec<Mention> {
    let entities = message.entities.as_deref().unwrap_or(&[]);
    let text = message.text.as_deref();

    entities
        .iter()
        .filter_map(|entity| match entity.kind.as_str() {
            "text_mention" => {
                let user = entity.user.as_ref()?;
                let platform_id = user.id.to_string();

                Some(Mention {
                    actor: Some(member_ref(Channel::Telegram, account, &platform_id)),
                    platform_id: Some(platform_id.clone()),
                    username: user.username.clone(),
                    name: display_name(user),
                    is_bot: user.is_bot,
                    is_self: is_the_bot(Some(&platform_id), user.username.as_deref(), bot),
                })
            }
            "mention" => {
                // A bare `@username` carries no user object, so the ref is keyed
                // by the username and `platform_id` stays unknown.
                let username = mention_username(text?, entity)?;

                Some(Mention {
                    actor: None,
                    platform_id: None,
                    username: Some(username.to_string()),
                    name: username.to_string(),
                    is_bot: false,
                    is_self: is_the_bot(None, Some(username), bot),
                })
            }
            _ => None,
        })
        .collect()
}

fn is_the_bot(platform_id: Option<&str>, username: Option<&str>, bot: &BotRef) -> bool {
    platform_id == Some(bot.platform_id.as_str())
        || (username.is_some() && username == bot.username.as_deref())
}

/// Extract the `@username` behind a plain `mention` entity. Telegram measures
/// entity offsets and lengths in **UTF-16 code units**, so slicing by byte or
/// char would split non-BMP characters; `utf16_to_byte` maps the bounds and
/// `None` is returned when they do not land on character boundaries.
fn mention_username<'a>(text: &'a str, entity: &MessageEntity) -> Option<&'a str> {
    let start = utf16_to_byte(text, entity.offset.max(0) as usize)?;
    let end = utf16_to_byte(
        text,
        entity.offset.saturating_add(entity.length).max(0) as usize,
    )
    .unwrap_or(text.len());

    text.get(start..end)?.strip_prefix('@')
}

fn utf16_to_byte(text: &str, target: usize) -> Option<usize> {
    let mut units = 0;

    for (byte_idx, ch) in text.char_indices() {
        if units == target {
            return Some(byte_idx);
        }
        units += ch.len_utf16();
        if units > target {
            return None;
        }
    }

    (units == target).then_some(text.len())
}

fn display_name(user: &User) -> String {
    match (&user.first_name, &user.last_name) {
        (Some(first), Some(last)) => format!("{first} {last}"),
        (Some(first), None) => first.clone(),
        (None, Some(last)) => last.clone(),
        (None, None) => user
            .username
            .clone()
            .unwrap_or_else(|| format!("user {}", user.id)),
    }
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
    edited_message: Option<Message>,
    channel_post: Option<Message>,
    edited_channel_post: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    message_id: i64,
    message_thread_id: Option<i64>,
    from: Option<User>,
    sender_chat: Option<Chat>,
    date: i64,
    edit_date: Option<i64>,
    chat: Chat,
    text: Option<String>,
    caption: Option<String>,
    entities: Option<Vec<MessageEntity>>,
    reply_to_message: Option<Box<Message>>,
    photo: Option<Vec<PhotoSize>>,
    voice: Option<Voice>,
    document: Option<Document>,
    sticker: Option<Sticker>,
    new_chat_members: Option<Vec<User>>,
    left_chat_member: Option<User>,
    new_chat_title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct User {
    id: i64,
    #[serde(default)]
    is_bot: bool,
    first_name: Option<String>,
    last_name: Option<String>,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageEntity {
    #[serde(rename = "type")]
    kind: String,
    offset: i64,
    length: i64,
    user: Option<User>,
}

#[derive(Debug, Deserialize)]
struct PhotoSize {
    file_id: String,
    width: i64,
    height: i64,
}

#[derive(Debug, Deserialize)]
struct Voice {
    file_id: String,
    duration: i64,
}

#[derive(Debug, Deserialize)]
struct Document {
    file_id: String,
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Sticker {
    file_id: String,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::event::ActorRef;

    const BOT_ID: i64 = 424242;
    const BOT_USERNAME: &str = "weftic_bot";

    fn bot_me() -> BotMe {
        BotMe {
            id: BOT_ID,
            username: Some(BOT_USERNAME.to_string()),
        }
    }

    fn bot_ref() -> BotRef {
        BotRef {
            actor: member_ref(Channel::Telegram, "default", &BOT_ID.to_string()),
            platform_id: BOT_ID.to_string(),
            username: Some(BOT_USERNAME.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn expected(
        kind: PlatformKind,
        target: DeliveryTarget,
        sender_id: &str,
        sender_name: &str,
        date_ms: i64,
        edit_ms: Option<i64>,
        reply_to: Option<ReplyRef>,
        message_id: &str,
        parts: Vec<Part>,
        dedup_id: &str,
        raw: Value,
    ) -> Event {
        Event {
            delivery: Delivery::Durable,
            origin: EventOrigin::Platform(Box::new(PlatformEvent {
                kind,
                target,
                envelope: Envelope {
                    sender: member_ref(Channel::Telegram, "default", sender_id),
                    sender_platform_id: sender_id.to_string(),
                    sender_name: Some(sender_name.to_string()),
                    bot: Some(bot_ref()),
                    thread: None,
                    date: Timestamp::from_millis(date_ms),
                    edit_date: edit_ms.map(Timestamp::from_millis),
                    reply_to,
                    quote: None,
                    forward: None,
                    mentions: vec![],
                    reply_markup: None,
                },
                parts,
                message_id: Some(message_id.to_string()),
                dedup: Some(DedupKey {
                    platform: Channel::Telegram,
                    platform_event_id: dedup_id.to_string(),
                }),
                raw: Some(raw),
            })),
        }
    }

    fn adapter() -> TelegramAdapter {
        TelegramAdapter::new("default", &bot_me())
    }

    #[test]
    fn private_text_message_maps_to_a_direct_event() {
        let payload = json!({
            "update_id": 100,
            "message": {
                "message_id": 5,
                "from": {"id": 123456, "is_bot": false, "first_name": "Ali", "last_name": "Reza", "username": "ali"},
                "date": 1_700_000_000,
                "chat": {"id": 123456, "type": "private", "first_name": "Ali"},
                "text": "hello world",
                "entities": [{"type": "bold", "offset": 0, "length": 5}]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        assert_eq!(
            events,
            vec![expected(
                PlatformKind::Message,
                DeliveryTarget::direct(Channel::Telegram, "default", "123456"),
                "123456",
                "Ali Reza",
                1_700_000_000_000,
                None,
                None,
                "5",
                vec![Part::Text {
                    text: "hello world".to_string(),
                    entities: vec![Entity {
                        kind: EntityKind::Bold,
                        offset: 0,
                        length: 5,
                    }],
                }],
                "100",
                payload,
            )]
        );
    }

    #[test]
    fn supergroup_message_maps_to_a_group_event() {
        let payload = json!({
            "update_id": 101,
            "message": {
                "message_id": 6,
                "from": {"id": 42, "is_bot": false, "first_name": "Bob"},
                "date": 1_700_000_100,
                "chat": {"id": -1001234567890i64, "type": "supergroup", "title": "Dev"},
                "text": "hi all",
                "message_thread_id": 7
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(
            platform.target,
            DeliveryTarget::group(Channel::Telegram, "default", "-1001234567890")
        );
        assert_eq!(
            platform.envelope.date,
            Timestamp::from_millis(1_700_000_100_000)
        );
        assert_eq!(platform.envelope.thread.as_deref(), Some("7"));
        assert_eq!(platform.envelope.sender_name.as_deref(), Some("Bob"));
    }

    #[test]
    fn edited_message_maps_to_an_edited_event_with_edit_date() {
        let payload = json!({
            "update_id": 102,
            "edited_message": {
                "message_id": 7,
                "from": {"id": 9, "is_bot": false, "first_name": "Cara"},
                "date": 1_700_000_200,
                "edit_date": 1_700_000_500,
                "chat": {"id": 9, "type": "private", "first_name": "Cara"},
                "text": "fixed"
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        assert_eq!(
            events,
            vec![expected(
                PlatformKind::EditedMessage,
                DeliveryTarget::direct(Channel::Telegram, "default", "9"),
                "9",
                "Cara",
                1_700_000_200_000,
                Some(1_700_000_500_000),
                None,
                "7",
                vec![Part::Text {
                    text: "fixed".to_string(),
                    entities: vec![],
                }],
                "102",
                payload,
            )]
        );
    }

    #[test]
    fn photo_message_uses_the_largest_size_and_keeps_the_caption() {
        let payload = json!({
            "update_id": 103,
            "message": {
                "message_id": 8,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_000_300,
                "chat": {"id": 5, "type": "private", "first_name": "Dee"},
                "caption": "look",
                "photo": [
                    {"file_id": "small", "width": 90, "height": 60},
                    {"file_id": "large", "width": 1280, "height": 720}
                ]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        assert_eq!(
            events,
            vec![expected(
                PlatformKind::Message,
                DeliveryTarget::direct(Channel::Telegram, "default", "5"),
                "5",
                "Dee",
                1_700_000_300_000,
                None,
                None,
                "8",
                vec![Part::Photo {
                    handle: MediaHandle("large".to_string()),
                    caption: Some("look".to_string()),
                }],
                "103",
                payload,
            )]
        );
    }

    #[test]
    fn reply_message_is_recorded_as_a_structured_reference() {
        let payload = json!({
            "update_id": 104,
            "message": {
                "message_id": 9,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_000_400,
                "chat": {"id": 5, "type": "private", "first_name": "Dee"},
                "text": "answering",
                "reply_to_message": {
                    "message_id": 3,
                    "from": {"id": 7, "is_bot": false, "first_name": "Quinn"},
                    "date": 1_700_000_000,
                    "chat": {"id": 5, "type": "private"},
                    "text": "question"
                }
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(
            platform.envelope.reply_to,
            Some(ReplyRef {
                message_id: "3".to_string(),
                sender: member_ref(Channel::Telegram, "default", "7"),
                sender_name: "Quinn".to_string(),
                sender_platform_id: "7".to_string(),
                snippet: Some("question".to_string()),
            })
        );
    }

    #[test]
    fn reply_without_a_resolvable_sender_degrades_to_no_reference() {
        let payload = json!({
            "update_id": 109,
            "message": {
                "message_id": 13,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_000_800,
                "chat": {"id": 5, "type": "private", "first_name": "Dee"},
                "text": "answering",
                "reply_to_message": {
                    "message_id": 3,
                    "date": 1_700_000_000,
                    "chat": {"id": 5, "type": "private"},
                    "text": "question"
                }
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(platform.envelope.reply_to, None);
    }

    #[test]
    fn reply_snippet_uses_a_placeholder_for_media_without_text() {
        let replied: Message = serde_json::from_value(json!({
            "message_id": 1,
            "date": 0,
            "chat": {"id": 1, "type": "private"},
            "photo": [{"file_id": "f", "width": 1, "height": 1}]
        }))
        .unwrap();

        assert_eq!(reply_snippet(&replied), Some("[photo]".to_string()));
    }

    #[test]
    fn reply_snippet_is_bounded_by_character_count() {
        let long = "a".repeat(REPLY_SNIPPET_MAX_CHARS + 10);

        assert_eq!(
            truncate_chars(&long, REPLY_SNIPPET_MAX_CHARS),
            format!("{}…", "a".repeat(REPLY_SNIPPET_MAX_CHARS))
        );
    }

    #[test]
    fn member_joined_is_a_service_event_without_parts() {
        let payload = json!({
            "update_id": 105,
            "message": {
                "message_id": 10,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_000_500,
                "chat": {"id": -1001234567890i64, "type": "supergroup", "title": "Dev"},
                "new_chat_members": [{"id": 77, "is_bot": false, "first_name": "Eve"}]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(
            platform.kind,
            PlatformKind::Service(ServiceKind::MemberJoined)
        );
        assert_eq!(platform.parts, vec![]);
    }

    #[test]
    fn text_mention_entity_becomes_a_mention_and_is_kept_out_of_prompts_as_ref() {
        let payload = json!({
            "update_id": 106,
            "message": {
                "message_id": 11,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_000_600,
                "chat": {"id": 5, "type": "private", "first_name": "Dee"},
                "text": "hi @bot",
                "entities": [
                    {"type": "text_mention", "offset": 3, "length": 4,
                     "user": {"id": 999, "is_bot": true, "first_name": "Bot"}}
                ]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload.clone());

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(
            platform.envelope.mentions,
            vec![Mention {
                actor: Some(ActorRef::new("member_0cfb80403eaf3ae6")),
                platform_id: Some("999".to_string()),
                username: None,
                name: "Bot".to_string(),
                is_bot: true,
                is_self: false,
            }]
        );
    }

    #[test]
    fn the_event_carries_the_bot_identity() {
        let payload = json!({
            "update_id": 111,
            "message": {
                "message_id": 15,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_001_000,
                "chat": {"id": 5, "type": "private", "first_name": "Dee"},
                "text": "hello"
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload);

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(platform.envelope.bot, Some(bot_ref()));
    }

    #[test]
    fn the_event_carries_its_own_message_id() {
        let payload = json!({
            "update_id": 115,
            "message": {
                "message_id": 21,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_001_400,
                "chat": {"id": 5, "type": "private", "first_name": "Dee"},
                "text": "hello"
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload);

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        // The message id is its own fact, distinct from the update id in `dedup`.
        assert_eq!(platform.message_id.as_deref(), Some("21"));
        assert_eq!(
            platform
                .dedup
                .as_ref()
                .map(|dedup| dedup.platform_event_id.as_str()),
            Some("115")
        );
    }

    #[test]
    fn mentioning_the_bot_marks_the_mention_as_self() {
        let payload = json!({
            "update_id": 110,
            "message": {
                "message_id": 14,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_000_900,
                "chat": {"id": -1001234567890i64, "type": "supergroup", "title": "Dev"},
                "text": "@weftic_bot hi",
                "entities": [
                    {"type": "text_mention", "offset": 0, "length": 11,
                     "user": {"id": 424242, "is_bot": true, "first_name": "Weftic", "username": "weftic_bot"}}
                ]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload);

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        let mention = &platform.envelope.mentions[0];
        assert_eq!(mention.platform_id.as_deref(), Some("424242"));
        assert_eq!(mention.username.as_deref(), Some("weftic_bot"));
        assert!(mention.is_self);
    }

    #[test]
    fn a_plain_username_mention_is_captured_and_the_bot_is_marked_self() {
        let payload = json!({
            "update_id": 112,
            "message": {
                "message_id": 16,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_001_100,
                "chat": {"id": -1001234567890i64, "type": "supergroup", "title": "Dev"},
                "text": "hey @weftic_bot please",
                "entities": [{"type": "mention", "offset": 4, "length": 11}]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload);

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(
            platform.envelope.mentions,
            vec![Mention {
                actor: None,
                platform_id: None,
                username: Some("weftic_bot".to_string()),
                name: "weftic_bot".to_string(),
                is_bot: false,
                is_self: true,
            }]
        );
    }

    #[test]
    fn a_plain_username_mention_of_someone_else_is_not_self() {
        let payload = json!({
            "update_id": 113,
            "message": {
                "message_id": 17,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_001_200,
                "chat": {"id": -1001234567890i64, "type": "supergroup", "title": "Dev"},
                "text": "cc @alice",
                "entities": [{"type": "mention", "offset": 3, "length": 6}]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload);

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        let mention = &platform.envelope.mentions[0];
        assert_eq!(mention.username.as_deref(), Some("alice"));
        assert_eq!(mention.platform_id, None);
        assert!(!mention.is_self);
    }

    #[test]
    fn mention_offsets_are_utf16_code_units() {
        // The emoji is one char but two UTF-16 code units, so `@weftic_bot`
        // starts at code unit 3 — not at byte / char offset 2.
        let payload = json!({
            "update_id": 114,
            "message": {
                "message_id": 18,
                "from": {"id": 5, "is_bot": false, "first_name": "Dee"},
                "date": 1_700_001_300,
                "chat": {"id": -1001234567890i64, "type": "supergroup", "title": "Dev"},
                "text": "\u{1F44D} @weftic_bot",
                "entities": [{"type": "mention", "offset": 3, "length": 11}]
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload);

        let events = adapter().normalize(&raw).unwrap();

        let EventOrigin::Platform(platform) = &events[0].origin else {
            panic!("expected a platform event");
        };
        assert_eq!(
            platform.envelope.mentions[0].username.as_deref(),
            Some("weftic_bot")
        );
        assert!(platform.envelope.mentions[0].is_self);
    }

    #[test]
    fn a_message_without_a_sender_is_rejected() {
        let payload = json!({
            "update_id": 107,
            "message": {
                "message_id": 12,
                "date": 1_700_000_700,
                "chat": {"id": 1, "type": "private"},
                "text": "who am i"
            }
        });
        let raw = RawUpdate::new(Channel::Telegram, payload);

        let error = adapter().normalize(&raw).unwrap_err();

        assert_eq!(
            error,
            AdapterError::MissingField {
                field: "message.from".to_string(),
            }
        );
    }

    #[test]
    fn an_update_without_a_message_is_dropped() {
        let raw = RawUpdate::new(
            Channel::Telegram,
            json!({"update_id": 108, "poll": {"id": "1", "question": "?"}}),
        );

        let events = adapter().normalize(&raw).unwrap();

        assert!(events.is_empty());
    }

    #[test]
    fn a_non_object_payload_is_malformed() {
        let raw = RawUpdate::new(Channel::Telegram, json!("not an update"));

        let error = adapter().normalize(&raw).unwrap_err();

        let AdapterError::Malformed { reason } = error else {
            panic!("expected a malformed adapter error");
        };
        assert_eq!(
            reason,
            "invalid type: string \"not an update\", expected struct Update"
        );
    }
}
