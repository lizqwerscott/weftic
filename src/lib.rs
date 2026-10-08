use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::{
    channel::{
        Channel,
        cli::CliRuntime,
        registry::ChannelRegistry,
        telegram::{client::TelegramClient, runtime::TelegramRuntime},
    },
    config::{Config, channel::DEFAULT_MAX_SEND_ATTEMPTS},
};

pub mod agent_engine;
pub mod channel;
pub mod config;
pub mod event;
pub mod event_store;
pub mod identity;
pub mod inbound;
pub mod output;
pub mod permissions;
pub mod session;
pub mod system_prompt;
pub mod tools;
pub mod tui;

pub fn registry_channels(config: &Config) -> Result<Arc<ChannelRegistry>> {
    let mut registry = ChannelRegistry::new();

    if config.is_channel_enabled("cli") {
        registry.register(Channel::Cli, Arc::new(CliRuntime));
        info!(target: "cli", "channel registered");
    }

    if config.is_channel_enabled("telegram") {
        if let Some(token) = config.channel_token("telegram")? {
            let attempts = config
                .channels
                .get("telegram")
                .map(|channel| channel.max_send_attempts)
                .unwrap_or(DEFAULT_MAX_SEND_ATTEMPTS);
            let client = Arc::new(TelegramClient::new(token));
            registry.register(
                Channel::Telegram,
                Arc::new(TelegramRuntime::new(client, "default", attempts)),
            );
            info!(target: "telegram", "channel registered (account `default`)");
        } else {
            warn!(target: "telegram", "channel enabled but no token configured; skipping");
        }
    }

    Ok(Arc::new(registry))
}
