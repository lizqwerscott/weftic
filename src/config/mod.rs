use std::{collections::HashMap, path::PathBuf};

use anyhow::{Result, anyhow};
use figment::{
    Figment,
    providers::{Format, Toml},
};

use crate::access::AccessPolicy;
use crate::config::{
    channel::ChannelConfig,
    engine::EngineConfig,
    model_provider::{ModelRegister, load_model_config},
    storage::StorageConfig,
    system_prompt::SystemPromptConfig,
};
use crate::permissions::Permissions;
use crate::{channel::Channel, config::workspace::WorkspaceConfig};

pub mod channel;
pub mod engine;
pub mod model_provider;
pub mod storage;
pub mod system_prompt;
pub mod workspace;

pub struct Config {
    pub model_register: ModelRegister,
    pub system_prompt: SystemPromptConfig,
    pub channels: HashMap<String, ChannelConfig>,
    pub engine: EngineConfig,
    pub storage: StorageConfig,
    pub permissions: Permissions,
    pub access: AccessPolicy,
    pub workspace: WorkspaceConfig,
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

        let engine = if app_config.contains("engine") {
            app_config.extract_inner("engine")?
        } else {
            EngineConfig::default()
        };
        engine.validate()?;

        let storage = if app_config.contains("storage") {
            app_config.extract_inner("storage")?
        } else {
            StorageConfig::default()
        };

        let permissions = if app_config.contains("permissions") {
            app_config.extract_inner("permissions")?
        } else {
            Permissions::default()
        };

        let access = if app_config.contains("access") {
            app_config.extract_inner("access")?
        } else {
            AccessPolicy::default()
        };

        Ok(Self {
            model_register,
            system_prompt: app_config.extract_inner("system_prompt")?,
            channels: app_config.extract_inner("channels")?,
            engine,
            storage,
            permissions,
            access,
            workspace: app_config.extract_inner("workspace")?,
        })
    }

    pub fn channel_templates(&self) -> Result<HashMap<Channel, String>> {
        self.channels
            .iter()
            .map(|(name, config)| {
                let channel = Channel::parse(name)
                    .ok_or_else(|| anyhow!("unknown channel `{name}` in [channels]"))?;
                Ok((channel, config.template.clone()))
            })
            .collect()
    }

    pub fn is_channel_enabled(&self, name: &str) -> bool {
        let Some(channel) = self.channels.get(name) else {
            return false;
        };

        channel.enabled
    }

    pub fn channel_token(&self, name: &str) -> Result<Option<String>> {
        let Some(channel) = self.channels.get(name) else {
            return Ok(None);
        };

        match &channel.token {
            Some(token) => Ok(Some(resolve_env(token)?)),
            None => Ok(None),
        }
    }
}

fn resolve_env(value: &str) -> Result<String> {
    match value
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        Some(name) => {
            std::env::var(name).map_err(|_| anyhow!("environment variable `{name}` is not set"))
        }
        None => Ok(value.to_string()),
    }
}
