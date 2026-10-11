//! The telegram channel runtime: registers telegram with the channel registry
//! and ties the outbound sender and inbound poller to one shared client.

use std::sync::Arc;

use crate::channel::DeliveryTarget;
use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
use crate::channel::registry::{ChannelRuntime, InboundDriver};
use crate::channel::sender::ChannelSender;
use crate::channel::telegram::client::{BotMe, TelegramClient};
use crate::channel::telegram::poll::TelegramPoller;
use crate::channel::telegram::send::TelegramSender;
use crate::event::MediaHandle;
use crate::media::{BoxedFetchFuture, MediaFetcher};

/// Downloads Telegram media by `file_id`.
pub struct TelegramMediaFetcher {
    client: Arc<TelegramClient>,
}

impl TelegramMediaFetcher {
    pub fn new(client: Arc<TelegramClient>) -> Self {
        Self { client }
    }
}

impl MediaFetcher for TelegramMediaFetcher {
    fn fetch<'a>(&'a self, handle: &MediaHandle) -> BoxedFetchFuture<'a, anyhow::Result<Vec<u8>>> {
        let file_id = handle.0.clone();

        Box::pin(async move {
            let file_path = self.client.get_file(&file_id).await?;
            self.client.download_file(&file_path).await
        })
    }
}

pub struct TelegramRuntime {
    client: Arc<TelegramClient>,
    account_id: String,
    bot: BotMe,
    max_attempts: u32,
}

impl TelegramRuntime {
    pub fn new(
        client: Arc<TelegramClient>,
        account_id: impl Into<String>,
        bot: BotMe,
        max_attempts: u32,
    ) -> Self {
        Self {
            client,
            account_id: account_id.into(),
            bot,
            max_attempts,
        }
    }
}

impl ChannelRuntime for TelegramRuntime {
    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::new(StreamMode::Off, ReplyMode::MessageTool)
    }

    fn sender(&self, target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>> {
        Some(Arc::new(TelegramSender::new(
            self.client.clone(),
            target.target_id(),
            self.max_attempts,
        )))
    }

    fn inbound(&self) -> Option<Arc<dyn InboundDriver>> {
        Some(Arc::new(TelegramPoller::new(
            self.client.clone(),
            self.account_id.clone(),
            &self.bot,
        )))
    }

    fn media_fetcher(&self) -> Option<Arc<dyn MediaFetcher>> {
        Some(Arc::new(TelegramMediaFetcher::new(self.client.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;

    fn bot() -> BotMe {
        BotMe {
            id: 1,
            username: None,
            ..Default::default()
        }
    }

    fn runtime() -> TelegramRuntime {
        TelegramRuntime::new(Arc::new(TelegramClient::new("token")), "default", bot(), 3)
    }

    #[test]
    fn telegram_runtime_streams_off_and_delivers_via_the_message_tool() {
        assert_eq!(
            runtime().capabilities(),
            ChannelCapabilities::new(StreamMode::Off, ReplyMode::MessageTool)
        );
    }

    #[test]
    fn telegram_runtime_has_a_sender_for_a_target() {
        let target = DeliveryTarget::direct(Channel::Telegram, "default", "123456");

        assert!(runtime().sender(&target).is_some());
    }

    #[test]
    fn telegram_runtime_has_an_inbound_driver() {
        assert!(runtime().inbound().is_some());
    }

    #[test]
    fn telegram_runtime_has_a_media_fetcher() {
        assert!(runtime().media_fetcher().is_some());
    }
}
