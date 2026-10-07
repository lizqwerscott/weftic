use std::fmt;

use genai::chat::{ChatMessage, ChatRequest, StopReason, Usage};

use crate::event::EventId;

pub struct SessionStep {
    data: ChatMessage,
    source_event_id: Option<EventId>,
}

#[derive(Debug, Clone)]
pub enum TurnError {
    Request(String),
    Stream(String),
    EmptyResponse,
    MaxIterations { limit: usize },
    Other(String),
}

impl fmt::Display for TurnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TurnError::Request(message) => write!(f, "model request failed: {message}"),
            TurnError::Stream(message) => write!(f, "model stream failed: {message}"),
            TurnError::EmptyResponse => {
                write!(f, "empty assistant response (no text, no tool calls)")
            }
            TurnError::MaxIterations { limit } => {
                write!(f, "agent did not finish within {limit} iterations")
            }
            TurnError::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TurnError {}

pub enum TurnStatus {
    InFlight,
    Complete,
    Failed(TurnError),
}

pub struct SessionTurn {
    steps: Vec<SessionStep>,
    status: TurnStatus,
    stop_reason: Option<StopReason>,
    usage: Option<Usage>,
}

impl SessionTurn {
    pub fn new(
        chat_req: &ChatRequest,
        source_event_id: Option<EventId>,
        status: TurnStatus,
        stop_reason: Option<StopReason>,
        usage: Option<Usage>,
    ) -> Self {
        let mut steps: Vec<SessionStep> = chat_req
            .messages
            .iter()
            .map(|msg| SessionStep {
                data: msg.clone(),
                source_event_id: None,
            })
            .collect();

        if let Some(first) = steps.first_mut() {
            first.source_event_id = source_event_id;
        }

        Self {
            steps,
            status,
            stop_reason,
            usage,
        }
    }

    pub fn status(&self) -> &TurnStatus {
        &self.status
    }

    fn to_chat_request(&self) -> ChatRequest {
        let chat_message: Vec<ChatMessage> =
            self.steps.iter().map(|step| step.data.clone()).collect();
        ChatRequest::from_messages(chat_message)
    }
}

#[derive(Default)]
pub struct SessionHistory {
    pub turns: Vec<SessionTurn>,
}

impl SessionHistory {
    pub fn to_chat_request(&self) -> ChatRequest {
        let mut chat_message: Vec<ChatMessage> = Vec::new();
        for turn in self.turns.iter() {
            let mut turn_chat_message: Vec<ChatMessage> =
                turn.steps.iter().map(|step| step.data.clone()).collect();

            chat_message.append(&mut turn_chat_message);
        }

        ChatRequest::from_messages(chat_message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_error_renders_its_message() {
        let error = TurnError::Request("connection reset by peer".to_string());
        assert_eq!(
            error.to_string(),
            "model request failed: connection reset by peer"
        );
    }

    #[test]
    fn stream_error_renders_its_message() {
        let error = TurnError::Stream("unexpected end of stream".to_string());
        assert_eq!(
            error.to_string(),
            "model stream failed: unexpected end of stream"
        );
    }

    #[test]
    fn empty_response_renders_without_a_payload() {
        assert_eq!(
            TurnError::EmptyResponse.to_string(),
            "empty assistant response (no text, no tool calls)"
        );
    }

    #[test]
    fn max_iterations_renders_the_limit() {
        assert_eq!(
            TurnError::MaxIterations { limit: 100 }.to_string(),
            "agent did not finish within 100 iterations"
        );
    }

    #[test]
    fn other_renders_verbatim() {
        assert_eq!(TurnError::Other("boom".to_string()).to_string(), "boom");
    }

    #[test]
    fn a_turn_attaches_its_source_event_to_the_opening_step_only() {
        use genai::chat::MessageContent;

        let chat = ChatRequest::default()
            .append_message(ChatMessage::user("hi"))
            .append_message(ChatMessage::assistant(MessageContent::from_text("hello")));

        let turn = SessionTurn::new(&chat, Some(EventId(7)), TurnStatus::Complete, None, None);

        assert_eq!(turn.steps[0].source_event_id, Some(EventId(7)));
        assert_eq!(turn.steps[1].source_event_id, None);
    }
}
