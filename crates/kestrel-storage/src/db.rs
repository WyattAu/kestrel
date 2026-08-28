//! Database bootstrap (ADR 0003, ADR 0009): dual `SQLite` databases with the
//! mandated pragmas, single-writer pools, append-only migrations.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use sqlx::{
    migrate::Migrator,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
    },
};

use crate::error::{StorageError, StorageResult};

/// Pool pair for one database file: one writer (max 1 connection), a small
/// bounded reader pool (WAL readers never block on the writer).
#[derive(Clone)]
pub struct DbPools {
    /// Single-writer pool (`max_connections = 1`, the `SQLite` discipline).
    pub write: SqlitePool,
    /// Reader pool.
    pub read: SqlitePool,
}

/// Both databases owned by `StorageService`.
#[derive(Clone)]
pub struct Databases {
    /// `cache.db` (rebuildable syncable metadata, ADR 0009).
    pub cache: DbPools,
    /// `data.db` (durable records, ADR 0009).
    pub data: DbPools,
}

impl Databases {
    /// Opens (creating if needed) both databases with pragmas
    /// (`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`,
    /// `busy_timeout=5s`), runs migrations, and returns the pools.
    ///
    /// # Errors
    /// IO/migration failures mapped per `docs/error-taxonomy.md`.
    pub async fn open(cache_path: &Path, data_path: &Path) -> StorageResult<Self> {
        if let Some(parent) = cache_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::BlobIo(e.to_string()))?;
        }
        if let Some(parent) = data_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::BlobIo(e.to_string()))?;
        }
        let cache = open_one(cache_path).await?;
        let data = open_one(data_path).await?;
        run_migrations(&cache, migrations_dir("cache")).await?;
        run_migrations(&data, migrations_dir("data")).await?;
        Ok(Self { cache, data })
    }

    /// `PRAGMA quick_check` on both files (schema.md §7 startup pass).
    ///
    /// # Errors
    /// Fails with the failing database name embedded.
    pub async fn integrity_check(&self) -> StorageResult<()> {
        for (name, pools) in [("cache", &self.cache), ("data", &self.data)] {
            let row: (String,) = sqlx::query_as("PRAGMA quick_check")
                .fetch_one(&pools.read)
                .await
                .map_err(|e| StorageError::Migration(format!("integrity {name}: {e}")))?;
            if row.0 != "ok" {
                return Err(StorageError::Migration(format!("{name}: {}", row.0)));
            }
        }
        Ok(())
    }

    /// Closes all pools (best effort; shutdown path).
    pub async fn close(&self) {
        self.cache.write.close().await;
        self.cache.read.close().await;
        self.data.write.close().await;
        self.data.read.close().await;
    }
}

async fn open_one(path: &Path) -> StorageResult<DbPools> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .pragma("wal_autocheckpoint", "1000");

    let write = SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts.clone())
        .await
        .map_err(|e| StorageError::Migration(format!("{}: {e}", path.display())))?;

    let read = SqlitePoolOptions::new()
        .min_connections(1)
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts.read_only(true))
        .await
        .map_err(|e| StorageError::Migration(format!("{}: {e}", path.display())))?;

    Ok(DbPools { write, read })
}

fn migrations_dir(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("migrations")
        .join(kind)
}

async fn run_migrations(pools: &DbPools, dir: PathBuf) -> StorageResult<()> {
    let migrator = Migrator::new(dir.as_path())
        .await
        .map_err(|e| StorageError::Migration(format!("{}: {e}", dir.display())))?;
    migrator
        .run(&pools.write)
        .await
        .map_err(|e| StorageError::Migration(e.to_string()))
}
