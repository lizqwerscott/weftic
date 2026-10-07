//! Channel capabilities: what a channel can do, independent of transport.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    Off,
    Draft,
    InPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyMode {
    Automatic,
    MessageTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelCapabilities {
    pub stream: StreamMode,
    pub reply: ReplyMode,
}

impl ChannelCapabilities {
    pub const fn new(stream: StreamMode, reply: ReplyMode) -> Self {
        Self { stream, reply }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_carry_both_axes() {
        let capabilities = ChannelCapabilities::new(StreamMode::InPlace, ReplyMode::Automatic);

        assert_eq!(capabilities.stream, StreamMode::InPlace);
        assert_eq!(capabilities.reply, ReplyMode::Automatic);
    }
}
