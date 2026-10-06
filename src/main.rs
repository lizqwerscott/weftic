use std::sync::Arc;

use anyhow::Result;

use weftic::agent_engine::AgentEngine;
use weftic::channel::cli::CliChannel;
use weftic::config::{Config, model_provider::ChatModel};
use weftic::session::manager::SessionManager;
use weftic::session::resolver::SessionResolver;
use weftic::system_prompt::SystemPromptManager;
use weftic::tools::ToolRouter;

#[tokio::main]
async fn main() -> Result<()> {
    // tracing_subscriber::fmt()
    //     .with_env_filter(EnvFilter::new("genai=debug"))
    //     .init();

    dotenvy::dotenv()?;

    println!("Load config...");

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
        100,
    ));
    engine.init()?;

    let workspace_root = std::env::current_dir()?;

    let resolver = SessionResolver::new(workspace_root, channel_templates);
    let mut manager = SessionManager::new(engine, resolver);

    CliChannel::new("main").run(&mut manager).await
}
