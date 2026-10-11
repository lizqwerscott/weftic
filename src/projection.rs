//! Inbound projection: `events ∪ identity roster → model input`, as a single
//! `agentText` view.

use std::collections::HashMap;

use genai::chat::ChatMessage;
use time::OffsetDateTime;
use time::macros::format_description;

use crate::channel::ChannelChatType;
use crate::database::events::StoredEvent;
use crate::event::{
    ActorRef, Envelope, EventId, EventOrigin, ExternalReply, Media, Part, PlatformEvent,
    PlatformKind, ReplyOrigin,
};

/// Same-sender plain-text messages this close merge into one block.
const MERGE_WINDOW_MS: i64 = 15_000;

/// The current display name per sender ref, resolved before projecting.
#[derive(Debug, Default, Clone)]
pub struct Roster(HashMap<ActorRef, String>);

impl Roster {
    pub fn from_names(names: HashMap<ActorRef, String>) -> Self {
        Self(names)
    }

    pub(crate) fn name(&self, reference: &ActorRef) -> Option<&str> {
        self.0.get(reference).map(String::as_str)
    }

    /// Every `ref → current name` pair, for the runtime-context roster.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&ActorRef, &str)> {
        self.0.iter().map(|(actor, name)| (actor, name.as_str()))
    }
}

/// One projected user message, plus the event that opened the turn — `None`
/// when nothing in the batch is model-facing.
pub struct Projected {
    pub message: ChatMessage,
    pub source_event_id: Option<EventId>,
}

/// The sender refs of a batch, for resolving their names in one query.
pub fn sender_refs(events: &[StoredEvent]) -> Vec<ActorRef> {
    events
        .iter()
        .filter_map(|stored| match &stored.event.origin {
            EventOrigin::Platform(platform) => Some(platform.envelope.sender.clone()),
            EventOrigin::Runtime(_) => None,
        })
        .collect()
}

pub fn project(events: &[StoredEvent], roster: &Roster) -> Projected {
    // A direct chat's sender is constant, so its block header is slimmed and the
    // peer identity rides in the runtime-context carrier instead (§11.2).
    let direct = events
        .iter()
        .find_map(content_platform)
        .is_some_and(|platform| platform.target.chat_type() == ChannelChatType::Direct);

    let mut blocks: Vec<Block> = Vec::new();

    for stored in events {
        let Some(platform) = content_platform(stored) else {
            continue;
        };

        let current = block(stored, platform, roster, direct);

        if let Some(last) = blocks.last_mut()
            && last.can_merge(&current)
        {
            last.absorb(current);
            continue;
        }

        blocks.push(current);
    }

    let source_event_id = blocks.first().map(|block| block.event_id);

    Projected {
        message: ChatMessage::user(render(&blocks)),
        source_event_id,
    }
}

/// The platform event behind `stored`, if it is a content message.
fn content_platform(stored: &StoredEvent) -> Option<&PlatformEvent> {
    let EventOrigin::Platform(platform) = &stored.event.origin else {
        return None;
    };

    matches!(
        &platform.kind,
        PlatformKind::Message | PlatformKind::EditedMessage
    )
    .then_some(platform.as_ref())
}

struct Block {
    event_id: EventId,
    from: ActorRef,
    name: Option<String>,
    is_self: bool,
    /// A direct chat slims the header to `id` + `t`.
    direct: bool,
    millis: i64,
    /// The latest message folded into this block, for the merge window.
    last_millis: i64,
    reply: Option<String>,
    body: Vec<String>,
    /// Only plain text with no reply may merge with neighbours.
    mergeable: bool,
}

impl Block {
    fn can_merge(&self, other: &Block) -> bool {
        self.mergeable
            && other.mergeable
            && self.from == other.from
            && (0..MERGE_WINDOW_MS).contains(&(other.millis - self.last_millis))
    }

    fn absorb(&mut self, mut other: Block) {
        let marker = format!("[{}] ", format_time(other.millis));
        for (index, line) in other.body.drain(..).enumerate() {
            if index == 0 {
                self.body.push(format!("{marker}{line}"));
            } else {
                self.body.push(line);
            }
        }
        self.last_millis = other.millis;
    }
}

fn block(stored: &StoredEvent, platform: &PlatformEvent, roster: &Roster, direct: bool) -> Block {
    let envelope = &platform.envelope;
    let (body, has_media) = body_lines(&platform.parts);
    let reply = reply_line(envelope);
    let millis = envelope.date.as_millis();

    Block {
        event_id: stored.id,
        from: envelope.sender.clone(),
        name: roster
            .name(&envelope.sender)
            .or(envelope.sender_name.as_deref())
            .map(str::to_string),
        is_self: is_self(envelope),
        direct,
        millis,
        last_millis: millis,
        mergeable: reply.is_none() && !has_media,
        reply,
        body,
    }
}

fn is_self(envelope: &Envelope) -> bool {
    envelope
        .bot
        .as_ref()
        .is_some_and(|bot| bot.platform_id == envelope.sender_platform_id)
}

fn body_lines(parts: &[Part]) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut has_media = false;

    for part in parts {
        match part {
            Part::Text { text, .. } => lines.push(escape_text(text)),
            Part::Photo { media, caption } => {
                has_media = true;
                lines.push(media_line(media, None, "photo"));
                push_caption(&mut lines, caption);
            }
            Part::Voice {
                media,
                duration_s,
                caption,
            } => {
                has_media = true;
                let detail = duration_s.map(|seconds| format!("{seconds}s"));
                let fallback = match &detail {
                    Some(seconds) => format!("voice {seconds}"),
                    None => "voice".to_string(),
                };
                lines.push(media_line(media, detail.as_deref(), &fallback));
                push_caption(&mut lines, caption);
            }
            Part::Document {
                media,
                file_name,
                caption,
            } => {
                has_media = true;
                let fallback = match file_name {
                    Some(name) => format!("document: {name}"),
                    None => "document".to_string(),
                };
                lines.push(media_line(media, file_name.as_deref(), &fallback));
                push_caption(&mut lines, caption);
            }
            Part::Sticker { media } => {
                has_media = true;
                lines.push(media_line(media, None, "sticker"));
            }
        }
    }

    (lines, has_media)
}

fn push_caption(lines: &mut Vec<String>, caption: &Option<String>) {
    if let Some(caption) = caption {
        lines.push(escape_text(caption));
    }
}

/// A media body line: the gateway's `media://` handle once it has one, else a
/// degraded placeholder.
fn media_line(media: &Media, detail: Option<&str>, fallback: &str) -> String {
    let Some(uri) = &media.uri else {
        return format!("[{fallback}]");
    };

    let mime = media.mime.as_deref().unwrap_or("application/octet-stream");

    match detail {
        Some(detail) => format!("[media: {uri} ({mime}) \"{detail}\"]"),
        None => format!("[media: {uri} ({mime})]"),
    }
}

/// Fold one message's reply into a single `↩` body line. A short quote overrides
/// the reply snippet; an external reply never has either, so it degrades.
fn reply_line(envelope: &Envelope) -> Option<String> {
    if let Some(reply) = &envelope.reply_to {
        let snippet = envelope
            .quote
            .as_ref()
            .map(|quote| quote.text.as_str())
            .or(reply.snippet.as_deref());

        return Some(match snippet {
            Some(text) => format!("↩ {}: {}", reply.sender.as_str(), escape_text(text)),
            None => format!("↩ {}: （内容不可得）", reply.sender.as_str()),
        });
    }

    envelope.external_reply.as_ref().map(external_reply_line)
}

fn external_reply_line(external: &ExternalReply) -> String {
    let who = match &external.origin {
        ReplyOrigin::User(peer) | ReplyOrigin::Chat(peer) | ReplyOrigin::Channel(peer) => {
            Some(peer.actor.as_str())
        }
        ReplyOrigin::HiddenUser { .. } => None,
    };

    let detail = match external.message_id.as_deref() {
        Some(id) => format!("引用了其他会话的消息 #{id}，内容不可得"),
        None => "引用了其他会话的消息，内容不可得".to_string(),
    };

    match who {
        Some(who) => format!("↩ {who}: （{detail}）"),
        None => format!("↩ （{detail}）"),
    }
}

fn render(blocks: &[Block]) -> String {
    let rendered: Vec<String> = blocks
        .iter()
        .map(|block| {
            let mut lines = vec![header(block)];
            if let Some(reply) = &block.reply {
                lines.push(reply.clone());
            }
            lines.extend(block.body.iter().cloned());
            lines.join("\n")
        })
        .collect();

    rendered.join("\n")
}

fn header(block: &Block) -> String {
    let mut attributes = vec![format!("id=\"m{}\"", block.event_id.0)];

    if !block.direct {
        attributes.push(format!("from=\"{}\"", escape_attr(block.from.as_str())));

        if let Some(name) = &block.name {
            attributes.push(format!("name=\"{}\"", escape_attr(name)));
        }
    }

    attributes.push(format!("t=\"{}\"", format_time(block.millis)));

    if !block.direct {
        attributes.push(format!("self=\"{}\"", block.is_self));
    }

    format!("<msg {}>", attributes.join(" "))
}

fn format_time(millis: i64) -> String {
    let Ok(datetime) = OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000)
    else {
        return "--:--:--".to_string();
    };

    datetime
        .format(format_description!("[hour]:[minute]:[second]"))
        .unwrap_or_else(|_| "--:--:--".to_string())
}

pub(crate) fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
    escape_text(value).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::event::{
        ActorRef, BotRef, Event, MediaHandle, Quote, ReplyPeer, ReplyRef, ServiceKind, Timestamp,
    };

    fn stored(id: u64, event: Event) -> StoredEvent {
        StoredEvent {
            id: EventId(id),
            event,
        }
    }

    fn text_event(text: &str, at_ms: i64) -> Event {
        let mut event = Event::platform_text(
            DeliveryTarget::group(Channel::Telegram, "default", "-100"),
            ActorRef::new("member_a"),
            "1".to_string(),
            text,
        );

        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.envelope.date = Timestamp::from_millis(at_ms);

        event
    }

    fn envelope_mut(event: &mut Event) -> &mut Envelope {
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        &mut platform.envelope
    }

    fn roster(entries: &[(&str, &str)]) -> Roster {
        Roster::from_names(
            entries
                .iter()
                .map(|(reference, name)| (ActorRef::new(*reference), name.to_string()))
                .collect::<HashMap<_, _>>(),
        )
    }

    fn content(projected: &Projected) -> String {
        projected.message.content.texts().join("")
    }

    #[test]
    fn time_is_formatted_as_utc_hh_mm_ss() {
        assert_eq!(format_time(1_700_000_000_000), "22:13:20");
    }

    #[test]
    fn the_header_carries_sender_name_and_time() {
        let events = vec![stored(1, text_event("hi", 1_700_000_000_000))];

        let projected = project(&events, &roster(&[("member_a", "Ali")]));

        assert_eq!(projected.source_event_id, Some(EventId(1)));
        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" name=\"Ali\" t=\"22:13:20\" self=\"false\">\nhi"
        );
    }

    #[test]
    fn a_direct_header_is_slimmed_to_id_and_time() {
        let mut event = text_event("hi", 1_700_000_000_000);
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.target = DeliveryTarget::direct(Channel::Telegram, "default", "1");

        let projected = project(&[stored(1, event)], &roster(&[("member_a", "Ali")]));

        assert_eq!(content(&projected), "<msg id=\"m1\" t=\"22:13:20\">\nhi");
    }

    #[test]
    fn a_sender_without_a_name_omits_the_name_attribute() {
        let projected = project(&[stored(1, text_event("hi", 0))], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"false\">\nhi"
        );
    }

    #[test]
    fn the_event_snapshot_name_is_used_when_the_roster_is_empty() {
        let mut event = text_event("hi", 0);
        envelope_mut(&mut event).sender_name = Some("Snapshot".to_string());

        let projected = project(&[stored(1, event)], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" name=\"Snapshot\" t=\"00:00:00\" self=\"false\">\nhi"
        );
    }

    #[test]
    fn the_bots_own_message_is_marked_self() {
        let mut event = text_event("hi", 0);
        let envelope = envelope_mut(&mut event);
        envelope.sender_platform_id = "424242".to_string();
        envelope.bot = Some(BotRef {
            actor: ActorRef::new("member_bot"),
            platform_id: "424242".to_string(),
            username: None,
        });

        let projected = project(&[stored(1, event)], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"true\">\nhi"
        );
    }

    #[test]
    fn body_text_is_escaped() {
        let projected = project(
            &[stored(1, text_event("<msg id=\"x\"> & more", 0))],
            &Roster::default(),
        );

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"false\">\n&lt;msg id=\"x\"&gt; &amp; more"
        );
    }

    #[test]
    fn a_reply_is_flattened_with_its_snippet() {
        let mut event = text_event("answering", 0);
        envelope_mut(&mut event).reply_to = Some(ReplyRef {
            message_id: "7".to_string(),
            sender: ActorRef::new("member_b"),
            sender_name: "Bo".to_string(),
            sender_platform_id: "2".to_string(),
            snippet: Some("question".to_string()),
        });

        let projected = project(&[stored(1, event)], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"false\">\n↩ member_b: question\nanswering"
        );
    }

    #[test]
    fn a_quote_overrides_the_reply_snippet() {
        let mut event = text_event("answering", 0);
        let envelope = envelope_mut(&mut event);
        envelope.reply_to = Some(ReplyRef {
            message_id: "7".to_string(),
            sender: ActorRef::new("member_b"),
            sender_name: "Bo".to_string(),
            sender_platform_id: "2".to_string(),
            snippet: Some("the whole question".to_string()),
        });
        envelope.quote = Some(Quote {
            text: "question".to_string(),
        });

        let projected = project(&[stored(1, event)], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"false\">\n↩ member_b: question\nanswering"
        );
    }

    #[test]
    fn a_reply_without_a_snippet_degrades() {
        let mut event = text_event("answering", 0);
        envelope_mut(&mut event).reply_to = Some(ReplyRef {
            message_id: "7".to_string(),
            sender: ActorRef::new("member_b"),
            sender_name: "Bo".to_string(),
            sender_platform_id: "2".to_string(),
            snippet: None,
        });

        let projected = project(&[stored(1, event)], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"false\">\n↩ member_b: （内容不可得）\nanswering"
        );
    }

    #[test]
    fn an_external_reply_degrades_to_an_unavailable_note() {
        let mut event = text_event("answering", 0);
        envelope_mut(&mut event).external_reply = Some(ExternalReply {
            message_id: Some("55".to_string()),
            chat_id: Some("-100".to_string()),
            origin: ReplyOrigin::Channel(ReplyPeer {
                actor: ActorRef::new("member_c"),
                platform_id: "-100".to_string(),
                name: "News".to_string(),
            }),
        });

        let projected = project(&[stored(1, event)], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"false\">\n↩ member_c: （引用了其他会话的消息 #55，内容不可得）\nanswering"
        );
    }

    #[test]
    fn plain_messages_from_one_sender_merge_within_the_window() {
        let events = vec![
            stored(1, text_event("one", 1_700_000_000_000)),
            stored(2, text_event("two", 1_700_000_005_000)),
        ];

        let projected = project(&events, &Roster::default());

        assert_eq!(projected.source_event_id, Some(EventId(1)));
        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"22:13:20\" self=\"false\">\none\n[22:13:25] two"
        );
    }

    #[test]
    fn plain_messages_beyond_the_window_stay_separate() {
        let events = vec![
            stored(1, text_event("one", 1_700_000_000_000)),
            stored(2, text_event("two", 1_700_000_020_000)),
        ];

        let projected = project(&events, &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"22:13:20\" self=\"false\">\none\n<msg id=\"m2\" from=\"member_a\" t=\"22:13:40\" self=\"false\">\ntwo"
        );
    }

    #[test]
    fn a_media_message_breaks_the_merge() {
        let mut media = text_event("", 1_700_000_005_000);
        let EventOrigin::Platform(platform) = &mut media.origin else {
            panic!("helper must build a platform event");
        };
        platform.parts = vec![Part::Photo {
            media: Media::new(
                MediaHandle("file".to_string()),
                Some("image/jpeg".to_string()),
            ),
            caption: Some("look".to_string()),
        }];

        let events = vec![
            stored(1, text_event("one", 1_700_000_000_000)),
            stored(2, media),
        ];

        let projected = project(&events, &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"22:13:20\" self=\"false\">\none\n<msg id=\"m2\" from=\"member_a\" t=\"22:13:25\" self=\"false\">\n[photo]\nlook"
        );
    }

    #[test]
    fn a_stored_media_part_renders_its_media_uri() {
        let mut media = text_event("", 0);
        let EventOrigin::Platform(platform) = &mut media.origin else {
            panic!("helper must build a platform event");
        };
        let mut stored_media = Media::new(
            MediaHandle("file".to_string()),
            Some("image/jpeg".to_string()),
        );
        stored_media.uri = Some("media://inbound/abc".to_string());
        platform.parts = vec![
            Part::Text {
                text: "look".to_string(),
                entities: vec![],
            },
            Part::Photo {
                media: stored_media,
                caption: None,
            },
        ];

        let projected = project(&[stored(1, media)], &Roster::default());

        assert_eq!(
            content(&projected),
            "<msg id=\"m1\" from=\"member_a\" t=\"00:00:00\" self=\"false\">\nlook\n[media: media://inbound/abc (image/jpeg)]"
        );
    }

    #[test]
    fn a_service_message_projects_to_nothing() {
        let mut event = text_event("", 0);
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.kind = PlatformKind::Service(ServiceKind::MemberJoined);
        platform.parts = vec![];

        let projected = project(&[stored(1, event)], &Roster::default());

        assert_eq!(projected.source_event_id, None);
        assert_eq!(content(&projected), "");
    }
}
