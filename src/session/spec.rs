use std::path::PathBuf;

use crate::channel::SessionKey;
use crate::permissions::{Mode, Role};
use crate::session::ChannelBinding;

#[derive(Debug)]
pub struct SessionSpec {
    pub key: SessionKey,
    pub workspace_root: PathBuf,
    pub template: String,
    pub mode: Mode,
    pub role: Role,
    pub binding: ChannelBinding,
}
