//! Deterministic opaque identity references: `member_<hex>`, plus the name
//! facts we observe for a ref (see `docs/architecture.md` §12).

use crate::channel::{Channel, ChannelChatType};
use crate::event::{ActorRef, Event, EventOrigin};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

pub fn member_ref(channel: Channel, account_id: &str, actor_id: &str) -> ActorRef {
    let key = format!("{channel}\u{1f}{account_id}\u{1f}{actor_id}");
    ActorRef::new(format!("member_{:016x}", fnv1a(key.as_bytes())))
}

/// A name observation extracted from an inbound event: which ref it is, and the
/// display name the platform reported this time. Stored by
/// `Database::observe_identity`; the projection reads the current name back for
/// the message block header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedIdentity {
    pub reference: ActorRef,
    pub channel: Channel,
    pub account_id: String,
    pub platform_id: String,
    pub name: Option<String>,
    /// Set in groups, where the same member may carry a per-chat nickname.
    pub chat_scope: Option<String>,
}

impl ObservedIdentity {
    /// Extract the sender's identity facts from a platform event. Runtime
    /// events have no external principal, so they yield `None`.
    pub fn from_event(event: &Event) -> Option<Self> {
        let EventOrigin::Platform(platform) = &event.origin else {
            return None;
        };

        let envelope = &platform.envelope;
        let target = &platform.target;

        let chat_scope = match target.chat_type() {
            ChannelChatType::Group => Some(target.target_id().to_string()),
            ChannelChatType::Direct => None,
        };

        Some(Self {
            reference: envelope.sender.clone(),
            channel: *target.channel(),
            account_id: target.account_id().to_string(),
            platform_id: envelope.sender_platform_id.clone(),
            name: envelope.sender_name.clone(),
            chat_scope,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::DeliveryTarget;
    use crate::event::{Delivery, InternalScope, RuntimeEvent, RuntimeKind};

    #[test]
    fn ref_is_stable_for_a_known_actor() {
        assert_eq!(
            member_ref(Channel::Telegram, "default", "123456").as_str(),
            "member_9090e286bd875016"
        );
    }

    #[test]
    fn ref_is_deterministic() {
        let first = member_ref(Channel::Telegram, "default", "42");
        let again = member_ref(Channel::Telegram, "default", "42");

        assert_eq!(first, again);
    }

    #[test]
    fn refs_differ_across_channel_account_and_actor() {
        let base = member_ref(Channel::Telegram, "default", "42");

        assert_ne!(base, member_ref(Channel::QQ, "default", "42"));
        assert_ne!(base, member_ref(Channel::Telegram, "second", "42"));
        assert_ne!(base, member_ref(Channel::Telegram, "default", "43"));
    }

    #[test]
    fn observation_extracts_a_direct_sender() {
        let mut event = Event::platform_text(
            DeliveryTarget::direct(Channel::Telegram, "default", "123456"),
            ActorRef::new("member_abc"),
            "123456".to_string(),
            "hi",
        );

        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.envelope.sender_name = Some("Ali".to_string());

        assert_eq!(
            ObservedIdentity::from_event(&event),
            Some(ObservedIdentity {
                reference: ActorRef::new("member_abc"),
                channel: Channel::Telegram,
                account_id: "default".to_string(),
                platform_id: "123456".to_string(),
                name: Some("Ali".to_string()),
                chat_scope: None,
            })
        );
    }

    #[test]
    fn observation_scopes_the_name_to_the_chat_in_groups() {
        let event = Event::platform_text(
            DeliveryTarget::group(Channel::Telegram, "default", "-100"),
            ActorRef::new("member_abc"),
            "42".to_string(),
            "hi",
        );

        let observed = ObservedIdentity::from_event(&event).unwrap();

        assert_eq!(observed.chat_scope.as_deref(), Some("-100"));
    }

    #[test]
    fn a_runtime_event_yields_no_observation() {
        let event = Event {
            delivery: Delivery::Durable,
            origin: EventOrigin::Runtime(RuntimeEvent {
                kind: RuntimeKind::Notice,
                scope: InternalScope::Heartbeat,
                correlation_id: None,
            }),
        };

        assert_eq!(ObservedIdentity::from_event(&event), None);
    }
}
