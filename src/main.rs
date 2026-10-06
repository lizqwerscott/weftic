use std::sync::Arc;

use anyhow::Result;

use weftic::agent_engine::AgentEngine;
use weftic::channel::cli::CliChannel;
use weftic::config::Config;
use weftic::session::manager::SessionManager;
use weftic::session::resolver::SessionResolver;

#[tokio::main]
async fn main() -> Result<()> {
    // tracing_subscriber::fmt()
    //     .with_env_filter(EnvFilter::new("genai=debug"))
    //     .init();

    dotenvy::dotenv()?;

    println!("Load config...");

    let config = Config::load()?;
    config.model_register.print_info();

    let model = config.model_register.resolve()?;

    let engine = Arc::new(AgentEngine::new(model, &config)?);
    engine.init()?;

    let workspace_root = std::env::current_dir()?;

    let channel_templates = config.channel_templates()?;
    let resolver = SessionResolver::new(workspace_root, channel_templates);
    let mut manager = SessionManager::new(engine, resolver);

    CliChannel::new("main").run(&mut manager).await
}
