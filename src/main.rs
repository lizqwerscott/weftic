use std::sync::Arc;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

use weftic::agent_engine::AgentEngine;
use weftic::channel::Channel;
use weftic::channel::cli::CliChannel;
use weftic::channel::registry::{ChannelRegistry, CliRuntime};
use weftic::channel::telegram::poll::TelegramPoller;
use weftic::channel::telegram::send::TelegramRuntime;
use weftic::config::{Config, model_provider::ChatModel};
use weftic::event_store::EventStore;
use weftic::output::{NullSink, OutputSink};
use weftic::session::manager::SessionManager;
use weftic::session::resolver::SessionResolver;
use weftic::system_prompt::SystemPromptManager;
use weftic::tools::ToolRouter;

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

    let telegram_token = config.telegram_token()?;

    let mut registry = ChannelRegistry::new();
    registry.register(Channel::Cli, Arc::new(CliRuntime));
    if let Some(token) = &telegram_token {
        registry.register(
            Channel::Telegram,
            Arc::new(TelegramRuntime::new(token.clone())),
        );
    }
    let registry = Arc::new(registry);

    let resolver = SessionResolver::new(
        workspace_root,
        channel_templates,
        registry,
        config.permissions.clone(),
    );
    let mut manager = SessionManager::new(
        engine,
        resolver,
        "main",
        EventStore::open(&config.storage.db_path())?,
    );

    // Telegram is the frontend for now; the CLI scaffold is paused.
    match telegram_token {
        Some(token) => {
            info!(target: "telegram", "long-polling enabled (account `default`)");
            let poller = TelegramPoller::new(token, "default");
            let sink: Arc<dyn OutputSink> = Arc::new(NullSink);
            poller.run(manager, sink).await
        }
        None => {
            info!(target: "telegram", "not configured; falling back to the CLI scaffold");
            CliChannel::new().run(&mut manager).await
        }
    }
}
