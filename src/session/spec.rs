use crate::channel::SessionKey;
use crate::permissions::{Mode, Role};
use crate::session::ChannelBinding;
use crate::workspace::WorkspacePolicy;

#[derive(Debug)]
pub struct SessionSpec {
    pub key: SessionKey,
    pub workspace_policy: WorkspacePolicy,
    pub template: String,
    pub mode: Mode,
    pub role: Role,
    pub binding: ChannelBinding,
}
