use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::{
    access::AccessPolicy,
    channel::{
        Channel,
        registry::ChannelRegistry,
        telegram::{
            client::{BotMe, TelegramClient},
            runtime::TelegramRuntime,
        },
    },
    config::{Config, channel::DEFAULT_MAX_SEND_ATTEMPTS},
};

pub mod access;
pub mod agent_engine;
pub mod channel;
pub mod config;
pub mod database;
pub mod event;
pub mod identity;
pub mod inbound;
pub mod output;
pub mod permissions;
pub mod projection;
pub mod session;
pub mod system_prompt;
pub mod tools;
mod workspace;

pub async fn registry_channels(config: &Config) -> Result<Arc<ChannelRegistry>> {
    let mut registry = ChannelRegistry::new();

    if config.is_channel_enabled("telegram") {
        if let Some(token) = config.channel_token("telegram")? {
            let attempts = config
                .channels
                .get("telegram")
                .map(|channel| channel.max_send_attempts)
                .unwrap_or(DEFAULT_MAX_SEND_ATTEMPTS);
            let client = Arc::new(TelegramClient::new(token));
            let bot = client
                .get_me()
                .await
                .context("fetching the telegram bot identity")?;

            let webhook_url = match client.get_webhook_info().await {
                Ok(info) if !info.url.is_empty() => Some(info.url),
                Ok(_) => None,
                Err(error) => {
                    warn!(target: "telegram", "webhook check failed: {error:#}");
                    None
                }
            };

            let report = bot_report(&bot, &config.access, webhook_url.as_deref());
            info!(target: "telegram", "{}", report.overview);
            for warning in &report.warnings {
                warn!(target: "telegram", "{}", warning);
            }

            registry.register(
                Channel::Telegram,
                Arc::new(TelegramRuntime::new(client, "default", bot, attempts)),
            );
            info!(target: "telegram", "channel registered (account `default`)");
        } else {
            warn!(target: "telegram", "channel enabled but no token configured; skipping");
        }
    }

    Ok(Arc::new(registry))
}

/// The Tier-1 startup self-check: a one-line `getMe` overview plus
/// cross-warnings against the configured access policy and webhook state.
/// Diagnostics only.
struct BotReport {
    overview: String,
    warnings: Vec<String>,
}

fn bot_report(bot: &BotMe, access: &AccessPolicy, webhook_url: Option<&str>) -> BotReport {
    let overview = format!(
        "@{} ({}) — privacy={} · topics={} · can_join_groups={}",
        bot.username.as_deref().unwrap_or("?"),
        bot.id,
        if bot.can_read_all_group_messages {
            "off"
        } else {
            "on"
        },
        if bot.has_topics_enabled { "on" } else { "off" },
        bot.can_join_groups,
    );

    let mut warnings = Vec::new();

    if let Some(url) = webhook_url {
        warnings.push(format!(
            "a webhook is set ({url}): getUpdates long polling will not receive updates — run deleteWebhook"
        ));
    }

    if access.dm_allow.is_empty() && access.groups.is_empty() {
        warnings.push(
            "no entry point configured: `[access]` has neither `dm_allow` nor `[[access.groups]]`, so nobody can reach the bot".to_string(),
        );
    }

    if !access.groups.is_empty() && !bot.can_read_all_group_messages {
        warnings.push(
            "privacy mode is on: only group messages that @ the bot, reply to it, or are /commands are delivered — disable it with BotFather /setprivacy".to_string(),
        );
    }

    if !access.groups.is_empty() && !bot.can_join_groups {
        warnings.push(
            "the bot cannot join groups, so the configured [[access.groups]] entries are unreachable".to_string(),
        );
    }

    if bot.has_topics_enabled {
        warnings.push(
            "topics are enabled (threaded mode), but private-chat topic routing is not implemented yet: all topics share one session (§4.1)".to_string(),
        );
    }

    BotReport { overview, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::GroupAccess;

    fn bot() -> BotMe {
        BotMe {
            id: 42,
            username: Some("weftic_bot".to_string()),
            ..Default::default()
        }
    }

    fn access(dm_allow: &[&str], groups: &[&str]) -> AccessPolicy {
        AccessPolicy {
            dm_allow: dm_allow.iter().map(|s| s.to_string()).collect(),
            groups: groups
                .iter()
                .map(|id| GroupAccess {
                    id: id.to_string(),
                    deny: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn overview_reads_privacy_topics_and_group_flags() {
        let bot = BotMe {
            can_read_all_group_messages: true,
            has_topics_enabled: true,
            can_join_groups: true,
            ..bot()
        };

        assert_eq!(
            bot_report(&bot, &access(&["telegram:1"], &[]), None).overview,
            "@weftic_bot (42) — privacy=off · topics=on · can_join_groups=true"
        );
    }

    #[test]
    fn a_bot_without_a_username_falls_back_to_a_placeholder() {
        let bot = BotMe {
            username: None,
            ..bot()
        };

        assert_eq!(
            bot_report(&bot, &access(&["telegram:1"], &[]), None).overview,
            "@? (42) — privacy=on · topics=off · can_join_groups=false"
        );
    }

    #[test]
    fn a_set_webhook_warns_that_polling_will_not_work() {
        let warnings =
            bot_report(&bot(), &access(&["telegram:1"], &[]), Some("https://x/y")).warnings;

        assert_eq!(
            warnings,
            vec![
                "a webhook is set (https://x/y): getUpdates long polling will not receive updates — run deleteWebhook".to_string(),
            ]
        );
    }

    #[test]
    fn an_empty_access_policy_warns_that_nobody_can_reach_the_bot() {
        let warnings = bot_report(&bot(), &access(&[], &[]), None).warnings;

        assert_eq!(
            warnings,
            vec![
                "no entry point configured: `[access]` has neither `dm_allow` nor `[[access.groups]]`, so nobody can reach the bot".to_string(),
            ]
        );
    }

    #[test]
    fn group_warnings_fire_only_when_groups_are_configured() {
        let bot = bot();

        assert!(
            bot_report(&bot, &access(&["telegram:1"], &[]), None)
                .warnings
                .is_empty()
        );
        assert_eq!(
            bot_report(&bot, &access(&["telegram:1"], &["telegram:-100"]), None).warnings,
            vec![
                "privacy mode is on: only group messages that @ the bot, reply to it, or are /commands are delivered — disable it with BotFather /setprivacy".to_string(),
                "the bot cannot join groups, so the configured [[access.groups]] entries are unreachable".to_string(),
            ]
        );
    }

    #[test]
    fn enabled_topics_warn_about_the_missing_routing() {
        let bot = BotMe {
            has_topics_enabled: true,
            ..bot()
        };

        assert_eq!(
            bot_report(&bot, &access(&["telegram:1"], &[]), None).warnings,
            vec![
                "topics are enabled (threaded mode), but private-chat topic routing is not implemented yet: all topics share one session (§4.1)".to_string(),
            ]
        );
    }
}
