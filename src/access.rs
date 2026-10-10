//! Access gate: who may start a turn at all.
//!
//! Default deny — a principal must be listed. Three gates, in order:
//!
//! 1. surface allowlist: DMs must be in `dm_allow`, groups in `groups`;
//! 2. per-group member deny list;
//! 3. group trigger: only `@bot` / a reply to the bot / a `/command` wakes it.
//!
//! A denied message is dropped silently (no refusal text, no turn); the caller
//! still records it for audit/dedup. See `docs/architecture.md` §6.6.

use serde::Deserialize;

use crate::channel::{Channel, ChannelChatType, DeliveryTarget};
use crate::event::{Envelope, Event, EventOrigin, Part};

/// Surface + per-group access policy, loaded from `[access]`.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct AccessPolicy {
    /// Principals allowed to open a DM, e.g. `telegram:123456`.
    #[serde(default)]
    pub dm_allow: Vec<String>,
    /// Groups the bot participates in.
    #[serde(default)]
    pub groups: Vec<GroupAccess>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupAccess {
    /// Group target id, e.g. `telegram:-100123`.
    pub id: String,
    /// Members that must be ignored, e.g. `telegram:999`; default is allow-all.
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Granted,
    Denied,
}

impl AccessPolicy {
    pub fn decide(&self, event: &Event) -> Admission {
        let EventOrigin::Platform(platform) = &event.origin else {
            // Runtime events are produced internally, not by an external principal.
            return Admission::Granted;
        };

        let target = &platform.target;

        match target.channel() {
            // The local entry is implicitly the owner.
            Channel::Webui => Admission::Granted,
            _ => match target.chat_type() {
                ChannelChatType::Direct => self.decide_direct(target),
                ChannelChatType::Group => {
                    self.decide_group(target, &platform.envelope, &platform.parts)
                }
            },
        }
    }

    fn decide_direct(&self, target: &DeliveryTarget) -> Admission {
        if self
            .dm_allow
            .iter()
            .any(|entry| entry == &principal(target))
        {
            Admission::Granted
        } else {
            Admission::Denied
        }
    }

    fn decide_group(
        &self,
        target: &DeliveryTarget,
        envelope: &Envelope,
        parts: &[Part],
    ) -> Admission {
        let Some(group) = self
            .groups
            .iter()
            .find(|group| group.id == principal(target))
        else {
            return Admission::Denied;
        };

        let sender = format!("{}:{}", target.channel(), envelope.sender_platform_id);
        if group.deny.iter().any(|entry| entry == &sender) {
            return Admission::Denied;
        }

        if triggers(envelope, parts) {
            Admission::Granted
        } else {
            Admission::Denied
        }
    }
}

/// `channel:target_id`, the same shape as `[permissions].owner` entries.
fn principal(target: &DeliveryTarget) -> String {
    format!("{}:{}", target.channel(), target.target_id())
}

/// A group message wakes the bot only when it mentions the bot, replies to the
/// bot, or is a `/command`; anything else is dropped without opening a turn.
fn triggers(envelope: &Envelope, parts: &[Part]) -> bool {
    if envelope.mentions.iter().any(|mention| mention.is_self) {
        return true;
    }

    let replied_to_bot = matches!(
        (&envelope.reply_to, &envelope.bot),
        (Some(reply), Some(bot)) if reply.sender_platform_id == bot.platform_id
    );
    if replied_to_bot {
        return true;
    }

    text_of(parts).is_some_and(|text| text.trim_start().starts_with('/'))
}

fn text_of(parts: &[Part]) -> Option<&str> {
    parts.iter().find_map(|part| match part {
        Part::Text { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;
    use crate::event::{ActorRef, BotRef, Mention, ReplyRef};

    fn bot() -> BotRef {
        BotRef {
            actor: ActorRef::new("member_bot"),
            platform_id: "424242".to_string(),
            username: Some("weftic_bot".to_string()),
        }
    }

    fn platform_event(target: DeliveryTarget, text: &str) -> Event {
        Event::platform_text(target, ActorRef::new("member_x"), "0".to_string(), text)
    }

    fn envelope_mut(event: &mut Event) -> &mut Envelope {
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        &mut platform.envelope
    }

    fn direct(chat: &str) -> DeliveryTarget {
        DeliveryTarget::direct(Channel::Telegram, "default", chat)
    }

    fn group(chat: &str) -> DeliveryTarget {
        DeliveryTarget::group(Channel::Telegram, "default", chat)
    }

    fn policy(dm_allow: &[&str], groups: &[(&str, &[&str])]) -> AccessPolicy {
        AccessPolicy {
            dm_allow: dm_allow.iter().map(|s| s.to_string()).collect(),
            groups: groups
                .iter()
                .map(|(id, deny)| GroupAccess {
                    id: id.to_string(),
                    deny: deny.iter().map(|s| s.to_string()).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn webui_is_always_granted() {
        let event = platform_event(
            DeliveryTarget::direct(Channel::Webui, "default", "webui"),
            "hi",
        );

        assert_eq!(AccessPolicy::default().decide(&event), Admission::Granted);
    }

    #[test]
    fn a_listed_dm_is_granted() {
        let policy = policy(&["telegram:123456"], &[]);
        let event = platform_event(direct("123456"), "hi");

        assert_eq!(policy.decide(&event), Admission::Granted);
    }

    #[test]
    fn an_unlisted_dm_is_denied() {
        let event = platform_event(direct("999"), "hi");

        assert_eq!(AccessPolicy::default().decide(&event), Admission::Denied);
    }

    #[test]
    fn an_unlisted_group_is_denied() {
        let mut event = platform_event(group("-100"), "@weftic_bot hi");
        envelope_mut(&mut event).mentions.push(Mention {
            actor: Some(ActorRef::new("member_bot")),
            platform_id: Some("424242".to_string()),
            username: Some("weftic_bot".to_string()),
            name: "weftic_bot".to_string(),
            is_bot: true,
            is_self: true,
        });

        assert_eq!(AccessPolicy::default().decide(&event), Admission::Denied);
    }

    #[test]
    fn a_listed_group_without_a_trigger_is_denied() {
        let policy = policy(&[], &[("telegram:-100", &[])]);
        let event = platform_event(group("-100"), "just chatting");

        assert_eq!(policy.decide(&event), Admission::Denied);
    }

    #[test]
    fn a_mention_of_the_bot_triggers_a_listed_group() {
        let policy = policy(&[], &[("telegram:-100", &[])]);
        let mut event = platform_event(group("-100"), "@weftic_bot hi");
        envelope_mut(&mut event).mentions.push(Mention {
            actor: Some(ActorRef::new("member_bot")),
            platform_id: Some("424242".to_string()),
            username: Some("weftic_bot".to_string()),
            name: "weftic_bot".to_string(),
            is_bot: true,
            is_self: true,
        });

        assert_eq!(policy.decide(&event), Admission::Granted);
    }

    #[test]
    fn a_reply_to_the_bot_triggers_a_listed_group() {
        let policy = policy(&[], &[("telegram:-100", &[])]);
        let mut event = platform_event(group("-100"), "and then?");
        let envelope = envelope_mut(&mut event);
        envelope.bot = Some(bot());
        envelope.reply_to = Some(ReplyRef {
            message_id: "7".to_string(),
            sender: ActorRef::new("member_bot"),
            sender_name: "weftic_bot".to_string(),
            sender_platform_id: "424242".to_string(),
            snippet: None,
        });

        assert_eq!(policy.decide(&event), Admission::Granted);
    }

    #[test]
    fn a_command_triggers_a_listed_group() {
        let policy = policy(&[], &[("telegram:-100", &[])]);
        let event = platform_event(group("-100"), "/status");

        assert_eq!(policy.decide(&event), Admission::Granted);
    }

    #[test]
    fn a_denied_member_is_dropped_before_the_trigger_check() {
        let policy = policy(&[], &[("telegram:-100", &["telegram:999"])]);
        let mut event = platform_event(group("-100"), "@weftic_bot hi");
        let envelope = envelope_mut(&mut event);
        envelope.sender_platform_id = "999".to_string();
        envelope.mentions.push(Mention {
            actor: Some(ActorRef::new("member_bot")),
            platform_id: Some("424242".to_string()),
            username: Some("weftic_bot".to_string()),
            name: "weftic_bot".to_string(),
            is_bot: true,
            is_self: true,
        });

        assert_eq!(policy.decide(&event), Admission::Denied);
    }
}
