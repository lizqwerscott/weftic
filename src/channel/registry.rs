//! Per-channel runtime: capabilities plus a sender factory.

use std::collections::HashMap;
use std::sync::Arc;

use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
use crate::channel::sender::ChannelSender;
use crate::channel::{Channel, DeliveryTarget};

pub trait ChannelRuntime: Send + Sync {
    fn capabilities(&self) -> ChannelCapabilities;
    fn sender(&self, target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>>;
}

pub struct CliRuntime;

impl ChannelRuntime for CliRuntime {
    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::new(StreamMode::InPlace, ReplyMode::Automatic)
    }

    fn sender(&self, _target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>> {
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn cli_target() -> DeliveryTarget {
        DeliveryTarget::direct(Channel::Cli, "default", "cli")
    }

    #[test]
    fn cli_runtime_streams_in_place_and_has_no_sender() {
        let runtime = CliRuntime;

        assert_eq!(
            runtime.capabilities(),
            ChannelCapabilities::new(StreamMode::InPlace, ReplyMode::Automatic)
        );
        assert!(runtime.sender(&cli_target()).is_none());
    }

    #[test]
    fn a_registered_runtime_can_be_looked_up() {
        let mut registry = ChannelRegistry::new();
        registry.register(Channel::Cli, Arc::new(CliRuntime));

        let runtime = registry.runtime(Channel::Cli).unwrap();

        assert_eq!(runtime.capabilities().reply, ReplyMode::Automatic);
    }

    #[test]
    fn an_unregistered_channel_has_no_runtime() {
        let registry = ChannelRegistry::new();

        assert!(registry.runtime(Channel::Telegram).is_none());
    }
}
