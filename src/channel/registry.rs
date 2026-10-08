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
    /// Drive this channel's inbound loop. Taking `self: Arc<Self>` lets the
    /// returned future be `'static`, so each driver can run on its own task.
    fn run(
        self: Arc<Self>,
        manager: Arc<SessionManager>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
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
pub mod testing {
    use super::*;
    use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
    use crate::channel::sender::ChannelSender;

    /// A stand-in channel runtime for tests: the second implementation that
    /// keeps the `ChannelRuntime` abstraction honest now that the CLI is gone.
    pub struct MockRuntime {
        capabilities: ChannelCapabilities,
        sender: Option<Arc<dyn ChannelSender>>,
    }

    impl MockRuntime {
        /// The `Automatic` + `InPlace` profile with no outbound sender — what a
        /// local/live channel (WebUI) will look like.
        pub fn live() -> Arc<Self> {
            Arc::new(Self {
                capabilities: ChannelCapabilities::new(StreamMode::InPlace, ReplyMode::Automatic),
                sender: None,
            })
        }

        /// A `MessageTool` profile that delivers through the given sender.
        pub fn outbound(sender: Arc<dyn ChannelSender>) -> Arc<Self> {
            Arc::new(Self {
                capabilities: ChannelCapabilities::new(StreamMode::Off, ReplyMode::MessageTool),
                sender: Some(sender),
            })
        }
    }

    impl ChannelRuntime for MockRuntime {
        fn capabilities(&self) -> ChannelCapabilities {
            self.capabilities
        }

        fn sender(&self, _target: &DeliveryTarget) -> Option<Arc<dyn ChannelSender>> {
            self.sender.clone()
        }
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
