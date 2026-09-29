use std::collections::HashMap;
use std::env;
use std::fmt;
use std::path::Path;

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;

use anyhow::{Context, Result};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Deepseek,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deepseek => write!(f, "Deepseek"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderConfig {
    Native {
        name: String,
        provide: Provider,
        base_url: Option<String>,
        api_key: Option<String>,
    },
    Openai {
        name: String,
        base_url: String,
        api_key: Option<String>,
    },
}

fn inner(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('<')?;
    rest.strip_suffix('>')
}

fn resolve_env(s: &str) -> Result<String> {
    if s.len() >= 2 && s.starts_with('<') && s.ends_with('>') {
        let env_name = inner(s).with_context(|| format!("not get {} inner", s))?;
        env::var(env_name).with_context(|| format!("Not find {} in ENV", env_name))
    } else {
        Ok(String::from(s))
    }
}

impl ProviderConfig {
    fn resolve_env(&mut self) -> Result<()> {
        match self {
            Self::Native {
                base_url, api_key, ..
            } => {
                *base_url = base_url.as_ref().map(|v| resolve_env(v)).transpose()?;

                *api_key = api_key.as_ref().map(|v| resolve_env(v)).transpose()?;
            }
            Self::Openai {
                base_url, api_key, ..
            } => {
                *base_url = resolve_env(base_url)?;

                *api_key = api_key.as_ref().map(|v| resolve_env(v)).transpose()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct ModelRegister {
    pub providers: HashMap<String, ProviderConfig>,
}

pub fn load_model_config(config_path: impl AsRef<Path>) -> Result<ModelRegister> {
    let figment = Figment::new().merge(Toml::file(config_path));
    let mut config: ModelRegister = figment.extract()?;

    for (key, p) in config.providers.iter_mut() {
        p.resolve_env()
            .with_context(|| format!("provider `{}`", key))?;
    }

    Ok(config)
}
