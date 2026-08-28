//! Blob/CAS operations, two-phase GC (schema.md §4.3), and settings.

use kestrel_core::ids::BlobHash;

use crate::{error::StorageResult, ops::Store};

/// Blob + GC operations extension.
pub(crate) trait StoreBlobsExt {
    /// Writes bytes into the CAS.
    fn write_blob(&self, bytes: &[u8]) -> impl Future<Output = StorageResult<BlobHash>>;
    /// Reads a blob from the CAS.
    fn read_blob(&self, hash: &BlobHash) -> impl Future<Output = StorageResult<Vec<u8>>>;
    /// Cross-DB outbox ref adjustment (code-side, ADR 0009).
    fn adjust_outbox_ref_inner(
        &self,
        hash: &BlobHash,
        delta: i64,
    ) -> impl Future<Output = StorageResult<()>>;
    /// GC mark: stamp unreferenced blobs.
    fn gc_mark(&self, now: i64) -> impl Future<Output = StorageResult<u64>>;
    /// GC sweep: drop registry rows (and files) whose mark expired.
    fn gc_sweep(&self, now: i64, grace_ms: i64) -> impl Future<Output = StorageResult<u64>>;
    /// Reads a data.db setting.
    fn get_setting(&self, key: &str) -> impl Future<Output = StorageResult<Option<String>>>;
    /// Writes a data.db setting.
    fn set_setting(&self, key: &str, value: &str) -> impl Future<Output = StorageResult<()>>;
}

/// settings row shape.
#[derive(sqlx::FromRow)]
struct SettingRow {
    value: String,
}

impl StoreBlobsExt for Store {
    async fn write_blob(&self, bytes: &[u8]) -> StorageResult<BlobHash> {
        self.blobs.write(bytes).await
    }

    async fn read_blob(&self, hash: &BlobHash) -> StorageResult<Vec<u8>> {
        self.blobs.read(hash).await
    }

    async fn adjust_outbox_ref_inner(&self, hash: &BlobHash, delta: i64) -> StorageResult<()> {
        // Registry row lives in cache.db; the outbox reference cannot use
        // triggers across files (ADR 0009).
        sqlx::query!(
            "INSERT INTO blobs (sha256, byte_size, refcount, created_at)
             VALUES (?1, 0, MAX(?2, 0), ?3)
             ON CONFLICT(sha256) DO UPDATE SET refcount = MAX(refcount + ?2, 0)",
            hash.to_hex(),
            delta,
            self.clock.now_unix_ms()
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(())
    }

    async fn gc_mark(&self, now: i64) -> StorageResult<u64> {
        let result = sqlx::query!(
            "UPDATE blobs SET last_gc_at = ?1 WHERE refcount = 0 AND last_gc_at IS NULL",
            now
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(result.rows_affected())
    }

    async fn gc_sweep(&self, now: i64, grace_ms: i64) -> StorageResult<u64> {
        // Claim rows atomically (refcount still 0 after the grace window),
        // then unlink files; a crash in between leaves orphan files that the
        // startup sweep collects (registry row is already gone).
        let rows: Vec<(String,)> = sqlx::query_as(
            "DELETE FROM blobs
             WHERE refcount = 0 AND last_gc_at IS NOT NULL AND last_gc_at <= ?1
             RETURNING sha256",
        )
        .bind(now - grace_ms)
        .fetch_all(&self.db.cache.write)
        .await?;
        for (hex,) in &rows {
            if let Some(hash) = BlobHash::parse_hex(hex) {
                let _ = self.blobs.remove(&hash).await;
            }
        }
        Ok(rows.len() as u64)
    }

    async fn get_setting(&self, key: &str) -> StorageResult<Option<String>> {
        let row = sqlx::query_as!(SettingRow, "SELECT value FROM settings WHERE key = ?1", key)
            .fetch_optional(&self.db.data.read)
            .await?;
        Ok(row.map(|r| r.value))
    }

    async fn set_setting(&self, key: &str, value: &str) -> StorageResult<()> {
        sqlx::query!(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            key,
            value
        )
        .execute(&self.db.data.write)
        .await?;
        Ok(())
    }
}
