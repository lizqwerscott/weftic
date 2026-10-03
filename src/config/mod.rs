use std::path::PathBuf;

use anyhow::{Result, anyhow};
use figment::{
    Figment,
    providers::{Format, Toml},
};

use crate::config::{
    model_provider::{ModelRegister, load_model_config},
    system_prompt::SystemPromptConfig,
};

pub mod model_provider;
pub mod system_prompt;

pub struct Config {
    pub model_register: ModelRegister,
    pub system_prompt: SystemPromptConfig,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = PathBuf::from("./configs/config.toml");
        let models_path = PathBuf::from("./configs/models.toml");

        if !config_path.exists() {
            return Err(anyhow!("{} does not exist", config_path.display()));
        }

        if !models_path.exists() {
            return Err(anyhow!("{} does not exist", models_path.display()));
        }

        let model_register = load_model_config(models_path)?;

        let app_config = Figment::new().merge(Toml::file(config_path));

        Ok(Self {
            model_register,
            system_prompt: app_config.extract_inner("system_prompt")?,
        })
    }
}
