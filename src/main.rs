use weftic::config::{ProviderConfig, load_model_config};

use anyhow::Result;

fn main() -> Result<()> {
    dotenvy::dotenv()?;

    println!("Load Provide config...");

    let models = load_model_config("./configs/models.toml")?;

    println!("==================");

    for (k, p) in models.providers.iter() {
        match p {
            ProviderConfig::Native {
                name,
                provide,
                base_url,
                api_key,
            } => {
                println!("{}({}) from {} native:", name, k, provide);
                if let Some(base_url) = base_url {
                    println!("Base url: {}", base_url);
                }
            }
            ProviderConfig::Openai {
                name,
                base_url,
                api_key,
            } => {
                println!("{}({}):", name, k);
                println!("Base url: {}", base_url);
            }
        }
    }
    println!("==================");


    Ok(())
}
