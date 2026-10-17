use anyhow::{Context, Result};
use serde::Deserialize;

use crate::clock::Clock;

/// `[clock]` — how model-facing timestamps are zoned (§11.2).
#[derive(Debug, Default, Deserialize)]
pub struct ClockConfig {
    /// A fixed UTC offset: `UTC` (default), `+08:00`, `-05:30`, `UTC+8` …
    #[serde(default)]
    pub utc_offset: Option<String>,
}

impl ClockConfig {
    pub fn clock(&self) -> Result<Clock> {
        match &self.utc_offset {
            Some(spec) => {
                Clock::parse(spec).with_context(|| format!("[clock].utc_offset = `{spec}`"))
            }
            None => Ok(Clock::utc()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_offset_is_utc() {
        assert_eq!(ClockConfig::default().clock().unwrap().label(), "UTC");
    }

    #[test]
    fn a_configured_offset_is_used() {
        let config = ClockConfig {
            utc_offset: Some("+08:00".to_string()),
        };

        assert_eq!(config.clock().unwrap().label(), "UTC+08:00");
    }

    #[test]
    fn a_bad_offset_names_the_key_and_the_value() {
        let config = ClockConfig {
            utc_offset: Some("Asia/Shanghai".to_string()),
        };

        let error = config.clock().unwrap_err();
        assert_eq!(error.to_string(), "[clock].utc_offset = `Asia/Shanghai`");
        assert_eq!(
            error.chain().nth(1).map(ToString::to_string).as_deref(),
            Some("`Asia/Shanghai` is not a UTC offset (expected `UTC` or something like `+08:00`)")
        );
    }
}
