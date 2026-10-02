use rustyline::error::ReadlineError;

use anyhow::{Result, anyhow};
use colored::Colorize;
use tracing_subscriber::EnvFilter;

use weftic::agent_engine::AgentEngine;
use weftic::config::load_model_config;
use weftic::tui::input::build_input;

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
        agent_engine.register_builtin_tools()?;

        let (_, mut rl) = build_input()?;

        loop {
            let readline = rl.readline("> ");
            match readline {
                Ok(line) => {
                    if line == "/exit" {
                        break;
                    }

                    if let Err(err) = agent_engine.run_turn(line.to_string()).await {
                        println!("{}: {}", "Error".red(), err.to_string());
                    }
                }
                Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                    println!("Exit");
                    break;
                }
                Err(err) => {
                    println!("Read error: {}", err.to_string());
                }
            }
        }
    } else {
        return Err(anyhow!("no provider and model found!"));
    }

    Ok(())
}
