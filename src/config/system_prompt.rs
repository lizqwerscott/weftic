use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SystemPromptConfig {
    pub character_path: PathBuf,
}
