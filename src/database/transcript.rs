//! The `messages` transcript: the model-side, append-only record of every
//! turn's steps.
//!
//! `events` is the source-side log (platform / runtime facts); `messages` is
//! the model-side log (the exact `ChatMessage`s sent to and received from the
//! provider). The two overlap only on the inbound user step, linked by
//! `source_event_id -> events.id`.

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use genai::chat::ChatMessage;

use crate::channel::SessionKey;
use crate::database::Database;
use crate::event::{EventId, Timestamp};
use crate::session::history::{SessionHistory, SessionStep, SessionTurn};
use crate::tools::message::MessageTool;

const TRANSCRIPT_SCHEMA_VERSION: i64 = 1;

/// One step read back from the table, before it is deserialized.
struct StoredStep {
    turn_idx: i64,
    content: String,
    source_event_id: Option<i64>,
}

/// A step prepared for insertion.
struct SerializedStep {
    role: String,
    content: String,
    source_event_id: Option<i64>,
    sent_message_id: Option<String>,
}

impl Database {
    pub(crate) async fn init_transcript_schema(&self) -> Result<()> {
        self.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS messages (
                     id               INTEGER PRIMARY KEY,
                     session_key      TEXT NOT NULL,
                     turn_idx         INTEGER NOT NULL,
                     step_idx         INTEGER NOT NULL,
                     role             TEXT NOT NULL,
                     content          TEXT NOT NULL,
                     source_event_id  INTEGER,
                     sent_message_id  TEXT,
                     created_at       INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS messages_session
                     ON messages(session_key, turn_idx, step_idx);",
            )?;

            let existing: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'transcript_schema_version'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;

            match existing {
                None => {
                    conn.execute(
                        "INSERT INTO meta (key, value) VALUES ('transcript_schema_version', ?1)",
                        params![TRANSCRIPT_SCHEMA_VERSION.to_string()],
                    )?;
                }
                Some(value) => {
                    let found: i64 = value.parse().unwrap_or(-1);
                    if found != TRANSCRIPT_SCHEMA_VERSION {
                        tracing::warn!(
                            "transcript schema_version {found} differs from expected \
                             {TRANSCRIPT_SCHEMA_VERSION}; historical sessions will be replayed \
                             from scratch"
                        );
                    }
                }
            }

            Ok(())
        })
        .await
    }

    /// Persist one completed turn's steps in a single transaction, so a
    /// `tool_call` and its `tool_response` are never split across a crash.
    ///
    /// `turn_idx` is the session's logical turn number at the time the turn
    /// completed (i.e. `Session::turn_count()` before the turn is appended).
    pub async fn append_turn(
        &self,
        session_key: &SessionKey,
        turn_idx: usize,
        turn: &SessionTurn,
    ) -> Result<()> {
        let key = session_key.as_str().to_string();
        let steps = serialize_steps(turn)?;
        let now = Timestamp::now().as_millis();

        self.write(move |conn| {
            let tx = conn.transaction()?;
            for (step_idx, step) in steps.into_iter().enumerate() {
                tx.execute(
                    "INSERT INTO messages
                         (session_key, turn_idx, step_idx, role, content,
                          source_event_id, sent_message_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        key,
                        turn_idx as i64,
                        step_idx as i64,
                        step.role,
                        step.content,
                        step.source_event_id,
                        step.sent_message_id,
                        now,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Rebuild a session's turn history from the transcript, oldest first.
    ///
    /// A step whose payload no longer deserializes (schema drift, a genai
    /// upgrade, …) degrades the whole session to an empty history instead of
    /// panicking: the turn boundary is the authority, and a half-loaded prefix
    /// would be worse than starting fresh.
    pub async fn load_history(&self, session_key: &SessionKey) -> Result<SessionHistory> {
        let key = session_key.as_str().to_string();
        let query_key = key.clone();
        let rows = self
            .read(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT turn_idx, content, source_event_id
                     FROM messages
                     WHERE session_key = ?1
                     ORDER BY turn_idx, step_idx, id",
                )?;
                let rows = stmt.query_map(params![query_key], |row| {
                    Ok(StoredStep {
                        turn_idx: row.get(0)?,
                        content: row.get(1)?,
                        source_event_id: row.get(2)?,
                    })
                })?;

                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(anyhow::Error::from)
            })
            .await?;

        let mut turns: Vec<SessionTurn> = Vec::new();
        let mut current: Option<(i64, Vec<SessionStep>)> = None;

        for row in rows {
            let message: ChatMessage = match serde_json::from_str(&row.content) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(
                        target: "transcript",
                        "dropping history for `{key}`: a step no longer deserializes ({error:#})"
                    );
                    return Ok(SessionHistory::default());
                }
            };

            let source_event_id = row
                .source_event_id
                .filter(|id| *id > 0)
                .map(|id| EventId(id as u64));
            let step = SessionStep::new(message, source_event_id);

            match &mut current {
                Some((turn_idx, steps)) if *turn_idx == row.turn_idx => steps.push(step),
                _ => {
                    if let Some((_, steps)) = current.take() {
                        turns.push(SessionTurn::from_steps(steps));
                    }
                    current = Some((row.turn_idx, vec![step]));
                }
            }
        }

        if let Some((_, steps)) = current.take() {
            turns.push(SessionTurn::from_steps(steps));
        }

        Ok(SessionHistory { turns })
    }
}

fn serialize_steps(turn: &SessionTurn) -> Result<Vec<SerializedStep>> {
    turn.steps()
        .iter()
        .map(|step| {
            Ok(SerializedStep {
                role: step.data().role.to_string(),
                content: serde_json::to_string(step.data())
                    .context("serializing a transcript step")?,
                source_event_id: step.source_event_id().map(|id| id.0 as i64),
                sent_message_id: sent_message_id_of(step),
            })
        })
        .collect()
}

/// The platform id of the message this step delivered, if any. A step is a
/// delivery receipt exactly when it is the tool response of a `message(send)`;
/// the `message` tool owns the `"sent message {id}"` format (§8.3).
fn sent_message_id_of(step: &SessionStep) -> Option<String> {
    let responses = step.data().content.tool_responses();
    if responses.len() != 1 {
        return None;
    }

    MessageTool::sent_message_id_of(responses[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    use genai::chat::{ToolCall, ToolResponse};

    use crate::channel::{Channel, DeliveryTarget};

    fn key(name: &str) -> SessionKey {
        DeliveryTarget::direct(Channel::Webui, "default", name).to_session_key("main")
    }

    async fn database() -> Database {
        Database::in_memory().await.unwrap()
    }

    /// A turn shaped like a real tool loop: user → assistant(tool_call) →
    /// tool(response) → assistant(final).
    fn turn_with_steps(source: Option<EventId>) -> SessionTurn {
        let mut steps = Vec::new();
        steps.push(SessionStep::new(ChatMessage::user("hi"), source));
        steps.push(SessionStep::new(
            ChatMessage::from(vec![ToolCall {
                call_id: "call_1".to_string(),
                fn_name: "message".to_string(),
                fn_arguments: serde_json::json!({"action": "send", "text": "yo"}),
                thought_signatures: None,
            }]),
            None,
        ));
        steps.push(SessionStep::new(
            ChatMessage::from(
                ToolResponse::new("call_1", "sent message msg_1").with_fn_name("message"),
            ),
            None,
        ));
        steps.push(SessionStep::new(ChatMessage::assistant("done"), None));

        SessionTurn::from_steps(steps)
    }

    #[tokio::test]
    async fn a_turn_roundtrips_through_the_transcript() {
        let db = database().await;
        let k = key("roundtrip");

        db.append_turn(&k, 0, &turn_with_steps(Some(EventId(7))))
            .await
            .unwrap();

        let history = db.load_history(&k).await.unwrap();
        assert_eq!(history.turns.len(), 1);

        let loaded = &history.turns[0];
        assert_eq!(loaded.steps().len(), 4);
        assert_eq!(loaded.steps()[0].data().role.to_string(), "User");
        assert_eq!(loaded.steps()[0].source_event_id(), Some(EventId(7)));
        assert_eq!(loaded.steps()[1].data().role.to_string(), "Assistant");
        assert_eq!(loaded.steps()[2].data().role.to_string(), "Tool");
        assert_eq!(loaded.steps()[3].data().role.to_string(), "Assistant");
    }

    #[tokio::test]
    async fn multiple_turns_load_in_order_and_keep_their_source_events() {
        let db = database().await;
        let k = key("ordered");

        db.append_turn(&k, 0, &turn_with_steps(Some(EventId(1))))
            .await
            .unwrap();
        db.append_turn(&k, 1, &turn_with_steps(Some(EventId(2))))
            .await
            .unwrap();

        let history = db.load_history(&k).await.unwrap();
        assert_eq!(history.turns.len(), 2);
        assert_eq!(
            history.turns[0].steps()[0].source_event_id(),
            Some(EventId(1))
        );
        assert_eq!(
            history.turns[1].steps()[0].source_event_id(),
            Some(EventId(2))
        );
    }

    #[tokio::test]
    async fn distinct_sessions_are_isolated() {
        let db = database().await;

        db.append_turn(&key("a"), 0, &turn_with_steps(Some(EventId(1))))
            .await
            .unwrap();

        let history = db.load_history(&key("b")).await.unwrap();
        assert!(history.turns.is_empty());
    }

    #[tokio::test]
    async fn a_step_that_no_longer_deserializes_degrades_to_a_fresh_session() {
        let db = database().await;
        let k = key("drift");

        db.append_turn(&k, 0, &turn_with_steps(Some(EventId(1))))
            .await
            .unwrap();

        let key = k.as_str().to_string();
        db.write(move |conn| {
            conn.execute(
                "UPDATE messages SET content = 'not json' WHERE session_key = ?1",
                params![key],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let history = db.load_history(&k).await.unwrap();
        assert!(history.turns.is_empty());
    }

    #[tokio::test]
    async fn a_message_send_backfills_its_sent_message_id_on_the_tool_row() {
        let db = database().await;
        let k = key("backfill");

        db.append_turn(&k, 0, &turn_with_steps(Some(EventId(1))))
            .await
            .unwrap();

        let key = k.as_str().to_string();
        let ids = db
            .read(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT sent_message_id FROM messages
                     WHERE session_key = ?1 ORDER BY step_idx, id",
                )?;
                let rows = stmt.query_map(params![key], |row| row.get::<_, Option<String>>(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();

        // user → assistant(tool_call) → tool("sent message msg_1") → assistant
        assert_eq!(ids, vec![None, None, Some("msg_1".to_string()), None]);
    }
}
