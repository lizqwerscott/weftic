//! Per-channel runtime: capabilities plus a sender factory.

use std::collections::HashMap;
use std::sync::Arc;

use crate::channel::capabilities::ChannelCapabilities;
use crate::channel::sender::ChannelSender;
use crate::channel::{Channel, DeliveryTarget};

pub trait ChannelRuntime: Send + Sync {
    fn capabilities(&self) -> ChannelCapabilities;
    fn sender(&self, target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>>;
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
