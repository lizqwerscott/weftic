use std::collections::HashMap;
use std::env;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

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
use reqwest::StatusCode;
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
    /// Whether the model accepts image input; absent = false (fail-closed, §11.5).
    #[serde(default)]
    vision: bool,
}

pub struct ResolvedModel {
    client: Client,
    model_id: String,
    options: ChatOptions,
    vision: bool,
}

pub type BoxedModelFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub type ChatEventStream<'a> =
    Pin<Box<dyn Stream<Item = genai::Result<ChatStreamEvent>> + Send + 'a>>;

pub trait ChatModel: Send + Sync {
    fn stream_chat<'a>(
        &'a self,
        request: ChatRequest,
    ) -> BoxedModelFuture<'a, Result<ChatEventStream<'a>>>;

    /// Whether this model accepts image input (§11.5). Fail-closed: default no.
    fn vision(&self) -> bool {
        false
    }
}

impl ChatModel for ResolvedModel {
    fn vision(&self) -> bool {
        self.vision
    }

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
        /// Optional probe URL for the startup health check; falls back to
        /// `{base_url}/models`. Point it at e.g. `/health` when the endpoint is
        /// not OpenAI-compatible, or at another host/port.
        #[serde(default)]
        health_url: Option<String>,
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
                base_url,
                api_key,
                health_url,
                ..
            } => {
                *base_url = resolve_env(base_url)?;

                *api_key = api_key.as_ref().map(|v| resolve_env(v)).transpose()?;

                *health_url = health_url.as_ref().map(|v| resolve_env(v)).transpose()?;
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
            vision: model.vision,
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

    /// The provider the active route points at, if it exists.
    fn active_provider(&self) -> Option<&ProviderConfig> {
        let (provider_name, _) = self.route.main.split_once('/')?;
        self.providers.get(provider_name)
    }

    /// The provider-side model id the active route resolves to, if any.
    fn active_model_id(&self) -> Option<&str> {
        let (provider_name, model_name) = self.route.main.split_once('/')?;
        let model = self.providers.get(provider_name)?.find_model(model_name)?;
        Some(model.model.as_str())
    }

    /// Probe the active provider's `GET /models` endpoint.
    ///
    /// Providers built on a built-in adapter (`Native`) expose no custom
    /// endpoint, so there is nothing to probe. A definitive rejection (bad auth,
    /// bad request, unknown endpoint) aborts startup; transient failures
    /// (connection, timeout, 429, 5xx) are retried and, if they persist, only
    /// warn — the endpoint may recover once the gateway is running.
    pub async fn check_health(&self) -> Result<()> {
        let Some(ProviderConfig::Openai {
            base_url,
            api_key,
            health_url,
            ..
        }) = self.active_provider()
        else {
            return Ok(());
        };

        let url = health_endpoint(base_url, health_url.as_deref());
        let http = reqwest::Client::new();
        let mut attempt = 1;

        loop {
            match probe_models(&http, &url, api_key.as_deref()).await {
                Ok(ids) => {
                    tracing::info!("model health check passed ({url})");
                    report_models(&ids, self.active_model_id());
                    return Ok(());
                }
                Err(ProbeFailure::Definitive(error)) => return Err(error),
                Err(ProbeFailure::Transient(error)) => {
                    if attempt >= HEALTH_ATTEMPTS {
                        tracing::warn!(
                            "model health check still failing after {HEALTH_ATTEMPTS} attempts: {error:#}"
                        );
                        return Ok(());
                    }
                    attempt += 1;
                    tokio::time::sleep(HEALTH_BACKOFF).await;
                }
            }
        }
    }
}

const HEALTH_ATTEMPTS: u32 = 3;
const HEALTH_BACKOFF: Duration = Duration::from_millis(750);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);

/// A failed health probe, split by whether retrying could help.
enum ProbeFailure {
    /// The endpoint answered and refused us; retrying will not help.
    Definitive(anyhow::Error),
    /// The endpoint was unreachable or overloaded; worth retrying.
    Transient(anyhow::Error),
}

fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// Where the startup probe points: an explicit `health_url` when configured,
/// otherwise the OpenAI-style `{base_url}/models`.
fn health_endpoint(base_url: &str, health_url: Option<&str>) -> String {
    match health_url {
        Some(url) => url.to_string(),
        None => models_url(base_url),
    }
}

async fn probe_models(
    http: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
) -> std::result::Result<Vec<String>, ProbeFailure> {
    let mut request = http.get(url).timeout(HEALTH_TIMEOUT);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }

    let response = request.send().await.map_err(|error| {
        ProbeFailure::Transient(anyhow::anyhow!("calling {url}: {}", error.without_url()))
    })?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if status.is_success() {
        return Ok(model_ids(&body));
    }

    let error = anyhow::anyhow!("{url} returned {status}: {body}");

    if retryable(status) {
        Err(ProbeFailure::Transient(error))
    } else {
        Err(ProbeFailure::Definitive(error))
    }
}

/// Rate limits and server errors are worth retrying; other 4xx are not.
fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// The model ids in an OpenAI-style `{ "data": [ { "id": … } ] }` response.
/// An unexpected body yields an empty list rather than an error.
fn model_ids(body: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("data")?.as_array().cloned())
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    model
                        .get("id")
                        .and_then(|id| id.as_str())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Log the provider's model list (truncated) and warn when the active model is
/// not among them.
fn report_models(ids: &[String], active: Option<&str>) {
    if ids.is_empty() {
        return;
    }

    const MAX_SHOWN: usize = 20;
    let shown = ids
        .iter()
        .take(MAX_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = ids.len().saturating_sub(MAX_SHOWN);

    if more == 0 {
        tracing::info!("provider exposes {} model(s): {}", ids.len(), shown);
    } else {
        tracing::info!(
            "provider exposes {} model(s): {}, … (+{more})",
            ids.len(),
            shown
        );
    }

    if let Some(active) = active
        && !ids.iter().any(|id| id == active)
    {
        tracing::warn!("the active model `{active}` is not in the provider's model list");
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
        vision: bool,
    }

    impl ScriptedModel {
        pub fn new(replies: Vec<Vec<ChatStreamEvent>>) -> Arc<Self> {
            Self::with_vision(replies, false)
        }

        /// A model that reports the given image-input capability (§11.5).
        pub fn with_vision(replies: Vec<Vec<ChatStreamEvent>>, vision: bool) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
                vision,
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
        fn vision(&self) -> bool {
            self.vision
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_defaults_to_false_and_is_read_from_config() {
        let plain: Model =
            serde_json::from_value(serde_json::json!({"name": "m", "model": "x"})).unwrap();
        assert!(!plain.vision);

        let seeing: Model =
            serde_json::from_value(serde_json::json!({"name": "m", "model": "x", "vision": true}))
                .unwrap();
        assert!(seeing.vision);
    }

    #[test]
    fn models_url_appends_the_models_path_once() {
        assert_eq!(models_url("http://host/v1"), "http://host/v1/models");
        assert_eq!(models_url("http://host/v1/"), "http://host/v1/models");
    }

    #[test]
    fn health_endpoint_prefers_an_explicit_url() {
        assert_eq!(
            health_endpoint("http://host/v1", Some("http://host/health")),
            "http://host/health"
        );
        assert_eq!(
            health_endpoint("http://host/v1", None),
            "http://host/v1/models"
        );
    }

    #[test]
    fn model_ids_reads_the_openai_list_shape() {
        let body = r#"{"object":"list","data":[{"id":"a"},{"id":"b"},{"object":"model"}]}"#;

        assert_eq!(model_ids(body), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn model_ids_is_empty_for_an_unexpected_body() {
        assert_eq!(model_ids("not json"), Vec::<String>::new());
        assert_eq!(model_ids(r#"{"data":[]}"#), Vec::<String>::new());
    }

    #[test]
    fn active_model_id_reads_the_resolved_model() {
        let register = ModelRegister {
            providers: HashMap::from([(
                "local".to_string(),
                ProviderConfig::Openai {
                    name: "Local".to_string(),
                    base_url: "http://host/v1".to_string(),
                    api_key: None,
                    health_url: None,
                    models: vec![Model {
                        name: "bonsai".to_string(),
                        model: "Bonsai-27B-PQ2_0".to_string(),
                        temperature: None,
                        max_tokens: None,
                        reasoning_effort: None,
                        vision: false,
                    }],
                },
            )]),
            route: ModelRoute {
                main: "local/bonsai".to_string(),
            },
        };

        assert_eq!(register.active_model_id(), Some("Bonsai-27B-PQ2_0"));
    }

    #[test]
    fn rate_limits_and_server_errors_are_retryable() {
        assert!(retryable(StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable(StatusCode::BAD_GATEWAY));
        assert!(retryable(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!retryable(StatusCode::UNAUTHORIZED));
        assert!(!retryable(StatusCode::FORBIDDEN));
        assert!(!retryable(StatusCode::NOT_FOUND));
        assert!(!retryable(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn a_native_provider_has_nothing_to_probe() {
        let register = ModelRegister {
            providers: HashMap::from([(
                "deepseek".to_string(),
                ProviderConfig::Native {
                    name: "Deepseek".to_string(),
                    provider: Provider::Deepseek,
                    models: vec![],
                },
            )]),
            route: ModelRoute {
                main: "deepseek/deepseek-flash".to_string(),
            },
        };

        assert!(register.check_health().await.is_ok());
    }
}
