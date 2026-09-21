//! Shared bounded artifact observations for Machine CLI and public gRPC.
use crate::controller::CredentialCarrier;
use crate::event::ClientEventData;
use crate::interaction_payload::ChangeArtifactPayload;
use crate::ledger::ObservedLedger;
use crate::machine::MachineError;
use crate::snapshot::RunSnapshot;
use rusqlite::OptionalExtension as _;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub const MAX_CHUNK_BYTES: u32 = 1_048_576;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    FinalResponse,
    FileChangeDiff,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactVisibility {
    Observer,
    ControllerOnly,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArtifactReference {
    pub artifact_id: String,
    pub created_at: String,
    pub interaction_request_id: Option<Uuid>,
    pub kind: ArtifactKind,
    pub visibility: ArtifactVisibility,
    pub media_type: String,
    pub byte_length: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize)]
pub struct ArtifactMetadata {
    pub artifact: ArtifactReference,
    pub maximum_chunk_size: u32,
}
#[derive(Debug)]
pub struct ArtifactChunk {
    pub artifact_id: String,
    pub offset: u64,
    pub length: u32,
    pub data: Vec<u8>,
    pub metadata: ArtifactReference,
    pub eof: bool,
    pub total_byte_length: u64,
    pub sha256: String,
}
impl ArtifactReference {
    pub fn machine_value(&self, run_id: Uuid) -> Value {
        json!({"schema_version":1,"artifact_id":self.artifact_id,"run_id":run_id,"kind":self.kind,"visibility":self.visibility,"interaction_request_id":self.interaction_request_id,"media_type":self.media_type,"byte_length":self.byte_length,"sha256":self.sha256,"created_at":self.created_at,"retention":"run_lifetime","integrity":"verified"})
    }
}

fn invalid(invariant: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable observation violates its checked shape",
        false,
        json!({"invariant":invariant}),
    )
}

fn identity(value: &str, field: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|value| value.get_version_num() == 7)
        .ok_or_else(|| MachineError::invalid_argument(field, "identity must be UUIDv7"))
}

fn ledger(state_root: &Path, snapshot: &RunSnapshot) -> Result<ObservedLedger, MachineError> {
    let run_id = snapshot.manifest.run_id;
    let head = snapshot.stamp.run_state_revision;
    ObservedLedger::open_run(state_root, run_id, head)
}

fn digest(value: &str) -> Result<(), MachineError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("artifact or snapshot SHA-256"));
    }
    Ok(())
}
pub(crate) fn change_artifact(
    value: ChangeArtifactPayload,
) -> Result<ArtifactReference, MachineError> {
    digest(&value.sha256)?;
    if value.artifact_id.get_version_num() != 7
        || value.truncated
        || value.media_type != "text/x-diff"
        || !(65537..=8 * 1024 * 1024).contains(&value.byte_length)
    {
        return Err(invalid("bounded immutable file-change artifact"));
    }
    Ok(ArtifactReference {
        artifact_id: value.artifact_id.to_string(),
        created_at: String::new(),
        interaction_request_id: None,
        kind: ArtifactKind::FileChangeDiff,
        visibility: ArtifactVisibility::ControllerOnly,
        media_type: value.media_type,
        byte_length: value.byte_length,
        sha256: value.sha256,
    })
}

fn artifact_error(run_id: Uuid, id: Uuid, code: &str, message: &str) -> MachineError {
    MachineError::new(
        code,
        message,
        false,
        json!({"run_id":run_id,"artifact_id":id}),
    )
}
fn artifact_integrity(
    run_id: Uuid,
    reference: &ArtifactReference,
    observed: Option<String>,
) -> MachineError {
    MachineError::new(
        "ARTIFACT_INTEGRITY_FAILURE",
        "artifact bytes do not match their durable reference",
        false,
        json!({"run_id":run_id,"artifact_id":reference.artifact_id,"expected_sha256":reference.sha256,"observed_sha256":observed}),
    )
}

fn final_artifact(value: &Value, id: Uuid) -> Result<Option<ArtifactReference>, MachineError> {
    if value.get("kind").and_then(Value::as_str) != Some("artifact")
        || value.get("artifact_id").and_then(Value::as_str) != Some(id.to_string().as_str())
    {
        return Ok(None);
    }
    let bytes = value
        .get("byte_length")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("final artifact byte length"))?;
    let sha = value
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("final artifact digest"))?;
    digest(sha)?;
    if bytes > 32 * 1024 * 1024 {
        return Err(invalid("final artifact provider bound"));
    }
    Ok(Some(ArtifactReference {
        artifact_id: id.to_string(),
        created_at: String::new(),
        interaction_request_id: None,
        kind: ArtifactKind::FinalResponse,
        visibility: ArtifactVisibility::Observer,
        media_type: "text/markdown".to_owned(),
        byte_length: bytes,
        sha256: sha.to_owned(),
    }))
}

fn artifact_reference(
    state_root: &Path,
    records: &ObservedLedger,
    run_id: Uuid,
    id: Uuid,
) -> Result<ArtifactReference, MachineError> {
    let mut found: Option<ArtifactReference> = None;
    for record in records.records() {
        let candidate = if let Some(event) = record.client_projection() {
            if let ClientEventData::ResponseFinal(crate::event::ResponseEventPayload {
                response: crate::event::FinalResponse::Artifact { artifact },
            }) = &event.record.data
            {
                (artifact.artifact_id == id).then(|| ArtifactReference {
                    artifact_id: id.to_string(),
                    created_at: artifact.created_at.clone(),
                    interaction_request_id: None,
                    kind: ArtifactKind::FinalResponse,
                    visibility: ArtifactVisibility::Observer,
                    media_type: artifact.media_type.clone(),
                    byte_length: artifact.byte_length,
                    sha256: artifact.sha256.clone(),
                })
            } else {
                None
            }
        } else {
            None
        };
        let candidate = if candidate.is_some() {
            candidate
        } else {
            match record.kind() {
                crate::audit::AuditKind::TurnTerminal => {
                    let bytes = crate::jcs::canonicalize(record.payload())
                        .map_err(|_| invalid("terminal artifact record"))?;
                    let value: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| invalid("terminal artifact payload"))?;
                    final_artifact(&value["final_response"], id)?
                }
                crate::audit::AuditKind::ApprovalRequested => {
                    let bytes = crate::jcs::canonicalize(record.payload())
                        .map_err(|_| invalid("interaction artifact record"))?;
                    let value: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| invalid("interaction artifact payload"))?;
                    let normalized = &value["interaction"];
                    let artifact = &normalized["payload"]["change_artifact"];
                    if artifact["artifact_id"].as_str() == Some(id.to_string().as_str()) {
                        if normalized["kind"].as_str() != Some("file_change_approval")
                            || normalized["run_id"].as_str() != Some(run_id.to_string().as_str())
                            || normalized["request_id"]
                                .as_str()
                                .and_then(|value| Uuid::parse_str(value).ok())
                                .is_none_or(|id| id.get_version_num() != 7)
                        {
                            return Err(invalid("controller-only artifact interaction binding"));
                        }
                        Some(change_artifact(
                            serde_json::from_value(artifact.clone())
                                .map_err(|_| invalid("interaction artifact metadata"))?,
                        )?)
                    } else {
                        None
                    }
                }
                crate::audit::AuditKind::ClientEvent => {
                    let event = crate::event::from_payload(record.payload())
                        .map_err(|_| invalid("legacy client artifact event"))?;
                    match event.data {
                        ClientEventData::ResponseFinal(crate::event::ResponseEventPayload {
                            response: crate::event::FinalResponse::Artifact { artifact },
                        }) if artifact.artifact_id == id => Some(ArtifactReference {
                            artifact_id: id.to_string(),
                            created_at: artifact.created_at.clone(),
                            interaction_request_id: None,
                            kind: ArtifactKind::FinalResponse,
                            visibility: ArtifactVisibility::Observer,
                            media_type: artifact.media_type,
                            byte_length: artifact.byte_length,
                            sha256: artifact.sha256,
                        }),
                        _ => None,
                    }
                }
                _ => None,
            }
        };
        if let Some(mut candidate) = candidate {
            if candidate.created_at.is_empty() {
                candidate.created_at = record.timestamp().to_owned();
            }
            if candidate.visibility == ArtifactVisibility::ControllerOnly {
                let bytes = crate::jcs::canonicalize(record.payload())
                    .map_err(|_| invalid("interaction artifact record"))?;
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| invalid("interaction artifact payload"))?;
                candidate.interaction_request_id = value["interaction"]["request_id"]
                    .as_str()
                    .and_then(|id| Uuid::parse_str(id).ok());
            }
            // Repeated terminal references retain the first durable creation time.
            if let Some(prior) = &found {
                candidate.created_at = prior.created_at.clone();
            }
            if found.as_ref().is_some_and(|prior| prior != &candidate) {
                return Err(invalid("one immutable artifact classification and digest"));
            }
            found = Some(candidate);
        }
    }
    if let Some(found) = found {
        return Ok(found);
    }
    if let Some(projected) = specialist_result_reference(state_root, run_id, id)? {
        return Ok(projected);
    }
    Err(artifact_error(
        run_id,
        id,
        "ARTIFACT_NOT_FOUND",
        "artifact is not referenced by this Run",
    ))
}

fn specialist_result_reference(
    state_root: &Path,
    run_id: Uuid,
    id: Uuid,
) -> Result<Option<ArtifactReference>, MachineError> {
    let path = state_root.join("orchestration/orchestration.sqlite3");
    if !path.exists() {
        return Ok(None);
    }
    let connection = rusqlite::Connection::open(path)
        .map_err(|_| invalid("Specialist result artifact association"))?;
    let has_table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='brokered_result_publications')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| invalid("Specialist result artifact association"))?;
    if !has_table {
        return Ok(None);
    }
    let row = connection
        .query_row(
            "SELECT created_at,byte_length,result_sha256
             FROM brokered_result_publications
             WHERE primary_run_id=?1 AND artifact_id=?2 AND state='published'",
            [run_id.to_string(), id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|_| invalid("Specialist result artifact association"))?;
    row.map(|(created_at, byte_length, sha256)| {
        digest(&sha256)?;
        if byte_length > 32 * 1024 * 1024 {
            return Err(invalid("Specialist result artifact provider bound"));
        }
        Ok(ArtifactReference {
            artifact_id: id.to_string(),
            created_at,
            interaction_request_id: None,
            kind: ArtifactKind::FinalResponse,
            visibility: ArtifactVisibility::Observer,
            media_type: "text/plain; charset=utf-8".to_owned(),
            byte_length,
            sha256,
        })
    })
    .transpose()
}

fn authorize_artifact(
    state_root: &Path,
    snapshot: &RunSnapshot,
    id: Uuid,
    reference: &ArtifactReference,
    carrier: Option<&CredentialCarrier>,
) -> Result<(), MachineError> {
    match carrier {
        Some(carrier) => {
            snapshot.authorize_current_controller(state_root, carrier, "artifact.read")
        }
        None if reference.visibility == ArtifactVisibility::ControllerOnly => Err(artifact_error(
            snapshot.manifest.run_id,
            id,
            "INTERACTION_ARTIFACT_REQUIRES_CONTROLLER",
            "artifact requires the current Controller",
        )),
        None => Ok(()),
    }
}

#[derive(Eq, PartialEq)]
struct FileVersion {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl FileVersion {
    fn from(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

fn verified_artifact(
    state_root: &Path,
    run_id: Uuid,
    id: Uuid,
    reference: &ArtifactReference,
) -> Result<(std::fs::File, FileVersion), MachineError> {
    use sha2::Digest as _;
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let system = crate::darwin::DarwinSystem;
    let uid = system.current_uid();
    let failure = || artifact_integrity(run_id, reference, None);
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(state_root)
        .map_err(|_| failure())?;
    let check_directory = |directory: &std::fs::File| -> Result<(), MachineError> {
        let metadata = directory.metadata().map_err(|_| failure())?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
            return Err(failure());
        }
        Ok(())
    };
    check_directory(&directory)?;
    for component in [
        "runs".to_owned(),
        run_id.to_string(),
        "artifacts".to_owned(),
    ] {
        directory = system
            .openat_nofollow(&directory, std::ffi::OsStr::new(&component), true)
            .map_err(|_| failure())?;
        check_directory(&directory)?;
    }
    let mut file = system
        .openat_nofollow(
            &directory,
            std::ffi::OsStr::new(&format!("{id}.bin")),
            false,
        )
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                artifact_error(
                    run_id,
                    id,
                    "ARTIFACT_NOT_FOUND",
                    "artifact bytes are unavailable",
                )
            } else {
                failure()
            }
        })?;
    let metadata = file.metadata().map_err(|_| failure())?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() != reference.byte_length
        || metadata.len() > 32 * 1024 * 1024
    {
        return Err(failure());
    }
    let version = FileVersion::from(&metadata);
    let mut digest = sha2::Sha256::new();
    let mut buffer = [0_u8; 65536];
    let mut observed = 0_u64;
    loop {
        let length = file.read(&mut buffer).map_err(|_| failure())?;
        if length == 0 {
            break;
        }
        observed = observed.checked_add(length as u64).ok_or_else(failure)?;
        if observed > reference.byte_length {
            return Err(failure());
        }
        digest.update(&buffer[..length]);
    }
    let observed_sha = format!("{:x}", digest.finalize());
    if observed != reference.byte_length
        || observed_sha != reference.sha256
        || FileVersion::from(&file.metadata().map_err(|_| failure())?) != version
    {
        return Err(artifact_integrity(run_id, reference, Some(observed_sha)));
    }
    Ok((file, version))
}

pub fn metadata(
    state_root: &Path,
    snapshot: &RunSnapshot,
    id: &str,
    carrier: Option<&CredentialCarrier>,
) -> Result<ArtifactMetadata, MachineError> {
    let id = identity(id, "artifact_id")?;
    let reference = artifact_reference(
        state_root,
        &ledger(state_root, snapshot)?,
        snapshot.manifest.run_id,
        id,
    )?;
    authorize_artifact(state_root, snapshot, id, &reference, carrier)?;
    verified_artifact(state_root, snapshot.manifest.run_id, id, &reference)?;
    authorize_artifact(state_root, snapshot, id, &reference, carrier)?;
    Ok(ArtifactMetadata {
        artifact: reference,
        maximum_chunk_size: MAX_CHUNK_BYTES,
    })
}

pub fn chunk(
    state_root: &Path,
    snapshot: &RunSnapshot,
    id: &str,
    range: (u64, u32),
    carrier: Option<&CredentialCarrier>,
) -> Result<ArtifactChunk, MachineError> {
    let id = identity(id, "artifact_id")?;
    let run_id = snapshot.manifest.run_id;
    let reference = artifact_reference(state_root, &ledger(state_root, snapshot)?, run_id, id)?;
    authorize_artifact(state_root, snapshot, id, &reference, carrier)?;
    let (offset, _) = range;
    let data = read_artifact_range(state_root, run_id, id, &reference, range)?;
    let actual = data.len();
    authorize_artifact(state_root, snapshot, id, &reference, carrier)?;
    Ok(ArtifactChunk {
        artifact_id: id.to_string(),
        metadata: reference.clone(),
        offset,
        length: actual as u32,
        data,
        eof: offset + actual as u64 == reference.byte_length,
        total_byte_length: reference.byte_length,
        sha256: reference.sha256,
    })
}

fn read_artifact_range(
    state_root: &Path,
    run_id: Uuid,
    id: Uuid,
    reference: &ArtifactReference,
    range: (u64, u32),
) -> Result<Vec<u8>, MachineError> {
    use std::io::{Read as _, Seek as _};
    let (offset, length) = range;
    if length == 0 || length > MAX_CHUNK_BYTES || offset > reference.byte_length {
        return Err(MachineError::new(
            "ARTIFACT_RANGE_INVALID",
            "artifact byte range is invalid",
            false,
            json!({"run_id":run_id,"artifact_id":id,"offset":offset,"length":length,"total":reference.byte_length}),
        ));
    }
    let (mut file, version) = verified_artifact(state_root, run_id, id, reference)?;
    let actual = u64::from(length).min(reference.byte_length - offset) as usize;
    let mut data = vec![0_u8; actual];
    file.seek(std::io::SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(&mut data))
        .map_err(|_| artifact_integrity(run_id, reference, None))?;
    if FileVersion::from(
        &file
            .metadata()
            .map_err(|_| artifact_integrity(run_id, reference, None))?,
    ) != version
    {
        return Err(artifact_integrity(run_id, reference, None));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    struct ArtifactTree {
        root: std::path::PathBuf,
        run_id: Uuid,
        id: Uuid,
        reference: ArtifactReference,
    }
    impl ArtifactTree {
        fn new(bytes: &[u8]) -> Self {
            let root =
                std::env::temp_dir().join(format!("dolgorae-gateway-artifact-{}", Uuid::now_v7()));
            let run_id = Uuid::now_v7();
            let id = Uuid::now_v7();
            for directory in [
                &root,
                &root.join("runs"),
                &root.join("runs").join(run_id.to_string()),
                &root.join("runs").join(run_id.to_string()).join("artifacts"),
            ] {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(directory)
                    .unwrap();
            }
            let reference = ArtifactReference {
                artifact_id: id.to_string(),
                created_at: String::new(),
                interaction_request_id: None,
                kind: ArtifactKind::FinalResponse,
                visibility: ArtifactVisibility::Observer,
                media_type: "text/markdown".into(),
                byte_length: bytes.len() as u64,
                sha256: crate::jcs::sha256_hex(bytes),
            };
            let tree = Self {
                root,
                run_id,
                id,
                reference,
            };
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(tree.path())
                .unwrap()
                .write_all(bytes)
                .unwrap();
            tree
        }
        fn path(&self) -> std::path::PathBuf {
            self.root
                .join("runs")
                .join(self.run_id.to_string())
                .join("artifacts")
                .join(format!("{}.bin", self.id))
        }
        fn read(&self, range: (u64, u32)) -> Result<Vec<u8>, MachineError> {
            read_artifact_range(&self.root, self.run_id, self.id, &self.reference, range)
        }
    }
    impl Drop for ArtifactTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn artifact_reads_raw_byte_ranges_and_verifies_bytes_outside_the_requested_chunk() {
        let tree = ArtifactTree::new(&[0, 255, 240, 159, 166, 128, 7]);
        assert_eq!(tree.read((1, 3)).unwrap(), [255, 240, 159]);
        assert_eq!(tree.read((6, 9)).unwrap(), [7]);
        assert!(tree.read((7, 1)).unwrap().is_empty());
        for range in [(0, 0), (0, MAX_CHUNK_BYTES + 1), (8, 1), (u64::MAX, 1)] {
            assert_eq!(tree.read(range).unwrap_err().code, "ARTIFACT_RANGE_INVALID");
        }
        // Corrupt a byte outside the requested range: verification covers the full file.
        std::fs::write(tree.path(), [0, 255, 240, 159, 166, 128, 8]).unwrap();
        assert_eq!(
            tree.read((0, 1)).unwrap_err().code,
            "ARTIFACT_INTEGRITY_FAILURE"
        );
    }

    #[test]
    fn artifact_refuses_symlink_hardlink_and_nonprivate_files_without_exposing_paths() {
        let tree = ArtifactTree::new(b"private artifact");
        let path = tree.path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = tree.read((0, 1)).unwrap_err();
        assert_eq!(error.code, "ARTIFACT_INTEGRITY_FAILURE");
        assert!(
            !error
                .details
                .to_string()
                .contains(tree.root.to_str().unwrap())
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let outside = tree.root.join("outside");
        std::fs::hard_link(&path, &outside).unwrap();
        assert_eq!(
            tree.read((0, 1)).unwrap_err().code,
            "ARTIFACT_INTEGRITY_FAILURE"
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert_eq!(
            tree.read((0, 1)).unwrap_err().code,
            "ARTIFACT_INTEGRITY_FAILURE"
        );
    }

    #[test]
    fn published_specialist_result_is_a_primary_owned_artifact_projection() {
        let tree = ArtifactTree::new(b"specialist result");
        let orchestration = tree.root.join("orchestration");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&orchestration)
            .unwrap();
        let connection =
            rusqlite::Connection::open(orchestration.join("orchestration.sqlite3")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE brokered_result_publications(
                   primary_run_id TEXT NOT NULL,
                   artifact_id TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   byte_length INTEGER NOT NULL,
                   result_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL
                 ) STRICT;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO brokered_result_publications VALUES(?1,?2,?3,?4,?5,'published')",
                rusqlite::params![
                    tree.run_id.to_string(),
                    tree.id.to_string(),
                    "2026-09-22T00:00:00.000000Z",
                    tree.reference.byte_length,
                    tree.reference.sha256,
                ],
            )
            .unwrap();
        let reference = specialist_result_reference(&tree.root, tree.run_id, tree.id)
            .unwrap()
            .unwrap();
        assert_eq!(reference.artifact_id, tree.id.to_string());
        assert_eq!(reference.visibility, ArtifactVisibility::Observer);
        assert_eq!(reference.media_type, "text/plain; charset=utf-8");
        assert_eq!(
            tree.read((0, reference.byte_length as u32)).unwrap(),
            b"specialist result"
        );
    }
}
