use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct WorkspaceConfig {
    pub base: PathBuf,
}
