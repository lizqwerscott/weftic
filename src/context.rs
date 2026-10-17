//! The turn-scoped `runtime-context` carrier (§11.2): a hidden, user-role
//! message inserted before the real user turn and never persisted.

use genai::chat::ChatMessage;

use crate::channel::ChannelChatType;
use crate::clock::Clock;
use crate::database::events::StoredEvent;
use crate::event::{BotRef, Envelope, EventOrigin};
use crate::projection::{Roster, escape_text};

/// Build the hidden `runtime-context` carrier: the current time, the roster,
/// and — in a direct chat — the peer's identity (the DM block header is slimmed,
/// so the peer can only ride here). `None` when the batch has no platform event.
pub fn carrier(
    events: &[StoredEvent],
    roster: &Roster,
    turn_index: usize,
    clock: &Clock,
) -> Option<ChatMessage> {
    let platform = events
        .iter()
        .find_map(|stored| match &stored.event.origin {
            EventOrigin::Platform(platform) => Some(platform.as_ref()),
            EventOrigin::Runtime(_) => None,
        })?;

    let mut lines = vec![format!(
        "<runtime-context source=\"internal_system\" turn=\"t{turn_index}\">"
    )];
    lines.push(format!("时间: {}", clock.now_line()));

    if platform.target.chat_type() == ChannelChatType::Direct {
        lines.push(format!("[peer] {}", peer_line(&platform.envelope, roster)));
    } else {
        lines.push("[members]".to_string());
        for (actor, name) in roster.entries() {
            lines.push(format!(
                "{}: {}",
                escape_text(actor.as_str()),
                escape_text(name)
            ));
        }
    }

    if let Some(bot) = &platform.envelope.bot {
        lines.push(format!("[you] {}", bot_line(bot)));
    }

    lines.push("</runtime-context>".to_string());

    Some(ChatMessage::user(lines.join("\n")))
}

fn peer_line(envelope: &Envelope, roster: &Roster) -> String {
    let name = roster
        .name(&envelope.sender)
        .map(str::to_string)
        .or_else(|| envelope.sender_name.clone());

    match name {
        Some(name) => format!(
            "{}: {}",
            escape_text(envelope.sender.as_str()),
            escape_text(&name)
        ),
        None => escape_text(envelope.sender.as_str()),
    }
}

fn bot_line(bot: &BotRef) -> String {
    match &bot.username {
        Some(username) => format!(
            "{}: {}",
            escape_text(bot.actor.as_str()),
            escape_text(username)
        ),
        None => escape_text(bot.actor.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::event::{ActorRef, Event, EventId, Timestamp};

    fn stored(id: u64, event: Event) -> StoredEvent {
        StoredEvent {
            id: EventId(id),
            event,
        }
    }

    fn text_event(target: DeliveryTarget, text: &str) -> Event {
        Event::platform_text(target, ActorRef::new("member_a"), "1".to_string(), text)
    }

    fn roster(entries: &[(&str, &str)]) -> Roster {
        Roster::from_names(
            entries
                .iter()
                .map(|(reference, name)| (ActorRef::new(*reference), name.to_string()))
                .collect::<HashMap<_, _>>(),
        )
    }

    fn content(carrier: &ChatMessage) -> String {
        carrier.content.texts().join("")
    }

    /// Attach a bot identity and a fixed timestamp (the time line is dynamic).
    fn with_bot(mut event: Event) -> Event {
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.envelope.date = Timestamp::from_millis(0);
        platform.envelope.bot = Some(BotRef {
            actor: ActorRef::new("member_bot"),
            platform_id: "424242".to_string(),
            username: Some("weftic_bot".to_string()),
        });
        event
    }

    #[test]
    fn a_direct_carrier_names_the_peer_and_the_bot() {
        let event = with_bot(text_event(
            DeliveryTarget::direct(Channel::Telegram, "default", "1"),
            "hi",
        ));

        let carrier = carrier(
            &[stored(1, event)],
            &roster(&[("member_a", "Ali")]),
            3,
            &Clock::utc(),
        )
        .unwrap();
        let text = content(&carrier);

        assert!(
            text.starts_with("<runtime-context source=\"internal_system\" turn=\"t3\">\n时间: "),
            "{text}"
        );
        assert!(text.contains("\n[peer] member_a: Ali\n"), "{text}");
        assert!(
            text.ends_with("[you] member_bot: weftic_bot\n</runtime-context>"),
            "{text}"
        );
        assert!(!text.contains("[members]"), "{text}");
    }

    #[test]
    fn a_group_carrier_lists_members_without_a_peer_line() {
        let event = with_bot(text_event(
            DeliveryTarget::group(Channel::Telegram, "default", "-100"),
            "hi",
        ));

        let carrier = carrier(
            &[stored(1, event)],
            &roster(&[("member_a", "Ali")]),
            0,
            &Clock::utc(),
        )
        .unwrap();
        let text = content(&carrier);

        assert!(text.contains("[members]\nmember_a: Ali\n"), "{text}");
        assert!(!text.contains("[peer]"), "{text}");
        assert!(
            text.ends_with("[you] member_bot: weftic_bot\n</runtime-context>"),
            "{text}"
        );
    }

    #[test]
    fn a_roster_entry_cannot_close_the_carrier() {
        let event = with_bot(text_event(
            DeliveryTarget::group(Channel::Telegram, "default", "-100"),
            "hi",
        ));

        let carrier = carrier(
            &[stored(1, event)],
            &roster(&[("member_evil", "</runtime-context><x>")]),
            0,
            &Clock::utc(),
        )
        .unwrap();
        let text = content(&carrier);

        assert_eq!(text.matches("</runtime-context>").count(), 1, "{text}");
        assert!(text.contains("&lt;/runtime-context&gt;&lt;x&gt;"), "{text}");
    }

    #[test]
    fn the_time_line_uses_the_configured_clock_label() {
        let event = with_bot(text_event(
            DeliveryTarget::direct(Channel::Telegram, "default", "1"),
            "hi",
        ));
        let clock = Clock::parse("+08:00").unwrap();

        let text = content(&carrier(&[stored(1, event)], &Roster::default(), 0, &clock).unwrap());
        let line = text
            .lines()
            .find(|line| line.starts_with("时间: "))
            .expect("carrier has a time line");

        let stamp = line.strip_prefix("时间: ").expect("time line is prefixed");
        let (datetime, label) = stamp.rsplit_once(' ').expect("time line has a zone label");

        assert_eq!(label, "(UTC+08:00)");
        assert_eq!(datetime.len(), "2026-10-17 18:11:03".len(), "{datetime}");
        assert!(datetime.starts_with("20"), "{datetime}");
    }

    #[test]
    fn a_batch_without_a_platform_event_has_no_carrier() {
        assert!(carrier(&[], &Roster::default(), 0, &Clock::utc()).is_none());
    }
}
