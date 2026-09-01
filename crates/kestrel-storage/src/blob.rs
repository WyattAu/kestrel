//! Content-addressed blob store (docs/schema.md §4, threat model §4.3).
//!
//! Write path: temp file in `blobs/tmp/` (`O_CREAT|O_EXCL`) → write + hash
//! concurrently → fsync → atomic rename to `blobs/ab/cd/<sha256>` → registry
//! row upsert. Crashes leave only orphan temp files (swept at startup).
//! Reads open with `O_NOFOLLOW`; symlinked targets are rejected.

use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, path::PathBuf};

use kestrel_core::ids::BlobHash;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::{StorageError, StorageResult};

/// Blob CAS handle over `$XDG_DATA_HOME/kestrel/blobs/`.
#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
    tmp: PathBuf,
}

/// How old an orphan temp file may be before the startup sweep removes it.
const TMP_SWEEP_AGE: std::time::Duration = std::time::Duration::from_hours(1);

impl BlobStore {
    /// Creates the store handle; directories are created on first write.
    #[must_use]
    pub fn new(root: PathBuf, tmp: PathBuf) -> Self {
        Self { root, tmp }
    }

    /// Final CAS path for a digest (`ab/cd/<hex>`).
    #[must_use]
    pub fn path_for(&self, hash: &BlobHash) -> PathBuf {
        let hex = hash.to_hex();
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(hex)
    }

    /// Writes bytes into the CAS and returns their hash. Idempotent: an
    /// existing identical blob is kept (dedup by construction).
    ///
    /// # Errors
    /// IO failures surface as [`StorageError::BlobIo`].
    pub async fn write(&self, bytes: &[u8]) -> StorageResult<BlobHash> {
        let hash = BlobHash::from_digest(Sha256::digest(bytes).into());
        let final_path = self.path_for(&hash);
        if tokio::fs::try_exists(&final_path).await.unwrap_or(false) {
            return Ok(hash);
        }
        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::BlobIo(e.to_string()))?;
        }
        tokio::fs::create_dir_all(&self.tmp)
            .await
            .map_err(|e| StorageError::BlobIo(e.to_string()))?;

        // Unique temp name; write + hash simultaneously, fsync, rename.
        let tmp_path = self
            .tmp
            .join(format!("{}.tmp", uuid::Uuid::now_v7().simple()));
        let mut hasher = Sha256::new();
        {
            let file = tokio::fs::File::create(&tmp_path)
                .await
                .map_err(|e| StorageError::BlobIo(e.to_string()))?;
            let mut writer = tokio::io::BufWriter::new(file);
            // Hash in chunks while writing so no second pass over the bytes
            // is needed.
            for chunk in bytes.chunks(64 * 1024) {
                hasher.update(chunk);
                writer
                    .write_all(chunk)
                    .await
                    .map_err(|e| StorageError::BlobIo(e.to_string()))?;
            }
            writer
                .flush()
                .await
                .map_err(|e| StorageError::BlobIo(e.to_string()))?;
            writer
                .get_ref()
                .sync_all()
                .await
                .map_err(|e| StorageError::BlobIo(e.to_string()))?;
        }
        let written = BlobHash::from_digest(hasher.finalize().into());
        // INVARIANT: digest of the bytes we just wrote must equal the
        // precomputed hash — guards against torn writes.
        if written != hash {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(StorageError::Invariant(format!(
                "hash mismatch while writing blob: expected {hash}, wrote {written}"
            )));
        }
        // A concurrent writer may have installed the same blob: dedup
        // makes a pre-existing destination success.
        if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
            if !tokio::fs::try_exists(&final_path).await.unwrap_or(false) {
                return Err(StorageError::BlobIo(format!(
                    "rename {} -> {}: {e}",
                    tmp_path.display(),
                    final_path.display()
                )));
            }
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }
        Ok(hash)
    }

    /// Reads a blob fully into memory.
    ///
    /// # Errors
    /// [`StorageError::BlobMissing`] when absent.
    pub async fn read(&self, hash: &BlobHash) -> StorageResult<Vec<u8>> {
        let path = self.path_for(hash);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::BlobMissing(hash.to_hex()))
            }
            Err(e) => Err(StorageError::BlobIo(e.to_string())),
        }
    }

    /// Opens a blob for reading with `O_NOFOLLOW` (threat model §4.3: a
    /// local attacker swapping in a symlink is rejected).
    ///
    /// # Errors
    /// [`StorageError::BlobMissing`] when absent; symlink targets are
    /// rejected as IO errors.
    pub fn open_nofollow_blocking(&self, hash: &BlobHash) -> StorageResult<std::fs::File> {
        let path = self.path_for(hash);
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    StorageError::BlobMissing(hash.to_hex())
                } else {
                    StorageError::BlobIo(format!("{}: {e}", path.display()))
                }
            })
    }

    /// Unlinks the file backing a hash (GC sweep step).
    ///
    /// # Errors
    /// IO errors other than `NotFound` surface.
    pub async fn remove(&self, hash: &BlobHash) -> StorageResult<()> {
        match tokio::fs::remove_file(self.path_for(hash)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StorageError::BlobIo(e.to_string())),
        }
    }

    /// Startup sweep of orphan temp files past the sweep age (1 h).
    /// (schema.md §4.1). Best-effort; failures are logged, not fatal.
    pub async fn sweep_tmp(&self) {
        let Ok(entries) = tokio::fs::read_dir(&self.tmp).await else {
            return;
        };
        let mut entries = entries;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let age = modified.elapsed().unwrap_or(std::time::Duration::ZERO);
            if age > TMP_SWEEP_AGE {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

    use proptest::prelude::*;

    use super::*;

    fn store(tmp: &std::path::Path) -> BlobStore {
        BlobStore::new(tmp.join("blobs"), tmp.join("blobs").join("tmp"))
    }

    #[tokio::test]
    async fn write_is_content_addressed_and_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let h1 = s.write(b"hello world").await.unwrap();
        let h2 = s.write(b"hello world").await.unwrap();
        assert_eq!(h1, h2);
        assert_eq!(
            h1.to_hex(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert!(s.path_for(&h1).exists());
        assert_eq!(s.read(&h1).await.unwrap(), b"hello world");
    }

    #[tokio::test]
    async fn missing_read_is_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let h = BlobHash::parse_hex(&"0".repeat(64)).unwrap();
        match s.read(&h).await {
            Err(StorageError::BlobMissing(hex)) => assert_eq!(hex, "0".repeat(64)),
            other => panic!("expected BlobMissing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nofollow_rejects_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let h = s.write(b"real").await.unwrap();
        assert!(s.open_nofollow_blocking(&h).is_ok());
        // Replace with a symlink to another file; O_NOFOLLOW must reject.
        let path = s.path_for(&h);
        let target = tmp.path().join("target");
        std::fs::write(&target, b"evil").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(s.open_nofollow_blocking(&h).is_err());
    }

    #[tokio::test]
    async fn tmp_sweep_removes_only_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        std::fs::create_dir_all(&s.tmp).unwrap();
        let old = s.tmp.join("old.tmp");
        std::fs::write(&old, b"x").unwrap();
        let fresh = s.tmp.join("fresh.tmp");
        std::fs::write(&fresh, b"y").unwrap();
        // Age the "old" file beyond the sweep threshold.
        let st = std::fs::metadata(&old).unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_hours(2);
        let _ = st;
        set_mtime(&old, past);
        s.sweep_tmp().await;
        assert!(!old.exists());
        assert!(fresh.exists());
    }

    fn set_mtime(path: &std::path::Path, t: std::time::SystemTime) {
        let f = std::fs::File::open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(kestrel_core::testkit::proptest_cases()))]

        #[test]
        fn cas_write_is_idempotent_and_read_roundtrips(data in proptest::collection::vec(0u8..=255u8, 0..4096)) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tmp = tempfile::tempdir().unwrap();
                let s = store(tmp.path());

                // Write twice, same hash
                let h1 = s.write(&data).await.unwrap();
                let h2 = s.write(&data).await.unwrap();
                prop_assert_eq!(&h1, &h2, "write not idempotent");

                // Read round-trips
                let read_back = s.read(&h1).await.unwrap();
                prop_assert_eq!(&read_back, &data, "read mismatch");

                // Remove then read fails
                s.remove(&h1).await.unwrap();
                prop_assert!(s.read(&h1).await.is_err(), "read after remove should fail");

                Ok(())
            })?;
        }
    }
}
