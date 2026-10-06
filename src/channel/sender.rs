use std::future::Future;
use std::pin::Pin;

pub type BoxedSendFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMessage {
    pub id: String,
}

impl SentMessage {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

pub trait ChannelSender: Send + Sync {
    fn send_message<'a>(
        &'a self,
        text: &'a str,
    ) -> BoxedSendFuture<'a, anyhow::Result<SentMessage>>;

    fn edit_message<'a>(
        &'a self,
        message_id: &'a str,
        text: &'a str,
    ) -> BoxedSendFuture<'a, anyhow::Result<()>>;
}

#[cfg(test)]
pub mod testing {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MockMessage {
        pub id: String,
        pub text: String,
    }

    #[derive(Default)]
    pub struct MockChannelSender {
        sent: Mutex<Vec<MockMessage>>,
        edits: Mutex<Vec<(String, String)>>,
        next_id: Mutex<u64>,
    }

    impl MockChannelSender {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        pub fn sent(&self) -> Vec<MockMessage> {
            self.sent.lock().unwrap().clone()
        }

        pub fn edits(&self) -> Vec<(String, String)> {
            self.edits.lock().unwrap().clone()
        }
    }

    impl ChannelSender for MockChannelSender {
        fn send_message<'a>(
            &'a self,
            text: &'a str,
        ) -> BoxedSendFuture<'a, anyhow::Result<SentMessage>> {
            Box::pin(async move {
                let mut next_id = self.next_id.lock().unwrap();
                *next_id += 1;
                let id = format!("msg_{next_id}");
                self.sent.lock().unwrap().push(MockMessage {
                    id: id.clone(),
                    text: text.to_owned(),
                });
                Ok(SentMessage::new(id))
            })
        }

        fn edit_message<'a>(
            &'a self,
            message_id: &'a str,
            text: &'a str,
        ) -> BoxedSendFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                self.edits
                    .lock()
                    .unwrap()
                    .push((message_id.to_owned(), text.to_owned()));
                Ok(())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{MockChannelSender, MockMessage};
    use super::*;

    #[tokio::test]
    async fn send_records_the_text_and_returns_an_increasing_id() {
        let sender = MockChannelSender::new();

        let first = sender.send_message("hello").await.unwrap();
        let second = sender.send_message("again").await.unwrap();

        assert_eq!(first.id, "msg_1");
        assert_eq!(second.id, "msg_2");
        assert_eq!(
            sender.sent(),
            vec![
                MockMessage {
                    id: "msg_1".to_string(),
                    text: "hello".to_string(),
                },
                MockMessage {
                    id: "msg_2".to_string(),
                    text: "again".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn edit_records_the_target_and_new_text() {
        let sender = MockChannelSender::new();

        sender.edit_message("msg_7", "fixed").await.unwrap();

        assert_eq!(
            sender.edits(),
            vec![("msg_7".to_string(), "fixed".to_string())]
        );
    }
}
