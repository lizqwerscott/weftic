use std::sync::Arc;

use anyhow::Result;
use colored::Colorize;
use rustyline::error::ReadlineError;
use tokio::sync::oneshot;

use crate::channel::{Channel, DeliveryTarget, SessionKey};
use crate::event::{ActorRef, Event};
use crate::output::OutputSink;
use crate::session::manager::{SessionInput, SessionManager};
use crate::tui::cli_sink::CliSink;
use crate::tui::input::build_input;

const CLI_SENDER: &str = "member_cli";

pub struct CliChannel {
    target: DeliveryTarget,
    session_key: SessionKey,
}

impl CliChannel {
    pub fn new(agent_id: &str) -> Self {
        let target = DeliveryTarget::direct(Channel::Cli, "default", "cli");
        let session_key = target.to_session_key(agent_id);
        Self {
            target,
            session_key,
        }
    }

    pub fn session_key(&self) -> &SessionKey {
        &self.session_key
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

        manager
            .submit(
                &self.session_key,
                SessionInput::Turn {
                    events: vec![self.event(text)],
                    sink,
                    reply,
                },
            )
            .await?;

        match reply_rx.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => println!("{}: {}", "Error".red(), err),
            Err(_) => println!("{}: session task ended", "Error".red()),
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventOrigin, Part, PlatformKind};

    #[test]
    fn session_key_is_the_canonical_cli_key() {
        let channel = CliChannel::new("main");
        assert_eq!(
            channel.session_key().as_str(),
            "agent:main:cli:default:direct:cli"
        );
    }

    #[test]
    fn event_is_a_cli_platform_text_message() {
        let channel = CliChannel::new("main");
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
