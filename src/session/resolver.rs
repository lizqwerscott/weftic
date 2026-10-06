use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use crate::channel::{Channel, SessionKey};
use crate::session::spec::SessionSpec;

pub struct SessionResolver {
    workspace_root: PathBuf,
    channel_templates: HashMap<Channel, String>,
}

#[derive(Debug, Clone)]
pub enum ResolveError {
    MalformedKey { key: SessionKey },
    UnconfiguredChannel { channel: Channel },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::MalformedKey { key } => write!(f, "malformed session key `{key}`"),
            ResolveError::UnconfiguredChannel { channel } => {
                write!(f, "no template configured for channel `{channel}`")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

impl SessionResolver {
    pub fn new(workspace_root: PathBuf, channel_templates: HashMap<Channel, String>) -> Self {
        Self {
            workspace_root,
            channel_templates,
        }
    }

    pub fn spec_for(&self, key: &SessionKey) -> Result<SessionSpec, ResolveError> {
        let target = key
            .parse()
            .ok_or_else(|| ResolveError::MalformedKey { key: key.clone() })?;

        let channel = *target.channel();
        let template = self
            .channel_templates
            .get(&channel)
            .ok_or(ResolveError::UnconfiguredChannel { channel })?
            .clone();

        Ok(SessionSpec {
            key: key.clone(),
            workspace_root: self.workspace_root.clone(),
            template,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::DeliveryTarget;

    fn resolver() -> SessionResolver {
        let mut channel_templates = HashMap::new();
        channel_templates.insert(Channel::Cli, "agent".to_string());
        SessionResolver::new(PathBuf::from("/work"), channel_templates)
    }

    #[test]
    fn spec_carries_the_key_and_resolved_template() {
        let key = DeliveryTarget::direct(Channel::Cli, "default", "cli").to_session_key("main");

        let spec = resolver().spec_for(&key).unwrap();

        assert_eq!(spec.key, key);
        assert_eq!(spec.workspace_root, PathBuf::from("/work"));
        assert_eq!(spec.template, "agent");
    }

    #[test]
    fn unconfigured_channel_is_rejected() {
        let key = DeliveryTarget::direct(Channel::Telegram, "default", "1").to_session_key("main");

        let error = resolver().spec_for(&key).unwrap_err();

        assert_eq!(
            error.to_string(),
            "no template configured for channel `telegram`"
        );
    }

    #[test]
    fn malformed_key_is_rejected() {
        let key = SessionKey::from("not-a-key");

        let error = resolver().spec_for(&key).unwrap_err();

        assert_eq!(error.to_string(), "malformed session key `not-a-key`");
    }
}
