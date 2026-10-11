//! The identity name layer: a ref's current display name plus its rename
//! history (see `docs/architecture.md` §12).
//!
//! Two tables:
//! - `channel_identities`: one row per ref, the latest name we saw;
//! - `aliases`: every name ever observed, scoped to `channel` / `chat`.
//!
//! Admission never reads this (it is a pure function of the event), and the
//! projection reads it for the message block header's `name`. A rename goes
//! through `aliases`; the ref itself never changes.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, params_from_iter};

use crate::database::Database;
use crate::event::{ActorRef, Timestamp};
use crate::identity::ObservedIdentity;

impl Database {
    pub(crate) async fn init_identity_schema(&self) -> Result<()> {
        self.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS channel_identities (
                     ref          TEXT PRIMARY KEY,
                     channel      TEXT NOT NULL,
                     account_id   TEXT NOT NULL,
                     platform_id  TEXT NOT NULL,
                     current_name TEXT,
                     updated_at   INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS aliases (
                     ref      TEXT NOT NULL,
                     scope    TEXT NOT NULL,
                     scope_id TEXT NOT NULL,
                     name     TEXT NOT NULL,
                     seen_at  INTEGER NOT NULL,
                     PRIMARY KEY (ref, scope, scope_id, name)
                 );",
            )?;

            Ok(())
        })
        .await
    }

    /// Record the name seen for a ref: upsert the current name and keep every
    /// observed name as an alias, so a rename never changes the ref.
    pub async fn observe_identity(&self, observed: &ObservedIdentity) -> Result<()> {
        let reference = observed.reference.as_str().to_string();
        let channel = observed.channel.to_string();
        let account_id = observed.account_id.clone();
        let platform_id = observed.platform_id.clone();
        let name = observed.name.clone();
        let chat_scope = observed.chat_scope.clone();
        let now = Timestamp::now().as_millis();

        self.write(move |conn| {
            conn.execute(
                "INSERT INTO channel_identities
                     (ref, channel, account_id, platform_id, current_name, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(ref) DO UPDATE SET
                     platform_id  = excluded.platform_id,
                     current_name = COALESCE(excluded.current_name, channel_identities.current_name),
                     updated_at   = excluded.updated_at",
                params![reference, channel, account_id, platform_id, name, now],
            )?;

            let Some(name) = name else {
                return Ok(());
            };

            conn.execute(
                "INSERT OR IGNORE INTO aliases (ref, scope, scope_id, name, seen_at)
                 VALUES (?1, 'channel', ?2, ?3, ?4)",
                params![reference, account_id, name, now],
            )?;

            if let Some(chat_scope) = chat_scope {
                conn.execute(
                    "INSERT OR IGNORE INTO aliases (ref, scope, scope_id, name, seen_at)
                     VALUES (?1, 'chat', ?2, ?3, ?4)",
                    params![reference, chat_scope, name, now],
                )?;
            }

            Ok(())
        })
        .await
    }

    /// The current display name for each of `refs` that has one; a ref with no
    /// recorded name is simply absent.
    pub async fn current_names(&self, refs: &[ActorRef]) -> Result<HashMap<ActorRef, String>> {
        let refs: Vec<String> = refs
            .iter()
            .map(|reference| reference.as_str().to_string())
            .collect();

        self.read(move |conn| {
            let mut names = HashMap::new();
            if refs.is_empty() {
                return Ok(names);
            }

            let placeholders = vec!["?"; refs.len()].join(", ");
            let sql = format!(
                "SELECT ref, current_name FROM channel_identities
                 WHERE current_name IS NOT NULL AND ref IN ({placeholders})"
            );

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(refs.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;

            for row in rows {
                let (reference, name) = row?;
                names.insert(ActorRef::new(reference), name);
            }

            Ok(names)
        })
        .await
    }

    /// Every name ever recorded for a ref, oldest first (its rename history).
    pub async fn aliases(&self, reference: &ActorRef) -> Result<Vec<String>> {
        let reference = reference.as_str().to_string();

        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT name FROM aliases WHERE ref = ?1 GROUP BY name ORDER BY MIN(rowid)",
            )?;
            let rows = stmt.query_map(params![reference], |row| row.get::<_, String>(0))?;

            let mut names = Vec::new();
            for row in rows {
                names.push(row?);
            }

            Ok(names)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;

    fn observation(reference: &str, name: Option<&str>) -> ObservedIdentity {
        ObservedIdentity {
            reference: ActorRef::new(reference),
            channel: Channel::Telegram,
            account_id: "default".to_string(),
            platform_id: "123456".to_string(),
            name: name.map(str::to_string),
            chat_scope: None,
        }
    }

    async fn memory_database() -> Database {
        Database::in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn observing_records_the_current_name() {
        let database = memory_database().await;

        database
            .observe_identity(&observation("member_1", Some("Ali")))
            .await
            .unwrap();

        assert_eq!(
            database
                .current_names(&[ActorRef::new("member_1")])
                .await
                .unwrap(),
            HashMap::from([(ActorRef::new("member_1"), "Ali".to_string())])
        );
    }

    #[tokio::test]
    async fn renaming_updates_the_current_name_and_keeps_the_old_one() {
        let database = memory_database().await;

        database
            .observe_identity(&observation("member_1", Some("Ali")))
            .await
            .unwrap();
        database
            .observe_identity(&observation("member_1", Some("Alina")))
            .await
            .unwrap();

        assert_eq!(
            database
                .current_names(&[ActorRef::new("member_1")])
                .await
                .unwrap(),
            HashMap::from([(ActorRef::new("member_1"), "Alina".to_string())])
        );
        assert_eq!(
            database.aliases(&ActorRef::new("member_1")).await.unwrap(),
            vec!["Ali".to_string(), "Alina".to_string()]
        );
    }

    #[tokio::test]
    async fn a_ref_without_a_name_is_absent_from_current_names() {
        let database = memory_database().await;

        database
            .observe_identity(&observation("member_x", None))
            .await
            .unwrap();

        assert!(
            database
                .current_names(&[ActorRef::new("member_x")])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            database
                .aliases(&ActorRef::new("member_x"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_nameless_observation_does_not_erase_a_known_name() {
        let database = memory_database().await;

        database
            .observe_identity(&observation("member_1", Some("Ali")))
            .await
            .unwrap();
        database
            .observe_identity(&observation("member_1", None))
            .await
            .unwrap();

        assert_eq!(
            database
                .current_names(&[ActorRef::new("member_1")])
                .await
                .unwrap(),
            HashMap::from([(ActorRef::new("member_1"), "Ali".to_string())])
        );
    }

    #[tokio::test]
    async fn current_names_only_returns_the_requested_refs() {
        let database = memory_database().await;

        database
            .observe_identity(&observation("member_1", Some("Ali")))
            .await
            .unwrap();
        database
            .observe_identity(&observation("member_2", Some("Bo")))
            .await
            .unwrap();

        let names = database
            .current_names(&[ActorRef::new("member_2"), ActorRef::new("member_3")])
            .await
            .unwrap();

        assert_eq!(
            names,
            HashMap::from([(ActorRef::new("member_2"), "Bo".to_string())])
        );
    }

    #[tokio::test]
    async fn an_empty_ref_list_yields_no_names() {
        let database = memory_database().await;

        assert!(database.current_names(&[]).await.unwrap().is_empty());
    }
}
