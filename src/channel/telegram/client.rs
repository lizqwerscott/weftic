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
