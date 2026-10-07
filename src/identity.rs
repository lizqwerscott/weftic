//! Deterministic opaque identity references: `member_<hex>`.

use crate::channel::Channel;
use crate::event::ActorRef;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
