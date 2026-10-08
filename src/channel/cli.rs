use std::sync::Arc;

use anyhow::Result;
use colored::Colorize;
use rustyline::error::ReadlineError;
use tokio::sync::oneshot;

use crate::channel::capabilities::{ChannelCapabilities, ReplyMode, StreamMode};
use crate::channel::registry::ChannelRuntime;
use crate::channel::sender::ChannelSender;
use crate::channel::{Channel, DeliveryTarget};
use crate::event::{ActorRef, Event};
use crate::output::OutputSink;
use crate::session::manager::{IngestOutcome, SessionManager};
use crate::tui::cli_sink::CliSink;
use crate::tui::input::build_input;

const CLI_SENDER: &str = "member_cli";

pub struct CliChannel {
    target: DeliveryTarget,
}

impl CliChannel {
    pub fn new() -> Self {
        Self {
            target: DeliveryTarget::direct(Channel::Cli, "default", "cli"),
        }
    }

    pub fn event(&self, text: impl Into<String>) -> Event {
        Event::platform_text(self.target.clone(), ActorRef::new(CLI_SENDER), text)
    }

    pub async fn run(&self, manager: &mut SessionManager) -> Result<()> {
        let (_, mut rl) = build_input()?;

        loop {
            match rl.readline("> ") {
                Ok(line) => {
                    if line == "/exit" {
                        break;
                    }
                    self.submit_turn(manager, line).await?;
                }
                Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                    println!("Exit");
                    break;
                }
                Err(err) => {
                    println!("Read error: {}", err);
                }
            }
        }

        Ok(())
    }

    async fn submit_turn(&self, manager: &mut SessionManager, text: String) -> Result<()> {
        let (reply, reply_rx) = oneshot::channel();
        let sink: Arc<dyn OutputSink> = Arc::new(CliSink::new());

        if let IngestOutcome::Turn { .. } = manager.ingest(self.event(text), sink, reply).await? {
            match reply_rx.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => println!("{}: {}", "Error".red(), err),
                Err(_) => println!("{}: session task ended", "Error".red()),
            }
        }

        Ok(())
    }
}

impl Default for CliChannel {
    fn default() -> Self {
        Self::new()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::registry::ChannelRegistry;
    use crate::event::{EventOrigin, Part, PlatformKind};

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
    fn a_registered_cli_runtime_can_be_looked_up() {
        let mut registry = ChannelRegistry::new();
        registry.register(Channel::Cli, Arc::new(CliRuntime));

        let runtime = registry.runtime(Channel::Cli).unwrap();

        assert_eq!(runtime.capabilities().reply, ReplyMode::Automatic);
    }

    #[test]
    fn event_is_a_cli_platform_text_message() {
        let channel = CliChannel::new();
        let event = channel.event("hi");

        let EventOrigin::Platform(platform) = event.origin else {
            panic!("CLI input must be a platform event");
        };

        assert_eq!(platform.kind, PlatformKind::Message);
        assert_eq!(
            platform.target,
            DeliveryTarget::direct(Channel::Cli, "default", "cli")
        );
        assert_eq!(
            platform.parts,
            vec![Part::Text {
                text: "hi".to_string(),
                entities: vec![],
            }]
        );
    }
}
