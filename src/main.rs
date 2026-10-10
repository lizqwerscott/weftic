use std::sync::Arc;

use anyhow::Result;
use tokio::task::JoinSet;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use weftic::agent_engine::AgentEngine;
use weftic::config::{Config, model_provider::ChatModel};
use weftic::database::Database;
use weftic::registry_channels;
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

    let registry = registry_channels(&config).await?;

    if registry.is_empty() {
        warn!(
            "no channel is running; nothing to do (enable one with `enabled = true` under [channels.*])"
        );
        return Ok(());
    }

    let resolver = SessionResolver::new(
        config.workspace.base.clone(),
        channel_templates,
        registry.clone(),
        config.permissions.clone(),
        config.access.clone(),
    );
    let database = Arc::new(Database::new(config.storage.db_path()).await?);
    let manager = Arc::new(SessionManager::new(
        engine,
        resolver,
        "main",
        database.clone(),
    ));

    // Each channel's inbound loop gets its own task, so one slow channel can't
    // stall the others.
    let mut drivers = JoinSet::new();
    for driver in registry.inbound_drivers() {
        let manager = manager.clone();
        drivers.spawn(async move { driver.run(manager).await });
    }

    let outcome = tokio::select! {
        // Fail fast: a channel driver stopping on its own is not a normal exit
        // (the gateway is meant to run forever), so it fails the whole process.
        result = async {
            match drivers.join_next().await {
                Some(Ok(Ok(()))) => Err(anyhow::anyhow!("a channel driver exited unexpectedly")),
                Some(Ok(Err(error))) => Err(error),
                Some(Err(join_error)) => Err(anyhow::Error::new(join_error)),
                None => Ok(()),
            }
        } => result,
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received; stopping channels");
            Ok::<(), anyhow::Error>(())
        }
    };

    info!("stopping channels");
    drivers.shutdown().await;
    info!("draining in-flight turns");
    manager.shutdown().await;
    info!("closing database");
    database.close();

    outcome
}
