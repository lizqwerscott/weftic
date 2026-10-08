//! The telegram channel runtime: registers telegram with the channel registry
//! and ties the outbound sender and inbound poller to one shared client.

use std::sync::Arc;

use crate::channel::DeliveryTarget;
use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
use crate::channel::registry::{ChannelRuntime, InboundDriver};
use crate::channel::sender::ChannelSender;
use crate::channel::telegram::client::TelegramClient;
use crate::channel::telegram::poll::TelegramPoller;
use crate::channel::telegram::send::TelegramSender;

pub struct TelegramRuntime {
    client: Arc<TelegramClient>,
    account_id: String,
    max_attempts: u32,
}

impl TelegramRuntime {
    pub fn new(
        client: Arc<TelegramClient>,
        account_id: impl Into<String>,
        max_attempts: u32,
    ) -> Self {
        Self {
            client,
            account_id: account_id.into(),
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
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;

    fn runtime() -> TelegramRuntime {
        TelegramRuntime::new(Arc::new(TelegramClient::new("token")), "default", 3)
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
}
