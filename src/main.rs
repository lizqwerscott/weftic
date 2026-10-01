use std::io::{self, Write};

use anyhow::{Result, anyhow};
use colored::Colorize;

use tracing_subscriber::EnvFilter;

use weftic::agent_engine::AgentEngine;
use weftic::config::load_model_config;

#[tokio::main]
async fn main() -> Result<()> {
    // tracing_subscriber::fmt()
    //     .with_env_filter(EnvFilter::new("genai=debug"))
    //     .init();

    dotenvy::dotenv()?;

    println!("Load Provide config...");

    let model_register = load_model_config("./configs/models.toml")?;
    model_register.print_info();

    if let Some((client, model)) = model_register.get_client_model() {
        let mut agent_engine = AgentEngine::new(client, model, "你是一个 AI 助手".to_string());
        agent_engine.register_buildin_tools()?;

        loop {
            let mut input = String::new();
            print!("> ");
            io::stdout().flush()?;
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input == "/exit" {
                break;
            }

            if let Err(err) = agent_engine.run_turn(input.to_string()).await {
                println!("{}: {}", "Error".red(), err.to_string());
            }
        }
    } else {
        return Err(anyhow!("not find provider and model!"));
    }

    Ok(())
}
