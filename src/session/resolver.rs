use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
use crate::channel::registry::ChannelRegistry;
use crate::channel::{Channel, ChannelChatType, DeliveryTarget, SessionKey};
use crate::permissions::{Mode, Permissions, Role};
use crate::session::ChannelBinding;
use crate::session::spec::SessionSpec;
use crate::workspace::WorkspacePolicy;

pub struct SessionResolver {
    workspace_base: PathBuf,
    channel_templates: HashMap<Channel, String>,
    registry: Arc<ChannelRegistry>,
    permissions: Permissions,
}

#[derive(Debug, Clone)]
pub enum ResolveError {
    MalformedKey { key: SessionKey },
    UnconfiguredChannel { channel: Channel },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::MalformedKey { key } => write!(f, "malformed session key `{key}`"),
            ResolveError::UnconfiguredChannel { channel } => {
                write!(f, "no template configured for channel `{channel}`")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

impl SessionResolver {
    pub fn new(
        workspace_base: PathBuf,
        channel_templates: HashMap<Channel, String>,
        registry: Arc<ChannelRegistry>,
        permissions: Permissions,
    ) -> Self {
        Self {
            workspace_base,
            channel_templates,
            registry,
            permissions,
        }
    }

    pub fn spec_for(&self, key: &SessionKey) -> Result<SessionSpec, ResolveError> {
        let target = key
            .parse()
            .ok_or_else(|| ResolveError::MalformedKey { key: key.clone() })?;

        let channel = *target.channel();
        let template = self
            .channel_templates
            .get(&channel)
            .ok_or(ResolveError::UnconfiguredChannel { channel })?
            .clone();

        let role = self.role_for(&target);
        let mode = match role {
            Role::Member => Mode::Chat,
            Role::Owner => Mode::Agent,
        };

        let binding = match self.registry.runtime(channel) {
            Some(runtime) => ChannelBinding {
                capabilities: runtime.capabilities(),
                sender: runtime.sender(&target),
            },
            None => ChannelBinding {
                capabilities: ChannelCapabilities::new(StreamMode::Off, ReplyMode::Automatic),
                sender: None,
            },
        };

        let derive = self.workspace_base.join(format!(
            "{channel}/{}/{}",
            target.chat_type(),
            target.target_id()
        ));

        let workspace_policy = WorkspacePolicy::new(derive, role);

        Ok(SessionSpec {
            key: key.clone(),
            workspace_policy,
            template,
            mode,
            role,
            binding,
        })
    }

    fn role_for(&self, target: &DeliveryTarget) -> Role {
        match target.channel() {
            Channel::Webui => Role::Owner,
            _ => match target.chat_type() {
                ChannelChatType::Group => Role::Member,
                ChannelChatType::Direct => {
                    if self
                        .permissions
                        .is_owner(target.channel(), target.target_id())
                    {
                        Role::Owner
                    } else {
                        Role::Member
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::DeliveryTarget;
    use crate::channel::registry::testing::MockRuntime;
    use crate::channel::telegram::client::TelegramClient;
    use crate::channel::telegram::runtime::TelegramRuntime;

    fn resolver() -> SessionResolver {
        let mut channel_templates = HashMap::new();
        channel_templates.insert(Channel::Webui, "agent".to_string());
        channel_templates.insert(Channel::Telegram, "telegram".to_string());

        let mut registry = ChannelRegistry::new();
        registry.register(Channel::Webui, MockRuntime::live());
        registry.register(
            Channel::Telegram,
            Arc::new(TelegramRuntime::new(
                Arc::new(TelegramClient::new("token")),
                "default",
                3,
            )),
        );

        let permissions = Permissions {
            owner: vec!["telegram:123456".to_string()],
        };

        SessionResolver::new(
            PathBuf::from("/work"),
            channel_templates,
            Arc::new(registry),
            permissions,
        )
    }

    #[test]
    fn webui_spec_is_owner_agent_with_automatic_replies() {
        let key =
            DeliveryTarget::direct(Channel::Webui, "default", "lizqwer").to_session_key("main");

        let spec = resolver().spec_for(&key).unwrap();

        assert_eq!(spec.key, key);
        assert_eq!(
            spec.workspace_policy,
            WorkspacePolicy::new(PathBuf::from("/work/webui/direct/lizqwer"), Role::Owner)
        );
        assert_eq!(spec.template, "agent");
        assert_eq!(spec.role, Role::Owner);
        assert_eq!(spec.mode, Mode::Agent);
        assert_eq!(spec.binding.capabilities.reply, ReplyMode::Automatic);
        assert!(spec.binding.sender.is_none());
    }

    #[test]
    fn a_telegram_owner_direct_chat_is_owner_agent_via_the_message_tool() {
        let key =
            DeliveryTarget::direct(Channel::Telegram, "default", "123456").to_session_key("main");

        let spec = resolver().spec_for(&key).unwrap();

        assert_eq!(spec.role, Role::Owner);
        assert_eq!(spec.mode, Mode::Agent);
        assert_eq!(spec.binding.capabilities.reply, ReplyMode::MessageTool);
        assert!(spec.binding.sender.is_some());
    }

    #[test]
    fn a_telegram_stranger_direct_chat_is_member_chat() {
        let key =
            DeliveryTarget::direct(Channel::Telegram, "default", "999").to_session_key("main");

        let spec = resolver().spec_for(&key).unwrap();

        assert_eq!(spec.role, Role::Member);
        assert_eq!(spec.mode, Mode::Chat);
    }

    #[test]
    fn a_telegram_group_is_member_chat() {
        let key =
            DeliveryTarget::group(Channel::Telegram, "default", "-100").to_session_key("main");

        let spec = resolver().spec_for(&key).unwrap();

        assert_eq!(spec.role, Role::Member);
        assert_eq!(spec.mode, Mode::Chat);
        assert_eq!(
            spec.workspace_policy,
            WorkspacePolicy::new(PathBuf::from("/work/telegram/group/-100"), Role::Member)
        );
    }

    #[test]
    fn unconfigured_channel_is_rejected() {
        let key = DeliveryTarget::direct(Channel::QQ, "default", "1").to_session_key("main");

        let error = resolver().spec_for(&key).unwrap_err();

        assert_eq!(error.to_string(), "no template configured for channel `qq`");
    }

    #[test]
    fn malformed_key_is_rejected() {
        let key = SessionKey::from("not-a-key");

        let error = resolver().spec_for(&key).unwrap_err();

        assert_eq!(error.to_string(), "malformed session key `not-a-key`");
    }
}
