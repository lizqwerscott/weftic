use anyhow::{Result, bail};
use serde::Deserialize;

const DEFAULT_MAX_ITERATIONS: usize = 100;

#[derive(Debug, Deserialize)]
pub struct EngineConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
        }
    }
}

impl EngineConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_iterations == 0 {
            bail!("engine.max_iterations must be at least 1");
        }

        Ok(())
    }
}

fn default_max_iterations() -> usize {
    DEFAULT_MAX_ITERATIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_caps_a_turn_at_a_hundred_iterations() {
        assert_eq!(EngineConfig::default().max_iterations, 100);
    }

    #[test]
    fn zero_iterations_is_rejected() {
        let config = EngineConfig { max_iterations: 0 };
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "engine.max_iterations must be at least 1"
        );
    }

    #[test]
    fn one_iteration_is_accepted() {
        assert!(EngineConfig { max_iterations: 1 }.validate().is_ok());
    }
}
