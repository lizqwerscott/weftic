//! The `events` table: append-only platform / runtime event log with dedup.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{OptionalExtension, params};

use crate::database::Database;
use crate::event::{DedupKey, Event, EventId, EventOrigin};

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: EventId,
    pub event: Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended(EventId),
    Duplicate(EventId),
}

impl Database {
    pub(crate) async fn init_schema(&self) -> Result<()> {
        self.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS events (
                     id                INTEGER PRIMARY KEY,
                     platform          TEXT,
                     platform_event_id TEXT,
                     data              TEXT NOT NULL,
                     created_at        INTEGER NOT NULL
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS events_dedup
                     ON events(platform, platform_event_id)
                     WHERE platform_event_id IS NOT NULL;
                 CREATE TABLE IF NOT EXISTS meta (
                     key   TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );",
            )?;

            let existing: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;

            match existing {
                None => {
                    conn.execute(
                        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                        params![SCHEMA_VERSION.to_string()],
                    )?;
                }
                Some(value) => {
                    let found: i64 = value.parse().unwrap_or(-1);
                    if found != SCHEMA_VERSION {
                        tracing::warn!(
                            "event schema_version {found} differs from expected {SCHEMA_VERSION}"
                        );
                    }
                }
            }

            Ok(())
        })
        .await
    }

    pub async fn append_event(&self, event: &Event) -> Result<AppendOutcome> {
        let dedup = dedup_key(event);
        let data = serde_json::to_string(event).context("serializing event")?;
        let platform = dedup.as_ref().map(|key| key.platform.to_string());
        let platform_event_id = dedup.as_ref().map(|key| key.platform_event_id.clone());

        self.write(move |conn| {
            let changed = conn.execute(
                "INSERT OR IGNORE INTO events (platform, platform_event_id, data, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![platform, platform_event_id, data, now_millis()],
            )?;

            if changed == 1 {
                let id = EventId(conn.last_insert_rowid() as u64);
                return Ok(AppendOutcome::Appended(id));
            }

            let (platform, platform_event_id) = match (platform, platform_event_id) {
                (Some(platform), Some(platform_event_id)) => (platform, platform_event_id),
                _ => bail!("insert was ignored without a dedup key"),
            };

            let id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM events WHERE platform = ?1 AND platform_event_id = ?2",
                    params![platform, platform_event_id],
                    |row| row.get(0),
                )
                .optional()?;

            let id = id.ok_or_else(|| anyhow!("deduplicated event is missing from the store"))?;
            Ok(AppendOutcome::Duplicate(EventId(id as u64)))
        })
        .await
    }

    pub async fn get_event(&self, id: EventId) -> Result<Option<StoredEvent>> {
        self.read(move |conn| {
            let data: Option<String> = conn
                .query_row(
                    "SELECT data FROM events WHERE id = ?1",
                    params![id.0 as i64],
                    |row| row.get(0),
                )
                .optional()?;

            let Some(data) = data else {
                return Ok(None);
            };

            let event = serde_json::from_str(&data)
                .with_context(|| format!("deserializing stored event {}", id.0))?;

            Ok(Some(StoredEvent { id, event }))
        })
        .await
    }

    pub async fn event_count(&self) -> Result<usize> {
        self.read(|conn| {
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
            Ok(count as usize)
        })
        .await
    }
}

fn dedup_key(event: &Event) -> Option<DedupKey> {
    match &event.origin {
        EventOrigin::Platform(platform) => platform.dedup.clone(),
        EventOrigin::Runtime(_) => None,
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}
