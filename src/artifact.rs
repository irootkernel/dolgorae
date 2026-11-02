//! Shared bounded artifact observations for Machine CLI and public gRPC.
use crate::controller::CredentialCarrier;
use crate::event::ClientEventData;
use crate::interaction_payload::ChangeArtifactPayload;
use crate::ledger::ObservedLedger;
use crate::machine::MachineError;
use crate::snapshot::RunSnapshot;
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
    UserInput,
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
    let accepted_operations = records
        .records()
        .iter()
        .filter(|record| record.kind() == crate::audit::AuditKind::TurnStarted)
        .map(|record| {
            let bytes = crate::jcs::canonicalize(record.payload())
                .map_err(|_| invalid("accepted Turn artifact binding"))?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("accepted Turn artifact binding"))?;
            value["operation_id"]
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or_else(|| invalid("accepted Turn operation identity"))
        })
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
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
                crate::audit::AuditKind::IdempotencyReserved => {
                    let bytes = crate::jcs::canonicalize(record.payload())
                        .map_err(|_| invalid("accepted input artifact record"))?;
                    let value: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| invalid("accepted input artifact payload"))?;
                    let operation_id = value["operation_id"]
                        .as_str()
                        .and_then(|value| Uuid::parse_str(value).ok());
                    if operation_id.is_some_and(|value| accepted_operations.contains(&value)) {
                        user_input_artifact(&value, id)?
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

fn user_input_artifact(
    intent: &Value,
    id: Uuid,
) -> Result<Option<ArtifactReference>, MachineError> {
    let content = &intent["accepted_input"]["content"];
    let storage = &content["storage"];
    if content["kind"].as_str() != Some("artifact")
        || storage["artifact_id"].as_str() != Some(id.to_string().as_str())
    {
        return Ok(None);
    }
    let created_at = storage["created_at"]
        .as_str()
        .filter(|value| crate::audit::is_microsecond_utc_timestamp(value))
        .ok_or_else(|| invalid("accepted input artifact creation time"))?;
    let byte_length = storage["byte_length"]
        .as_u64()
        .filter(|value| *value <= 8 * 1024 * 1024)
        .ok_or_else(|| invalid("accepted input artifact byte length"))?;
    let sha256 = storage["sha256"]
        .as_str()
        .ok_or_else(|| invalid("accepted input artifact digest"))?;
    digest(sha256)?;
    Ok(Some(ArtifactReference {
        artifact_id: id.to_string(),
        created_at: created_at.to_owned(),
        interaction_request_id: None,
        kind: ArtifactKind::UserInput,
        visibility: ArtifactVisibility::ControllerOnly,
        media_type: "text/plain; charset=utf-8".to_owned(),
        byte_length,
        sha256: sha256.to_owned(),
    }))
}

pub(crate) fn accepted_input_text(
    state_root: &Path,
    run_id: Uuid,
    artifact_id: Uuid,
    created_at: &str,
    byte_length: u64,
    sha256: &str,
) -> Result<String, MachineError> {
    let reference = ArtifactReference {
        artifact_id: artifact_id.to_string(),
        created_at: created_at.to_owned(),
        interaction_request_id: None,
        kind: ArtifactKind::UserInput,
        visibility: ArtifactVisibility::ControllerOnly,
        media_type: "text/plain; charset=utf-8".to_owned(),
        byte_length,
        sha256: sha256.to_owned(),
    };
    let length =
        u32::try_from(byte_length).map_err(|_| invalid("accepted input inline byte length"))?;
    let bytes = read_artifact_range(state_root, run_id, artifact_id, &reference, (0, length))?;
    String::from_utf8(bytes).map_err(|_| invalid("accepted input UTF-8"))
}

fn specialist_result_reference(
    state_root: &Path,
    run_id: Uuid,
    id: Uuid,
) -> Result<Option<ArtifactReference>, MachineError> {
    let path = crate::engagement::EngagementStore::workspace_database_path(state_root);
    if !path.exists() {
        return Ok(None);
    }
    let row = crate::orchestration::OrchestrationStore::open_observer(state_root)?
        .published_result_artifact(run_id, id)?;
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
            visibility: ArtifactVisibility::ControllerOnly,
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
    let normalized =
        if crate::engagement::EngagementStore::workspace_database_path(state_root).exists() {
            crate::orchestration::OrchestrationStore::open_observer(state_root)?
                .shared_result_artifact(run_id, id)
                .map_err(|error| {
                    if error.code == "RESULT_CONTENT_UNAVAILABLE" {
                        error
                    } else {
                        failure()
                    }
                })?
        } else {
            None
        };
    let mut file = if let Some(file) = normalized {
        file
    } else {
        system
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
            })?
    };
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
    let _content_pin = crate::orchestration::OrchestrationStore::result_reader_pin(state_root)?;
    let id = identity(id, "artifact_id")?;
    if let Some((reference, _)) = crate::result_component_artifact::read_bound(
        state_root,
        snapshot.manifest.run_id,
        id,
        carrier,
    )? {
        return Ok(ArtifactMetadata {
            artifact: reference,
            maximum_chunk_size: MAX_CHUNK_BYTES,
        });
    }
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
    let _content_pin = crate::orchestration::OrchestrationStore::result_reader_pin(state_root)?;
    let id = identity(id, "artifact_id")?;
    let run_id = snapshot.manifest.run_id;
    if let Some((reference, bytes)) =
        crate::result_component_artifact::read_bound(state_root, run_id, id, carrier)?
    {
        let (offset, length) = range;
        if length == 0 || length > MAX_CHUNK_BYTES || offset > reference.byte_length {
            return Err(MachineError::invalid_argument(
                "range",
                "artifact range exceeds its bounded length",
            ));
        }
        let end = offset
            .saturating_add(u64::from(length))
            .min(reference.byte_length);
        let data = bytes[offset as usize..end as usize].to_vec();
        return Ok(ArtifactChunk {
            artifact_id: id.to_string(),
            offset,
            length: data.len() as u32,
            data,
            eof: end == reference.byte_length,
            total_byte_length: reference.byte_length,
            sha256: reference.sha256.clone(),
            metadata: reference,
        });
    }
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

/// Called only after the orchestration service authenticates the source Primary.
pub(crate) fn primary_task_context(
    state_root: &Path,
    snapshot: &RunSnapshot,
    id: Uuid,
) -> Result<crate::orchestration::AcceptedTaskContext, MachineError> {
    let run_id = snapshot.manifest.run_id;
    let reference = artifact_reference(state_root, &ledger(state_root, snapshot)?, run_id, id)?;
    if reference.visibility == ArtifactVisibility::ControllerOnly
        && specialist_result_reference(state_root, run_id, id)?.as_ref() != Some(&reference)
    {
        authorize_artifact(state_root, snapshot, id, &reference, None)?;
    }
    if reference.byte_length == 0 || reference.byte_length > 262_144 {
        return Err(MachineError::invalid_argument(
            "context_refs",
            "each context artifact must contain 1 to 262144 UTF-8 bytes",
        ));
    }
    if !(reference.media_type.starts_with("text/") || reference.media_type == "application/json") {
        return Err(MachineError::invalid_argument(
            "context_refs",
            "context artifacts must use a textual media type",
        ));
    }
    let bytes = read_artifact_range(
        state_root,
        run_id,
        id,
        &reference,
        (0, reference.byte_length as u32),
    )?;
    let content = String::from_utf8(bytes).map_err(|_| {
        MachineError::invalid_argument("context_refs", "context artifact bytes must be valid UTF-8")
    })?;
    if content.contains('\0') {
        return Err(MachineError::invalid_argument(
            "context_refs",
            "context artifact text must not contain NUL",
        ));
    }
    Ok(crate::orchestration::AcceptedTaskContext {
        artifact_id: id,
        media_type: reference.media_type,
        byte_length: reference.byte_length,
        sha256: reference.sha256,
        content,
    })
}

fn read_artifact_range(
    state_root: &Path,
    run_id: Uuid,
    id: Uuid,
    reference: &ArtifactReference,
    range: (u64, u32),
) -> Result<Vec<u8>, MachineError> {
    let _content_pin = crate::orchestration::OrchestrationStore::result_reader_pin(state_root)?;
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

/// Serializes the logical per-Run quota check with artifact publication.
pub struct ArtifactQuotaLease {
    file: std::fs::File,
    _reservation: Option<RunQuotaSerializationLease>,
}

/// A protected allocator-Run lock usable before birth without creating a Run.
pub(crate) struct RunQuotaSerializationLease {
    state_root: std::path::PathBuf,
    run_id: Uuid,
    directory: std::path::PathBuf,
    file: std::fs::File,
}
impl RunQuotaSerializationLease {
    pub(crate) fn acquire(state_root: &Path, run_id: Uuid) -> Result<Self, MachineError> {
        use std::os::unix::fs::OpenOptionsExt as _;
        if run_id.get_version_num() != 7 {
            return Err(quota_integrity(
                "quota reservation requires an allocator Run identity",
            ));
        }
        let system = crate::darwin::DarwinSystem;
        crate::workspace::verify_secure_directory(state_root, system.current_uid())?;
        let directory = state_root.join("artifact-quota");
        match crate::workspace::create_directory(&directory, 0o700) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(quota_integrity("quota serialization directory unavailable")),
        }
        crate::workspace::verify_secure_directory(&directory, system.current_uid())?;
        let path = directory.join(format!("{run_id}.lock"));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| quota_integrity("quota serialization lock unavailable"))?;
        let started = std::time::Instant::now();
        loop {
            verify_quota_lock(&directory, &path, &file)?;
            match system.lock_exclusive_nonblocking(&file) {
                Ok(()) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    if started.elapsed() >= std::time::Duration::from_secs(5) {
                        return Err(MachineError::new(
                            "RUN_BUSY",
                            "Run quota reservation has another owner",
                            true,
                            json!({"run_id":run_id,"owner_kind":"attachment"}),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => {
                    return Err(quota_integrity(
                        "quota serialization lock acquisition failed",
                    ));
                }
            }
        }
        let lease = Self {
            state_root: state_root.to_owned(),
            run_id,
            directory,
            file,
        };
        lease.verify(state_root, run_id)?;
        Ok(lease)
    }
    pub(crate) fn verify(&self, state_root: &Path, run_id: Uuid) -> Result<(), MachineError> {
        if self.state_root != state_root || self.run_id != run_id {
            return Err(quota_integrity("quota serialization owner differs"));
        }
        crate::workspace::verify_secure_directory(
            state_root,
            crate::darwin::DarwinSystem.current_uid(),
        )?;
        verify_quota_lock(
            &self.directory,
            &self.directory.join(format!("{run_id}.lock")),
            &self.file,
        )
    }
}
impl Drop for RunQuotaSerializationLease {
    fn drop(&mut self) {
        let _ = crate::darwin::DarwinSystem.unlock(&self.file);
    }
}

fn run_quota_namespace(directory: &Path) -> Option<(&Path, Uuid)> {
    let run = directory.parent()?;
    let id = Uuid::parse_str(run.file_name()?.to_str()?).ok()?;
    let runs = run
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "runs"))?;
    Some((runs.parent()?, id))
}

impl ArtifactQuotaLease {
    pub(crate) fn verify_for(&self, directory: &Path) -> Result<(), MachineError> {
        if let Some(reservation) = &self._reservation {
            let (state, run) = run_quota_namespace(directory)
                .ok_or_else(|| quota_integrity("quota Run namespace changed"))?;
            reservation.verify(state, run)?;
        }
        verify_quota_lock(directory, &directory.join(".quota.lock"), &self.file)
    }
    pub(crate) fn acquire_for_run(directory: &Path, run_id: Uuid) -> Result<Self, MachineError> {
        Self::acquire(directory).map_err(|mut error| {
            if error.code == "RUN_BUSY" {
                error.retryable = true;
                error.details = json!({"run_id":run_id,"owner_kind":"attachment"});
            }
            error
        })
    }
    pub fn acquire(directory: &Path) -> Result<Self, MachineError> {
        let reservation = run_quota_namespace(directory)
            .map(|(state, run)| RunQuotaSerializationLease::acquire(state, run))
            .transpose()?;
        Self::acquire_physical(directory, reservation)
    }
    pub(crate) fn acquire_under_reservation(
        directory: &Path,
        reservation: &RunQuotaSerializationLease,
    ) -> Result<Self, MachineError> {
        let (state, run) = run_quota_namespace(directory)
            .ok_or_else(|| quota_integrity("quota reservation has no Run namespace"))?;
        reservation.verify(state, run)?;
        Self::acquire_physical(directory, None)
    }
    fn acquire_physical(
        directory: &Path,
        reservation: Option<RunQuotaSerializationLease>,
    ) -> Result<Self, MachineError> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let system = crate::darwin::DarwinSystem;
        crate::workspace::verify_secure_directory(directory, system.current_uid())?;
        let path = directory.join(".quota.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| quota_integrity("artifact quota lock unavailable"))?;
        let started = std::time::Instant::now();
        loop {
            verify_quota_lock(directory, &path, &file)?;
            match system.lock_exclusive_nonblocking(&file) {
                Ok(()) => {
                    if let Err(error) = verify_quota_lock(directory, &path, &file) {
                        let _ = system.unlock(&file);
                        return Err(error);
                    }
                    break;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    let Some(remaining) =
                        std::time::Duration::from_secs(5).checked_sub(started.elapsed())
                    else {
                        return Err(MachineError::new(
                            "RUN_BUSY",
                            "artifact publication quota has another owner",
                            false,
                            json!({}),
                        ));
                    };
                    std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
                }
                Err(_) => return Err(quota_integrity("artifact quota lock acquisition failed")),
            }
        }
        let lease = Self {
            file,
            _reservation: reservation,
        };
        lease.verify_for(directory)?;
        Ok(lease)
    }
}
fn verify_quota_lock(
    directory: &Path,
    path: &Path,
    file: &std::fs::File,
) -> Result<(), MachineError> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = crate::darwin::DarwinSystem.current_uid();
    crate::workspace::verify_secure_directory(directory, uid)
        .map_err(|_| quota_integrity("artifact quota directory identity is untrusted"))?;
    crate::workspace::verify_secure_file(path, uid)
        .map_err(|_| quota_integrity("artifact quota lock identity is untrusted"))?;
    let retained = file
        .metadata()
        .map_err(|_| quota_integrity("artifact quota lock identity unavailable"))?;
    let current = std::fs::symlink_metadata(path)
        .map_err(|_| quota_integrity("artifact quota lock identity unavailable"))?;
    if retained.dev() != current.dev()
        || retained.ino() != current.ino()
        || retained.nlink() != 1
        || retained.uid() != uid
        || retained.mode() & 0o777 != 0o600
    {
        return Err(quota_integrity(
            "artifact quota lock changed or has an untrusted alias",
        ));
    }
    Ok(())
}
impl Drop for ArtifactQuotaLease {
    fn drop(&mut self) {
        let _ = crate::darwin::DarwinSystem.unlock(&self.file);
    }
}
fn quota_integrity(reason: &str) -> MachineError {
    MachineError::new(
        "ARTIFACT_INTEGRITY_FAILURE",
        "artifact quota evidence is untrusted",
        false,
        json!({"invariant":reason}),
    )
}

pub const MAX_RUN_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
/// A fresh capacity observation; pointer bytes cannot reduce a retained body's
/// logical charge. Caller holds `ArtifactQuotaLease` through its publication.
pub fn logical_artifact_bytes(directory: &Path) -> Result<u64, MachineError> {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let uid = crate::darwin::DarwinSystem.current_uid();
    crate::workspace::verify_secure_directory(directory, uid)?;
    let mut normalized = BTreeMap::new();
    let mut capture_bytes = 0_u64;
    if let Some(run_root) = directory.parent()
        && let Some(run_id) = run_root
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| Uuid::parse_str(n).ok())
        && let Some(runs) = run_root
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "runs"))
        && let Some(state_root) = runs.parent()
    {
        let database = crate::engagement::EngagementStore::workspace_database_path(state_root);
        match std::fs::symlink_metadata(&database) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(quota_integrity(
                    "artifact publication namespace unavailable",
                ));
            }
            Ok(_) => {
                normalized = crate::orchestration::OrchestrationStore::open_observer(state_root)?
                    .logical_result_artifacts(run_id)?;
                let merge_bytes =
                    crate::delegation_service::research_merge::logical_bytes(state_root, run_id)?;
                capture_bytes = crate::group_quota::reserved_bytes(state_root, run_id)?
                    .checked_add(crate::result_component_artifact::logical_bytes(
                        state_root, run_id,
                    )?)
                    .and_then(|bytes| bytes.checked_add(merge_bytes))
                    .ok_or_else(|| quota_integrity("artifact logical byte sum overflow"))?;
            }
        }
    }
    let mut total = capture_bytes;
    for reference in normalized.values() {
        if !reference.retired {
            total = total
                .checked_add(reference.byte_length)
                .ok_or_else(|| quota_integrity("artifact logical byte sum overflow"))?;
        }
    }
    let mut seen = BTreeSet::new();
    for entry in std::fs::read_dir(directory)
        .map_err(|_| quota_integrity("artifact directory unavailable"))?
    {
        let entry = entry.map_err(|_| quota_integrity("artifact directory entry unavailable"))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| quota_integrity("artifact file name invalid"))?;
        if name == ".quota.lock" {
            continue;
        }
        let published = name
            .strip_suffix(".bin")
            .and_then(|n| Uuid::parse_str(n).ok())
            .filter(|id| id.get_version_num() == 7);
        let prepared = name
            .strip_prefix('.')
            .and_then(|n| Uuid::parse_str(n).ok())
            .filter(|id| id.get_version_num() == 7);
        if published.is_none() && prepared.is_none() {
            return Err(quota_integrity(
                "artifact namespace contains an unknown identity",
            ));
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(entry.path())
            .map_err(|_| quota_integrity("artifact capacity file unavailable"))?;
        let metadata = file
            .metadata()
            .map_err(|_| quota_integrity("artifact capacity identity unavailable"))?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() > 32 * 1024 * 1024
        {
            return Err(quota_integrity("artifact capacity file is untrusted"));
        }
        if let Some(id) = published
            && let Some(reference) = normalized.get(&id)
        {
            if !reference.normalized {
                if metadata.len() != reference.byte_length {
                    return Err(quota_integrity("legacy artifact length changed"));
                }
                seen.insert(id);
                continue;
            }
            if metadata.len() > 1024 {
                return Err(quota_integrity("artifact content reference exceeds bound"));
            }
            let before = FileVersion::from(&metadata);
            let mut bytes = Vec::new();
            file.by_ref()
                .take(1025)
                .read_to_end(&mut bytes)
                .map_err(|_| quota_integrity("artifact content reference unavailable"))?;
            let pointer: Value = serde_json::from_slice(&bytes)
                .map_err(|_| quota_integrity("artifact content reference malformed"))?;
            if pointer.as_object().is_none_or(|value| value.len() != 4)
                || pointer["schema"] != "dolgorae.native-result-content-reference/v1"
                || pointer["artifact_id"] != id.to_string()
                || pointer["sha256"] != reference.sha256
                || pointer["byte_length"] != reference.byte_length
                || FileVersion::from(&file.metadata().map_err(|_| {
                    quota_integrity("artifact content reference identity unavailable")
                })?) != before
            {
                return Err(quota_integrity("artifact content reference changed"));
            }
            seen.insert(id);
        } else {
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| quota_integrity("artifact logical byte sum overflow"))?;
        }
    }
    if normalized
        .iter()
        .any(|(id, reference)| reference.published && !seen.contains(id))
    {
        return Err(quota_integrity("published artifact reference is missing"));
    }
    Ok(total)
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
    fn a_base_only_engagement_database_does_not_hide_run_artifacts() {
        // `dolgorae init` now creates the generation-6 base database, so most
        // workspaces carry one without the orchestration extension.
        let tree = ArtifactTree::new(b"run artifact");
        let orchestration = tree.root.join("orchestration");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&orchestration)
            .unwrap();
        drop(
            crate::engagement::EngagementStore::open(&orchestration.join("orchestration.sqlite3"))
                .unwrap(),
        );
        assert_eq!(
            specialist_result_reference(&tree.root, tree.run_id, tree.id).unwrap(),
            None
        );
        assert_eq!(tree.read((0, 12)).unwrap(), b"run artifact");
    }

    #[test]
    fn published_specialist_result_is_a_primary_owned_artifact_projection() {
        let tree = ArtifactTree::new(b"specialist result");
        let orchestration = tree.root.join("orchestration");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&orchestration)
            .unwrap();
        // The fixture database must be a verified generation-6 database: every
        // opener gates on the accepted generation and the namespace record.
        let database = orchestration.join("orchestration.sqlite3");
        drop(crate::engagement::EngagementStore::open(&database).unwrap());
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE brokered_result_publications(
                   task_id TEXT PRIMARY KEY,
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
                "INSERT INTO brokered_result_publications(
                   task_id,primary_run_id,artifact_id,created_at,byte_length,result_sha256,state
                 ) VALUES(?1,?2,?3,?4,?5,?6,'published')",
                rusqlite::params![
                    Uuid::now_v7().to_string(),
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
        assert_eq!(reference.visibility, ArtifactVisibility::ControllerOnly);
        assert_eq!(reference.media_type, "text/plain; charset=utf-8");
        assert_eq!(
            tree.read((0, reference.byte_length as u32)).unwrap(),
            b"specialist result"
        );
    }
    #[test]
    fn waiting_quota_publisher_rejects_replaced_lock_identity() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let directory = std::env::temp_dir().join(format!(
            "dolgorae-artifact-quota-identity-{}",
            Uuid::now_v7()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let lease = ArtifactQuotaLease::acquire(&directory).unwrap();
        let contender_root = directory.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        let contender = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = ArtifactQuotaLease::acquire(&contender_root);
            completed_tx.send(()).unwrap();
            result
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            completed_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        std::fs::rename(directory.join(".quota.lock"), directory.join("old-lock")).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(".quota.lock"))
            .unwrap();
        assert_eq!(
            contender.join().unwrap().err().unwrap().code,
            "ARTIFACT_INTEGRITY_FAILURE"
        );
        drop(lease);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn logical_artifact_quota_counts_exact_limit_and_one_extra_byte() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let directory =
            std::env::temp_dir().join(format!("dolgorae-artifact-quota-{}", Uuid::now_v7()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let lease = ArtifactQuotaLease::acquire(&directory).unwrap();
        for _ in 0..8 {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(directory.join(format!("{}.bin", Uuid::now_v7())))
                .unwrap()
                .set_len(32 * 1024 * 1024)
                .unwrap();
        }
        assert_eq!(
            logical_artifact_bytes(&directory).unwrap(),
            MAX_RUN_ARTIFACT_BYTES
        );
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(format!(".{}", Uuid::now_v7())))
            .unwrap()
            .set_len(1)
            .unwrap();
        assert_eq!(
            logical_artifact_bytes(&directory).unwrap(),
            MAX_RUN_ARTIFACT_BYTES + 1
        );
        let run_id = Uuid::now_v7();
        let busy = ArtifactQuotaLease::acquire_for_run(&directory, run_id)
            .err()
            .unwrap();
        assert_eq!(busy.code, "RUN_BUSY");
        assert!(busy.retryable);
        assert_eq!(
            busy.details,
            json!({"run_id":run_id,"owner_kind":"attachment"})
        );
        drop(lease);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
