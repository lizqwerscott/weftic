//! Per-channel runtime: capabilities, sender factory, and inbound driver.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;

use crate::channel::capabilities::ChannelCapabilities;
use crate::channel::sender::ChannelSender;
use crate::channel::{Channel, DeliveryTarget};
use crate::session::manager::SessionManager;

pub trait InboundDriver: Send + Sync {
    fn run<'a>(
        &'a self,
        manager: &'a mut SessionManager,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

pub trait ChannelRuntime: Send + Sync {
    fn capabilities(&self) -> ChannelCapabilities;
    fn sender(&self, target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>>;

    /// The inbound loop this channel wants driven, if it consumes messages.
    fn inbound(&self) -> Option<Arc<dyn InboundDriver>> {
        None
    }
}

#[derive(Default)]
pub struct ChannelRegistry {
    runtimes: HashMap<Channel, Arc<dyn ChannelRuntime>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, channel: Channel, runtime: Arc<dyn ChannelRuntime>) {
        self.runtimes.insert(channel, runtime);
    }

    pub fn runtime(&self, channel: Channel) -> Option<Arc<dyn ChannelRuntime>> {
        self.runtimes.get(&channel).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.runtimes.is_empty()
    }

    pub fn inbound_drivers(&self) -> Vec<Arc<dyn InboundDriver>> {
        self.runtimes
            .values()
            .filter_map(|runtime| runtime.inbound())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unregistered_channel_has_no_runtime() {
        let registry = ChannelRegistry::new();

        assert!(registry.runtime(Channel::Telegram).is_none());
    }
}
