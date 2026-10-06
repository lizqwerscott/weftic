use std::borrow::Borrow;
use std::fmt;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

const KEY_FIELD_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub mod cli;
pub mod sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelChatType {
    Direct,
    Group,
}

impl fmt::Display for ChannelChatType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => write!(f, "direct"),
            Self::Group => write!(f, "group"),
        }
    }
}

impl ChannelChatType {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "group" => Some(Self::Group),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    Cli,
    Webui,
    Telegram,
    QQ,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli => write!(f, "cli"),
            Self::QQ => write!(f, "qq"),
            Self::Telegram => write!(f, "telegram"),
            Self::Webui => write!(f, "webui"),
        }
    }
}

impl Channel {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cli" => Some(Self::Cli),
            "webui" => Some(Self::Webui),
            "telegram" => Some(Self::Telegram),
            "qq" => Some(Self::QQ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey(String);

impl SessionKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(&self) -> Option<DeliveryTarget> {
        let mut parts = self.0.split(':');

        if parts.next() != Some("agent") {
            return None;
        }

        let _agent_id = parts.next()?;
        let channel = Channel::parse(parts.next()?)?;
        let account_id = percent_decode_str(parts.next()?).decode_utf8().ok()?;
        let chat_type = ChannelChatType::parse(parts.next()?)?;
        let target_id = percent_decode_str(parts.next()?).decode_utf8().ok()?;

        if parts.next().is_some() {
            return None;
        }

        Some(match chat_type {
            ChannelChatType::Direct => DeliveryTarget::direct(channel, account_id, target_id),
            ChannelChatType::Group => DeliveryTarget::group(channel, account_id, target_id),
        })
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SessionKey {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SessionKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl Borrow<str> for SessionKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeliveryTarget {
    channel: Channel,
    account_id: String,
    chat_type: ChannelChatType,
    target_id: String,
}

impl DeliveryTarget {
    pub fn direct(
        channel: Channel,
        account_id: impl Into<String>,
        target_id: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            account_id: account_id.into(),
            chat_type: ChannelChatType::Direct,
            target_id: target_id.into(),
        }
    }

    pub fn group(
        channel: Channel,
        account_id: impl Into<String>,
        target_id: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            account_id: account_id.into(),
            chat_type: ChannelChatType::Group,
            target_id: target_id.into(),
        }
    }

    pub fn to_session_key(&self, agent_id: &str) -> SessionKey {
        let account_id = utf8_percent_encode(&self.account_id, KEY_FIELD_ENCODE_SET);
        let target_id = utf8_percent_encode(&self.target_id, KEY_FIELD_ENCODE_SET);
        SessionKey(format!(
            "agent:{}:{}:{}:{}:{}",
            agent_id, self.channel, account_id, self.chat_type, target_id
        ))
    }

    pub fn channel(&self) -> &Channel {
        &self.channel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_direct_key_matches_architecture_example() {
        let key = DeliveryTarget::direct(Channel::Cli, "default", "cli").to_session_key("main");
        assert_eq!(key.as_str(), "agent:main:cli:default:direct:cli");
    }

    #[test]
    fn webui_direct_key_matches_architecture_example() {
        let key =
            DeliveryTarget::direct(Channel::Webui, "default", "lizqwer").to_session_key("main");
        assert_eq!(key.as_str(), "agent:main:webui:default:direct:lizqwer");
    }

    #[test]
    fn telegram_group_key_keeps_negative_id_verbatim() {
        let key =
            DeliveryTarget::group(Channel::Telegram, "default", "-1001234").to_session_key("main");
        assert_eq!(key.as_str(), "agent:main:telegram:default:group:-1001234");
    }

    #[test]
    fn target_id_separators_are_percent_encoded() {
        let key = DeliveryTarget::group(Channel::QQ, "default", "a:b/c").to_session_key("main");
        assert_eq!(key.as_str(), "agent:main:qq:default:group:a%3Ab%2Fc");
    }

    #[test]
    fn direct_and_group_keys_differ_for_the_same_target_id() {
        let direct =
            DeliveryTarget::direct(Channel::Telegram, "default", "1").to_session_key("main");
        let group = DeliveryTarget::group(Channel::Telegram, "default", "1").to_session_key("main");
        assert_ne!(direct, group);
    }

    #[test]
    fn session_key_displays_its_canonical_form() {
        let key = DeliveryTarget::direct(Channel::Cli, "default", "cli").to_session_key("main");
        assert_eq!(key.to_string(), "agent:main:cli:default:direct:cli");
    }

    #[test]
    fn account_id_separators_are_percent_encoded() {
        let key = DeliveryTarget::direct(Channel::Telegram, "a:b", "42").to_session_key("main");
        assert_eq!(key.as_str(), "agent:main:telegram:a%3Ab:direct:42");
    }

    #[test]
    fn session_key_round_trips_direct_targets() {
        let target = DeliveryTarget::direct(Channel::Webui, "default", "lizqwer");
        let key = target.to_session_key("main");
        assert_eq!(key.parse(), Some(target));
    }

    #[test]
    fn session_key_round_trips_targets_with_separators() {
        let target = DeliveryTarget::group(Channel::QQ, "a:b", "x:y/z");
        let key = target.to_session_key("main");
        assert_eq!(key.parse(), Some(target));
    }

    #[test]
    fn parse_rejects_a_missing_agent_prefix() {
        let key = SessionKey::from("main:cli:default:direct:cli");
        assert_eq!(key.parse(), None);
    }

    #[test]
    fn parse_rejects_a_missing_segment() {
        let key = SessionKey::from("agent:main:cli:default:direct");
        assert_eq!(key.parse(), None);
    }

    #[test]
    fn parse_rejects_a_trailing_segment() {
        let key = SessionKey::from("agent:main:cli:default:direct:cli:extra");
        assert_eq!(key.parse(), None);
    }

    #[test]
    fn parse_rejects_an_unknown_channel() {
        let key = SessionKey::from("agent:main:signal:default:direct:1");
        assert_eq!(key.parse(), None);
    }

    #[test]
    fn parse_rejects_an_unknown_chat_type() {
        let key = SessionKey::from("agent:main:cli:default:channel:1");
        assert_eq!(key.parse(), None);
    }

    #[test]
    fn parse_rejects_invalid_percent_encoding() {
        let key = SessionKey::from("agent:main:cli:default:direct:%FF");
        assert_eq!(key.parse(), None);
    }
}
