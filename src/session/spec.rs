use std::path::PathBuf;

use crate::channel::SessionKey;

#[derive(Debug)]
pub struct SessionSpec {
    pub key: SessionKey,
    pub workspace_root: PathBuf,
    pub template: String,
}
