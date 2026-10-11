use serde::Deserialize;

use crate::channel::ChannelChatType;

pub const DEFAULT_MAX_SEND_ATTEMPTS: u32 = 3;

#[derive(Debug, Deserialize)]
pub struct ChannelConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
    /// Prompt template for group chats (§15).
    pub template_group: String,
    /// Prompt template for direct chats (§15).
    pub template_direct: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "default_max_send_attempts")]
    pub max_send_attempts: u32,
}

impl ChannelConfig {
    pub fn templates(&self) -> ChannelTemplates {
        ChannelTemplates {
            group: self.template_group.clone(),
            direct: self.template_direct.clone(),
        }
    }
}

/// The prompt template names for one channel, per chat type (§15).
#[derive(Debug, Clone)]
pub struct ChannelTemplates {
    pub group: String,
    pub direct: String,
}

impl ChannelTemplates {
    /// Both chat types use the same template.
    pub fn uniform(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            group: name.clone(),
            direct: name,
        }
    }

    pub fn for_chat_type(&self, chat_type: ChannelChatType) -> &str {
        match chat_type {
            ChannelChatType::Group => &self.group,
            ChannelChatType::Direct => &self.direct,
        }
    }

    pub fn names(&self) -> [&str; 2] {
        [self.group.as_str(), self.direct.as_str()]
    }
}

fn default_false() -> bool {
    false
}

fn default_max_send_attempts() -> u32 {
    DEFAULT_MAX_SEND_ATTEMPTS
}
