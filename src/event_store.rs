use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};

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

pub struct EventStore {
    conn: Connection,
}

impl EventStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating event store directory {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("opening event store {}", path.display()))?;

        Self::from_connection(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        })
        .context("enabling WAL journal mode")?;

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
        )
        .context("initializing event store schema")?;

        let store = Self { conn };
        store.check_schema_version()?;
        Ok(store)
    }

    fn check_schema_version(&self) -> Result<()> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;

        match existing {
            None => {
                self.conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(value) => {
                let found: i64 = value.parse().unwrap_or(-1);
                if found != SCHEMA_VERSION {
                    tracing::warn!(
                        "event store schema_version {found} differs from expected {SCHEMA_VERSION}"
                    );
                }
            }
        }

        Ok(())
    }

    pub fn append(&self, event: &Event) -> Result<AppendOutcome> {
        let dedup = dedup_key(event);
        let data = serde_json::to_string(event).context("serializing event")?;

        let changed = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO events (platform, platform_event_id, data, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    dedup.as_ref().map(|key| key.platform.to_string()),
                    dedup.as_ref().map(|key| key.platform_event_id.as_str()),
                    data,
                    now_millis(),
                ],
            )
            .context("appending event")?;

        if changed == 1 {
            let id = EventId(self.conn.last_insert_rowid() as u64);
            return Ok(AppendOutcome::Appended(id));
        }

        let key = dedup.ok_or_else(|| anyhow!("insert was ignored without a dedup key"))?;
        let id = self
            .find_id(&key)?
            .ok_or_else(|| anyhow!("deduplicated event is missing from the store"))?;
        Ok(AppendOutcome::Duplicate(id))
    }

    pub fn get(&self, id: EventId) -> Result<Option<StoredEvent>> {
        let data: Option<String> = self
            .conn
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
    }

    pub fn len(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    fn find_id(&self, key: &DedupKey) -> Result<Option<EventId>> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM events WHERE platform = ?1 AND platform_event_id = ?2",
                params![key.platform.to_string(), key.platform_event_id],
                |row| row.get(0),
            )
            .optional()?;

        Ok(id.map(|value| EventId(value as u64)))
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::event::{ActorRef, Delivery, InternalScope, RuntimeEvent, RuntimeKind};

    fn memory_store() -> EventStore {
        EventStore::in_memory().unwrap()
    }

    fn message(text: &str) -> Event {
        Event::platform_text(
            DeliveryTarget::direct(Channel::Cli, "default", "cli"),
            ActorRef::new("member_cli"),
            text,
        )
    }

    fn message_with_dedup(text: &str, platform: Channel, platform_event_id: &str) -> Event {
        let mut event = Event::platform_text(
            DeliveryTarget::direct(platform, "default", "1"),
            ActorRef::new("member_cli"),
            text,
        );

        let EventOrigin::Platform(platform_event) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform_event.dedup = Some(DedupKey {
            platform,
            platform_event_id: platform_event_id.to_string(),
        });

        event
    }

    fn heartbeat_notice() -> Event {
        Event {
            delivery: Delivery::Ephemeral,
            origin: EventOrigin::Runtime(RuntimeEvent {
                kind: RuntimeKind::Notice,
                scope: InternalScope::Heartbeat,
                correlation_id: None,
            }),
        }
    }

    #[test]
    fn append_assigns_increasing_ids_in_order() {
        let store = memory_store();

        assert_eq!(
            store.append(&message("one")).unwrap(),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store.append(&message("two")).unwrap(),
            AppendOutcome::Appended(EventId(2))
        );
        assert_eq!(
            store.append(&message("three")).unwrap(),
            AppendOutcome::Appended(EventId(3))
        );

        assert_eq!(store.len().unwrap(), 3);
        assert!(!store.is_empty().unwrap());
    }

    #[test]
    fn a_replayed_event_returns_the_original_id_without_growing_the_log() {
        let store = memory_store();
        let event = message_with_dedup("hi", Channel::Telegram, "42");

        assert_eq!(
            store.append(&event).unwrap(),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store.append(&event).unwrap(),
            AppendOutcome::Duplicate(EventId(1))
        );

        assert_eq!(store.len().unwrap(), 1);
    }

    #[test]
    fn a_duplicate_does_not_consume_an_id() {
        let store = memory_store();
        let first = message_with_dedup("hi", Channel::Telegram, "42");

        assert_eq!(
            store.append(&first).unwrap(),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store.append(&first).unwrap(),
            AppendOutcome::Duplicate(EventId(1))
        );
        assert_eq!(
            store
                .append(&message_with_dedup("other", Channel::Telegram, "43"))
                .unwrap(),
            AppendOutcome::Appended(EventId(2))
        );
    }

    #[test]
    fn a_dedup_key_is_scoped_to_its_platform() {
        let store = memory_store();

        assert_eq!(
            store
                .append(&message_with_dedup("hi", Channel::Telegram, "42"))
                .unwrap(),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store
                .append(&message_with_dedup("hi", Channel::QQ, "42"))
                .unwrap(),
            AppendOutcome::Appended(EventId(2))
        );

        assert_eq!(store.len().unwrap(), 2);
    }

    #[test]
    fn events_without_a_dedup_key_are_never_deduped() {
        let store = memory_store();

        assert_eq!(
            store.append(&message("same")).unwrap(),
            AppendOutcome::Appended(EventId(1))
        );
        assert_eq!(
            store.append(&message("same")).unwrap(),
            AppendOutcome::Appended(EventId(2))
        );
        assert_eq!(
            store.append(&heartbeat_notice()).unwrap(),
            AppendOutcome::Appended(EventId(3))
        );

        assert_eq!(store.len().unwrap(), 3);
    }

    #[test]
    fn get_returns_the_stored_event_and_unknown_ids_are_none() {
        let store = memory_store();
        let event = message("hi");
        store.append(&event).unwrap();

        assert_eq!(store.get(EventId(1)).unwrap().unwrap().event, event);
        assert!(store.get(EventId(999)).unwrap().is_none());
    }

    #[test]
    fn dedup_survives_reopening_the_store() {
        let path = temp_db_path();

        {
            let store = EventStore::open(&path).unwrap();
            assert_eq!(
                store
                    .append(&message_with_dedup("hi", Channel::Telegram, "42"))
                    .unwrap(),
                AppendOutcome::Appended(EventId(1))
            );
        }

        {
            let store = EventStore::open(&path).unwrap();
            assert_eq!(
                store
                    .append(&message_with_dedup("hi", Channel::Telegram, "42"))
                    .unwrap(),
                AppendOutcome::Duplicate(EventId(1))
            );
            assert_eq!(store.len().unwrap(), 1);
        }

        remove_db(&path);
    }

    fn temp_db_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir().join(format!(
            "weftic_events_{}_{}_{}.db",
            std::process::id(),
            n,
            nanos
        ))
    }

    fn remove_db(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let _ = std::fs::remove_file(Path::new(&file));
        }
    }
}
