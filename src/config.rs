use std::collections::HashMap;
use std::env;
use std::fmt;
use std::path::Path;

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use genai::Client;
use genai::ModelIden;
use genai::ServiceTarget;
use genai::adapter::AdapterKind;
use genai::resolver::AuthData;
use genai::resolver::Endpoint;
use genai::resolver::ServiceTargetResolver;
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
pub struct Model {
    pub name: String,
    model: String,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    reasoning_effot: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderConfig {
    Native {
        name: String,
        provide: Provider,
        models: Vec<Model>,
    },
    Openai {
        name: String,
        base_url: String,
        api_key: Option<String>,
        models: Vec<Model>,
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
            Self::Native { .. } => {}
            Self::Openai {
                base_url, api_key, ..
            } => {
                *base_url = resolve_env(base_url)?;

                *api_key = api_key.as_ref().map(|v| resolve_env(v)).transpose()?;
            }
        }
        Ok(())
    }

    fn find_model(&self, model_name: &str) -> Option<&Model> {
        let models = match self {
            Self::Native { models, .. } => models,
            Self::Openai { models, .. } => models,
        };

        models.iter().find(|model| model.name == model_name)
    }

    fn build_client(&self) -> Result<Client> {
        match self {
            Self::Native { .. } => Ok(Client::default()),
            Self::Openai {
                base_url, api_key, ..
            } => {
                let base_url = base_url.clone();
                let api_key = api_key.clone();

                let target_resolver = ServiceTargetResolver::from_resolver_fn(
                    move |service_target: ServiceTarget| -> std::result::Result<ServiceTarget, genai::resolver::Error> {
                        let ServiceTarget { model, .. } = service_target;
			            let endpoint = Endpoint::from_owned(base_url.clone());
                        let auth = if let Some(key) = api_key {
                            AuthData::Key(key.clone())
                        } else {
                            AuthData::None
                        };
			            let model = ModelIden::new(AdapterKind::OpenAI, model.model_name);
			            // TODO: point to xai
			            Ok(ServiceTarget { endpoint, auth, model })
                    },
                );

                Ok(Client::builder()
                    .with_service_target_resolver(target_resolver)
                    .build())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ModelRoute {
    main: String,
}

#[derive(Debug, Deserialize)]
pub struct ModelRegister {
    pub providers: HashMap<String, ProviderConfig>,
    route: ModelRoute,
}

impl ModelRegister {
    pub fn get_client_model(&self) -> Option<(Client, String)> {
        let parts: Vec<&str> = self.route.main.split('/').collect();
        let provider_name = parts.first()?;
        let model = parts.get(1)?;

        let provider = self.providers.get(*provider_name)?;
        let model = provider.find_model(model)?;

        let client = provider.build_client().ok()?;

        Some((client, model.model.clone()))
    }

    pub fn print_info(&self) {
        println!("Route: {}", self.route.main);

        println!("==================");

        for (k, p) in self.providers.iter() {
            match p {
                ProviderConfig::Native {
                    name,
                    provide,
                    models,
                } => {
                    println!("{}({}) from {} native:", name, k, provide);

                    for model in models.iter() {
                        println!("Model: {}", model.name);
                    }
                }
                ProviderConfig::Openai {
                    name,
                    base_url,
                    api_key,
                    models,
                } => {
                    println!("{}({}):", name, k);
                    println!("Base url: {}", base_url);

                    for model in models.iter() {
                        println!("Model: {}", model.name);
                    }
                }
            }
            println!("==================");
        }
    }
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
