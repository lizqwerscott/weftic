use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use weftic::agent_engine::AgentEngine;
use weftic::channel::telegram::poll::TelegramPoller;
use weftic::config::{Config, model_provider::ChatModel};
use weftic::event_store::EventStore;
use weftic::output::{NullSink, OutputSink};
use weftic::session::manager::SessionManager;
use weftic::session::resolver::SessionResolver;
use weftic::system_prompt::SystemPromptManager;
use weftic::tools::ToolRouter;
use weftic::{ChannelSetup, setup_channels};

/// Log to stderr, defaulting to `info`; override with `RUST_LOG`.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv()?;
    init_logging();

    info!("loading config");

    let config = Config::load()?;
    config.model_register.print_info();

    let model: Arc<dyn ChatModel> = Arc::new(config.model_register.resolve()?);

    let mut tool_router = ToolRouter::new();
    tool_router.register_builtin_tools()?;

    let channel_templates = config.channel_templates()?;

    let system_prompt_manager =
        SystemPromptManager::new(&config.system_prompt, channel_templates.values().cloned())?;

    let engine = Arc::new(AgentEngine::new(
        model,
        tool_router,
        system_prompt_manager,
        config.engine.max_iterations,
    ));
    engine.init()?;

    let workspace_root = std::env::current_dir()?;

    let ChannelSetup {
        registry,
        telegram_token,
    } = setup_channels(&config)?;

    if registry.is_empty() {
        warn!(
            "no channel is running; nothing to do (enable one with `enabled = true` under [channels.*])"
        );
        return Ok(());
    }

    let resolver = SessionResolver::new(
        workspace_root,
        channel_templates,
        registry,
        config.permissions.clone(),
    );
    let manager = SessionManager::new(
        engine,
        resolver,
        "main",
        EventStore::open(&config.storage.db_path())?,
    );

    if let Some(token) = telegram_token {
        info!(target: "telegram", "long-polling enabled (account `default`)");
        let poller = TelegramPoller::new(token, "default");
        let sink: Arc<dyn OutputSink> = Arc::new(NullSink);
        poller.run(manager, sink).await?;
    }

    Ok(())
}
