//! Permission model: mode × role decide which tool groups a session may use.

use serde::Deserialize;

use crate::channel::Channel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Chat,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Member,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolGroup {
    Outbound,
    File,
    Exec,
}

pub fn allowed_groups(mode: Mode, role: Role) -> Vec<ToolGroup> {
    match role {
        Role::Member => vec![ToolGroup::Outbound],
        Role::Owner => match mode {
            Mode::Chat => vec![ToolGroup::Outbound],
            Mode::Agent => vec![ToolGroup::Outbound, ToolGroup::File, ToolGroup::Exec],
        },
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub owner: Vec<String>,
}

impl Permissions {
    pub fn is_owner(&self, channel: &Channel, target_id: &str) -> bool {
        let needle = format!("{channel}:{target_id}");
        self.owner.iter().any(|entry| entry == &needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_only_gets_outbound_in_either_mode() {
        assert_eq!(
            allowed_groups(Mode::Chat, Role::Member),
            vec![ToolGroup::Outbound]
        );
        assert_eq!(
            allowed_groups(Mode::Agent, Role::Member),
            vec![ToolGroup::Outbound]
        );
    }

    #[test]
    fn owner_chat_gets_only_outbound() {
        assert_eq!(
            allowed_groups(Mode::Chat, Role::Owner),
            vec![ToolGroup::Outbound]
        );
    }

    #[test]
    fn owner_agent_gets_every_group() {
        let groups = allowed_groups(Mode::Agent, Role::Owner);

        assert_eq!(
            groups,
            vec![ToolGroup::Outbound, ToolGroup::File, ToolGroup::Exec]
        );
    }

    #[test]
    fn owner_allowlist_matches_both_channel_and_id() {
        let permissions = Permissions {
            owner: vec!["telegram:123456".to_string()],
        };

        assert!(permissions.is_owner(&Channel::Telegram, "123456"));
        assert!(!permissions.is_owner(&Channel::Telegram, "999"));
        assert!(!permissions.is_owner(&Channel::QQ, "123456"));
    }
}
