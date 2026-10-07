use serde::Deserialize;

pub const DEFAULT_MAX_SEND_ATTEMPTS: u32 = 3;

#[derive(Debug, Deserialize)]
pub struct ChannelConfig {
    pub template: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "default_max_send_attempts")]
    pub max_send_attempts: u32,
}

fn default_max_send_attempts() -> u32 {
    DEFAULT_MAX_SEND_ATTEMPTS
}
