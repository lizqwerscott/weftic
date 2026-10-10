use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::{
    channel::{
        Channel,
        registry::ChannelRegistry,
        telegram::{client::TelegramClient, runtime::TelegramRuntime},
    },
    config::{Config, channel::DEFAULT_MAX_SEND_ATTEMPTS},
};

pub mod access;
pub mod agent_engine;
pub mod channel;
pub mod config;
pub mod database;
pub mod event;
pub mod identity;
pub mod inbound;
pub mod output;
pub mod permissions;
pub mod session;
pub mod system_prompt;
pub mod tools;
mod workspace;

pub async fn registry_channels(config: &Config) -> Result<Arc<ChannelRegistry>> {
    let mut registry = ChannelRegistry::new();

    if config.is_channel_enabled("telegram") {
        if let Some(token) = config.channel_token("telegram")? {
            let attempts = config
                .channels
                .get("telegram")
                .map(|channel| channel.max_send_attempts)
                .unwrap_or(DEFAULT_MAX_SEND_ATTEMPTS);
            let client = Arc::new(TelegramClient::new(token));
            let bot = client
                .get_me()
                .await
                .context("fetching the telegram bot identity")?;
            registry.register(
                Channel::Telegram,
                Arc::new(TelegramRuntime::new(client, "default", bot, attempts)),
            );
            info!(target: "telegram", "channel registered (account `default`)");
        } else {
            warn!(target: "telegram", "channel enabled but no token configured; skipping");
        }
    }

    Ok(Arc::new(registry))
}
