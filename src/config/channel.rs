use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ChannelConfig {
    pub template: String,
    #[serde(default)]
    pub token: Option<String>,
}
