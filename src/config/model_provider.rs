use std::collections::HashMap;
use std::env;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use colored::Colorize;
use figment::{
    Figment,
    providers::{Format, Toml},
};
use futures::Stream;
use genai::Client;
use genai::ModelIden;
use genai::ServiceTarget;
use genai::adapter::AdapterKind;
use genai::chat::{ChatOptions, ChatRequest, ChatStreamEvent, ReasoningEffort};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
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
    reasoning_effort: Option<String>,
}

pub struct ResolvedModel {
    client: Client,
    model_id: String,
    options: ChatOptions,
}

pub type BoxedModelFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub type ChatEventStream<'a> =
    Pin<Box<dyn Stream<Item = genai::Result<ChatStreamEvent>> + Send + 'a>>;

pub trait ChatModel: Send + Sync {
    fn stream_chat<'a>(
        &'a self,
        request: ChatRequest,
    ) -> BoxedModelFuture<'a, Result<ChatEventStream<'a>>>;
}

impl ChatModel for ResolvedModel {
    fn stream_chat<'a>(
        &'a self,
        request: ChatRequest,
    ) -> BoxedModelFuture<'a, Result<ChatEventStream<'a>>> {
        Box::pin(async move {
            let options = self
                .options
                .clone()
                .with_capture_tool_calls(true)
                .with_capture_usage(true)
                .with_capture_reasoning_content(true)
                .with_capture_content(true);

            let response = self
                .client
                .exec_chat_stream(self.model_id.as_str(), request, Some(&options))
                .await?;

            let stream: ChatEventStream<'a> = Box::pin(response.stream);

            Ok(stream)
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderConfig {
    Native {
        name: String,
        provider: Provider,
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
    pub fn resolve(&self) -> Result<ResolvedModel> {
        let (provider_name, model_name) = self.route.main.split_once('/').with_context(|| {
            format!(
                "malformed route.main `{}` (expected `provider/model`)",
                self.route.main
            )
        })?;

        let provider = self
            .providers
            .get(provider_name)
            .with_context(|| format!("unknown provider `{provider_name}` in route.main"))?;

        let model = provider.find_model(model_name).with_context(|| {
            format!("unknown model `{model_name}` in provider `{provider_name}`")
        })?;

        let client = provider.build_client()?;

        let mut options = ChatOptions::default();

        if let Some(temperature) = model.temperature {
            options = options.with_temperature(temperature);
        }

        if let Some(max_tokens) = model.max_tokens {
            options = options.with_max_tokens(max_tokens);
        }

        if let Some(effort) = model.reasoning_effort.as_deref() {
            let effort = ReasoningEffort::from_keyword(effort)
                .with_context(|| format!("unknown reasoning_effort `{effort}`"))?;
            options = options.with_reasoning_effort(effort);
        }

        Ok(ResolvedModel {
            client,
            model_id: model.model.clone(),
            options,
        })
    }

    pub fn print_info(&self) {
        let parts: Vec<&str> = self.route.main.split('/').collect();
        let activate_provider_name = parts.first();
        let activate_model = parts.get(1);

        println!();
        println!("{}", "Providers".bright_white().bold());

        for (k, p) in self.providers.iter() {
            let active = if let Some(name) = activate_provider_name {
                k == name
            } else {
                false
            };

            let bullet = if active {
                "▸".green().bold().to_string()
            } else {
                "•".bright_black().to_string()
            };

            match p {
                ProviderConfig::Native {
                    name,
                    provider,
                    models,
                } => {
                    println!(
                        "  {} {}({}) from {} native",
                        bullet,
                        name.bright_white().bold(),
                        k,
                        provider.to_string().bright_white()
                    );

                    print!("    {}: ", "models".bright_black());

                    let mut first_model = true;

                    for model in models.iter() {
                        let model_active = if let Some(a_name) = activate_model {
                            *a_name == model.name
                        } else {
                            false
                        };

                        if first_model {
                            first_model = false;
                        } else {
                            print!(", ")
                        }

                        if model_active {
                            print!("{}", model.name.green());
                        } else {
                            print!("{}", model.name.yellow());
                        }
                    }

                    println!();
                }
                ProviderConfig::Openai {
                    name,
                    base_url,
                    models,
                    ..
                } => {
                    println!(
                        "  {} {}({})",
                        bullet,
                        name.bright_white().bold(),
                        k.bright_white()
                    );

                    println!("    {}: {}", "Base url".bright_black(), base_url.cyan());

                    print!("    {}: ", "models".bright_black());
                    let mut first_model = true;

                    for model in models.iter() {
                        let model_active = if let Some(a_name) = activate_model {
                            *a_name == model.name
                        } else {
                            false
                        };

                        if first_model {
                            first_model = false;
                        } else {
                            print!(", ")
                        }

                        if model_active {
                            print!("{}", model.name.green());
                        } else {
                            print!("{}", model.name.yellow());
                        }
                    }
                    println!();
                }
            }
        }
        println!();
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

#[cfg(test)]
pub mod testing {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use genai::chat::{MessageContent, StopReason, StreamChunk, StreamEnd, ToolCall, Usage};

    use super::*;

    #[derive(Default)]
    pub struct ScriptedModel {
        replies: Mutex<VecDeque<Vec<ChatStreamEvent>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl ScriptedModel {
        pub fn new(replies: Vec<Vec<ChatStreamEvent>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        pub fn text_reply(text: &str) -> Arc<Self> {
            Self::new(vec![text_events(text)])
        }

        pub fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    pub fn text_events(text: &str) -> Vec<ChatStreamEvent> {
        vec![
            ChatStreamEvent::Chunk(StreamChunk {
                content: text.to_string(),
            }),
            ChatStreamEvent::End(text_end(text)),
        ]
    }

    pub fn text_end(text: &str) -> StreamEnd {
        StreamEnd {
            captured_usage: Some(Usage {
                prompt_tokens: Some(1),
                completion_tokens: Some(1),
                total_tokens: Some(2),
                ..Default::default()
            }),
            captured_stop_reason: Some(StopReason::Completed("stop".to_string())),
            captured_content: Some(MessageContent::from_text(text)),
            captured_reasoning_content: None,
            captured_response_id: None,
        }
    }

    pub fn tool_call_events(
        call_id: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> Vec<ChatStreamEvent> {
        vec![ChatStreamEvent::End(StreamEnd {
            captured_usage: Some(Usage {
                prompt_tokens: Some(1),
                completion_tokens: Some(1),
                total_tokens: Some(2),
                ..Default::default()
            }),
            captured_stop_reason: Some(StopReason::ToolCall("tool_use".to_string())),
            captured_content: Some(MessageContent::from_tool_calls(vec![ToolCall {
                call_id: call_id.to_string(),
                fn_name: name.to_string(),
                fn_arguments: arguments,
                thought_signatures: None,
            }])),
            captured_reasoning_content: None,
            captured_response_id: None,
        })]
    }

    impl ChatModel for ScriptedModel {
        fn stream_chat<'a>(
            &'a self,
            request: ChatRequest,
        ) -> BoxedModelFuture<'a, Result<ChatEventStream<'a>>> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request);

                let events = self.replies.lock().unwrap().pop_front().unwrap_or_default();
                let stream: ChatEventStream<'a> =
                    Box::pin(futures::stream::iter(events.into_iter().map(Ok)));

                Ok(stream)
            })
        }
    }
}
