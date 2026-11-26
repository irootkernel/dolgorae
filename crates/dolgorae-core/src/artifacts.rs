//! Immutable SHA-256 content-addressed artifact store.
//!
//! Blobs live under `<home_dir>/artifacts/<sha[0..2]>/<sha>` with `0700`
//! directories and `0600` files, written via streaming incremental SHA-256 into
//! a private temp file + `sync_all()` + atomic rename.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::CoreError;
use crate::roles::hex_lower;
use crate::types::ArtifactRef;
use crate::{MAX_ARTIFACT_BYTES, MAX_INLINE_BYTES, MAX_PAGE_BYTES};

const STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Content-addressed artifact store rooted at `<home_dir>/artifacts`.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    /// Initializes `<home_dir>/artifacts` and `.tmp` with `0700` permissions.
    pub fn open(home_dir: &Path) -> Result<Self, CoreError> {
        ensure_dir_0700(home_dir)?;
        let root = home_dir.join("artifacts");
        ensure_dir_0700(&root)?;
        ensure_dir_0700(&root.join(".tmp"))?;
        Ok(Self { root })
    }

    /// Streams an artifact from `reader`, computing SHA-256 incrementally and
    /// rejecting payloads larger than `MAX_ARTIFACT_BYTES` (64 MiB).
    pub fn put_reader<R: Read>(
        &self,
        media_type: &str,
        mut reader: R,
    ) -> Result<ArtifactRef, CoreError> {
        let tmp_dir = self.root.join(".tmp");
        ensure_dir_0700(&tmp_dir)?;
        let tmp_path = tmp_dir.join(format!("upload-{}", Uuid::now_v7()));

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)?;

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
        let mut total: u64 = 0;

        let write_result: Result<(), CoreError> = (|| {
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                total = total
                    .checked_add(n as u64)
                    .ok_or_else(|| CoreError::payload_too_large("artifact size overflow"))?;
                if total > MAX_ARTIFACT_BYTES as u64 {
                    return Err(CoreError::payload_too_large(format!(
                        "artifact exceeds maximum size of {MAX_ARTIFACT_BYTES} bytes"
                    )));
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n])?;
            }
            file.sync_all()?;
            Ok(())
        })();

        if let Err(err) = write_result {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        drop(file);

        let sha256 = hex_lower(&hasher.finalize());
        let final_path = self.blob_path(&sha256)?;
        if let Some(parent) = final_path.parent() {
            ensure_dir_0700(parent)?;
        }
        // Content-addressed: identical digest implies identical content.
        if final_path.exists() {
            let _ = fs::remove_file(&tmp_path);
        } else {
            fs::rename(&tmp_path, &final_path)?;
            let _ = fs::set_permissions(&final_path, fs::Permissions::from_mode(0o600));
        }

        Ok(ArtifactRef {
            artifact_id: format!("art-{sha256}"),
            sha256,
            media_type: media_type.to_owned(),
            byte_len: total,
        })
    }

    /// Convenience wrapper writing a byte slice via `put_reader`.
    pub fn put_bytes(&self, media_type: &str, bytes: &[u8]) -> Result<ArtifactRef, CoreError> {
        self.put_reader(media_type, bytes)
    }

    /// Spills text larger than `MAX_INLINE_BYTES` (256 KiB) to an immutable artifact.
    pub fn spill_inline_text(
        &self,
        text: String,
    ) -> Result<(Option<String>, Option<ArtifactRef>), CoreError> {
        if text.len() > MAX_INLINE_BYTES {
            let art = self.put_bytes("text/plain; charset=utf-8", text.as_bytes())?;
            Ok((None, Some(art)))
        } else {
            Ok((Some(text), None))
        }
    }

    /// Reads a bounded byte slice (capped at `MAX_PAGE_BYTES` = 4 MiB) from a stored blob.
    pub fn read_slice(
        &self,
        sha256: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<u8>, CoreError> {
        let path = self.blob_path(sha256)?;
        if !path.is_file() {
            return Err(CoreError::not_found(format!(
                "artifact blob '{sha256}' not found"
            )));
        }
        let capped = max_bytes.clamp(1, MAX_PAGE_BYTES);
        let mut file = fs::File::open(&path)?;
        let len = file.metadata()?.len();
        if offset >= len {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(offset))?;
        let to_read = usize::try_from((len - offset).min(capped as u64)).unwrap_or(capped);
        let mut out = vec![0u8; to_read];
        file.read_exact(&mut out)?;
        Ok(out)
    }

    /// Returns the filesystem path for a validated 64-char hex digest.
    pub fn blob_path(&self, sha256: &str) -> Result<PathBuf, CoreError> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(CoreError::bad_request(format!(
                "invalid artifact sha256 '{sha256}'"
            )));
        }
        Ok(self.root.join(&sha256[..2]).join(sha256))
    }
}

pub(crate) fn ensure_dir_0700(dir: &Path) -> Result<(), CoreError> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
