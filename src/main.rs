use anyhow::{Result, anyhow};

use genai::{
    Client,
    chat::{
        ChatMessage, ChatOptions, ChatRequest,
        printer::{PrintChatStreamOptions, print_chat_stream},
    },
};
use tracing_subscriber::EnvFilter;

use weftic::agent_engine::AgentEngine;
use weftic::config::{ProviderConfig, load_model_config};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("genai=debug"))
        .init();

    dotenvy::dotenv()?;

    println!("Load Provide config...");

    let model_register = load_model_config("./configs/models.toml")?;
    model_register.print_info();

    if let Some((client, model)) = model_register.get_client_model() {
        let mut agent_engine = AgentEngine::new(client, model, "你是一个 AI 助手".to_string());

        let question = "你好".to_string();

        agent_engine.run_turn(question).await?;
    } else {
        return Err(anyhow!("not find provider and model!"));
    }

    Ok(())
}
