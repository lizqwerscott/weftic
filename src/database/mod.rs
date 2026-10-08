use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use deadpool_sqlite::{Config, Hook, HookError, Manager, Pool, Runtime};
use rusqlite::Connection;

pub mod events;

const PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA busy_timeout = 5000;
    PRAGMA foreign_keys = ON;
    ";

pub struct Database {
    write_pool: Pool,
    read_pool: Pool,
}

impl Database {
    pub async fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        Self::from_config(Config::new(path.to_path_buf())).await
    }

    /// A shared-cache in-memory database: unlike a plain `:memory:`, every
    /// connection sees the same database, so the reader and writer pools agree.
    pub async fn in_memory() -> Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let uri = format!(
            "file:weftic_mem_{}_{n}?mode=memory&cache=shared",
            std::process::id()
        );

        Self::from_config(Config::new(uri)).await
    }

    async fn from_config(cfg: Config) -> Result<Self> {
        let database = Self {
            write_pool: build_pool(&cfg, 1)?,
            read_pool: build_pool(&cfg, 4)?,
        };

        database.init_schema().await?;

        Ok(database)
    }

    pub fn close(&self) {
        self.write_pool.close();
        self.read_pool.close();
    }

    /// Run a closure on the single writer connection, returning its result.
    async fn write<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.write_pool.get().await?;
        conn.interact(f)
            .await
            .map_err(|error| anyhow!("sqlite interact failed: {error}"))?
    }

    /// Run a closure on a pooled reader connection, returning its result.
    async fn read<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.read_pool.get().await?;
        conn.interact(f)
            .await
            .map_err(|error| anyhow!("sqlite interact failed: {error}"))?
    }
}

fn build_pool(cfg: &Config, max_size: usize) -> Result<Pool> {
    let pool = Pool::builder(Manager::from_config(cfg, Runtime::Tokio1))
        .max_size(max_size)
        .post_create(Hook::sync_fn(|conn, _metrics| {
            let guard = conn
                .lock()
                .map_err(|_| HookError::Message("sqlite connection mutex poisoned".into()))?;
            guard.execute_batch(PRAGMAS).map_err(HookError::Backend)
        }))
        .build()?;

    Ok(pool)
}
