//! Shared Telegram Bot API client: base URL, token, and the response envelope.
//!
//! Both the inbound poller and the outbound sender talk through this, so the
//! token and the HTTP connection pool live in exactly one place.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

const API_BASE: &str = "https://api.telegram.org";

pub struct TelegramClient {
    http: reqwest::Client,
    token: String,
}

impl TelegramClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            token: token.into(),
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{API_BASE}/bot{}/{}", self.token, method)
    }

    /// POST a method and return the raw body; only transport failures error.
    pub async fn post(&self, method: &str, body: &Value) -> Result<String> {
        self.http
            .post(self.url(method))
            .json(body)
            .send()
            .await
            .map_err(|error| anyhow!("calling telegram {method}: {}", error.without_url()))?
            .text()
            .await
            .map_err(|error| {
                anyhow!(
                    "reading telegram {method} response: {}",
                    error.without_url()
                )
            })
    }

    /// The bot's own account, as reported by `getMe`.
    pub async fn get_me(&self) -> Result<BotMe> {
        let body = self.get("getMe", &[]).await?;

        Envelope::<BotMe>::decode("getMe", &body)?
            .into_result("getMe")?
            .ok_or_else(|| anyhow!("telegram getMe returned no result"))
    }

    /// The bot's webhook status, as reported by `getWebhookInfo`.
    pub async fn get_webhook_info(&self) -> Result<WebhookInfo> {
        let body = self.get("getWebhookInfo", &[]).await?;

        Envelope::<WebhookInfo>::decode("getWebhookInfo", &body)?
            .into_result("getWebhookInfo")?
            .ok_or_else(|| anyhow!("telegram getWebhookInfo returned no result"))
    }

    /// GET a method with query parameters and return the raw body.
    pub async fn get(&self, method: &str, query: &[(&str, String)]) -> Result<String> {
        self.http
            .get(self.url(method))
            .query(query)
            .send()
            .await
            .map_err(|error| anyhow!("calling telegram {method}: {}", error.without_url()))?
            .text()
            .await
            .map_err(|error| {
                anyhow!(
                    "reading telegram {method} response: {}",
                    error.without_url()
                )
            })
    }
}

/// The bot's own account and global settings, as reported by `getMe`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BotMe {
    pub id: i64,
    pub username: Option<String>,
    /// Whether the bot may be added to groups.
    #[serde(default)]
    pub can_join_groups: bool,
    /// `true` when privacy mode is disabled, i.e. the bot reads all group
    /// messages (not just mentions / replies / commands).
    #[serde(default)]
    pub can_read_all_group_messages: bool,
    /// Whether forum-topic mode (threaded conversations) is enabled.
    #[serde(default)]
    pub has_topics_enabled: bool,
    /// Whether users may create and delete topics in private chats.
    #[serde(default)]
    pub allows_users_to_create_topics: bool,
}

/// The bot's webhook status, as reported by `getWebhookInfo`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WebhookInfo {
    /// The webhook URL, empty when no webhook is set up.
    #[serde(default)]
    pub url: String,
}

/// The `{ ok, description, result }` envelope every Bot API method returns.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub ok: bool,
    pub description: Option<String>,
    pub result: Option<T>,
}

impl<T: for<'de> Deserialize<'de>> Envelope<T> {
    pub fn decode(method: &str, body: &str) -> Result<Self> {
        serde_json::from_str(body).with_context(|| format!("decoding telegram {method} response"))
    }

    /// Reject an `ok: false` response; the payload (if any) is the caller's.
    pub fn into_result(self, method: &str) -> Result<Option<T>> {
        if !self.ok {
            return Err(anyhow!(
                "telegram {method} failed: {}",
                self.description
                    .unwrap_or_else(|| "unknown error".to_string())
            ));
        }

        Ok(self.result)
    }
}
