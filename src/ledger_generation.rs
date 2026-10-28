//! Immutable history generations. Selection is read once; old files are retained.
use crate::audit::{AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
use crate::darwin::DarwinSystem;
use crate::jcs::sha256_hex;
use crate::ledger::{LedgerError, MAX_LEDGER_BYTES, MAX_LEDGER_RECORDS, RunStateProjection};
use crate::workspace::{sync_directory, verify_secure_directory, verify_secure_file};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEGMENTS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Content {
    path: String,
    byte_length: u64,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Segment {
    content: Content,
    first_sequence: u64,
    last_sequence: u64,
    previous_chain_hash: String,
    terminal_chain_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    schema: String,
    run_id: Uuid,
    projection: RunStateProjection,
    segments: Vec<Segment>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    schema: String,
    pub generation: u64,
    pub run_id: Uuid,
    checkpoint: Content,
    receipt_index: Content,
    pub append_path: String,
    pub durable_watermark: u64,
    pub chain_hash: String,
    pub finalization_reserved_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    schema: String,
    manifest: Content,
}

/// Held before audit flock. An exclusive lease closes workspace ledger admission.
pub(crate) struct WorkspaceLease(File);
impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        let _ = DarwinSystem.unlock(&self.0);
    }
}
impl WorkspaceLease {
    pub(crate) fn acquire(state_root: &Path, exclusive: bool) -> Result<Self, LedgerError> {
        let uid = DarwinSystem.current_uid();
        for path in [
            state_root.to_path_buf(),
            state_root.join("runtime"),
            state_root.join("runtime/locks"),
        ] {
            verify_secure_directory(&path, uid).map_err(LedgerError::SecurityPolicy)?;
        }
        let path = state_root.join("runtime/locks/ledger-maintenance.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        verify_secure_file(&path, uid).map_err(LedgerError::SecurityPolicy)?;
        let result = if exclusive {
            DarwinSystem.lock_exclusive_nonblocking(&file)
        } else {
            DarwinSystem.lock_shared_nonblocking(&file)
        };
        result.map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                LedgerError::WriterBusy(path.clone())
            } else {
                LedgerError::Io(error)
            }
        })?;
        let lease = Self(file);
        if !exclusive
            && crate::maintenance::workspace_maintenance_pending(state_root)
                .map_err(LedgerError::SecurityPolicy)?
        {
            return Err(LedgerError::WriterBusy(path));
        }
        Ok(lease)
    }
    pub(crate) fn for_run(root: &Path) -> Result<Option<Self>, LedgerError> {
        if root
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "runs")
        {
            let state_root = root
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| integrity("missing workspace root"))?;
            match fs::symlink_metadata(state_root.join("workspace.json")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
                Ok(_) => verify_secure_file(
                    &state_root.join("workspace.json"),
                    DarwinSystem.current_uid(),
                )
                .map_err(LedgerError::SecurityPolicy)?,
            }
            return Self::acquire(state_root, false).map(Some);
        }
        Ok(None)
    }
}

pub(crate) struct PinnedGeneration {
    pub manifest: Option<Manifest>,
    pub historical: Vec<File>,
    pub append_path: PathBuf,
    pub checkpoint_projection: Option<RunStateProjection>,
    segments: Vec<Segment>,
    pub base_bytes: u64,
    pub observation_paths: Vec<PathBuf>,
}

/// Checks only selection/manifest identity, never accepts a historical prefix.
/// The terminal deletion escape uses this restricted pin when history is corrupt.
pub(crate) fn selected_terminal_record_path(
    root: &Path,
    run_id: Uuid,
) -> Result<(PathBuf, Option<String>), LedgerError> {
    match selected_manifest(root, run_id)? {
        Some((selection, manifest)) => {
            let mut path = checked_path(root, &manifest.append_path)?;
            if secure_open(&path)?.metadata()?.len() == 0 {
                let checkpoint: Checkpoint =
                    read_content_json(root, &manifest.checkpoint, MAX_METADATA_BYTES)?;
                let final_segment = checkpoint
                    .segments
                    .last()
                    .ok_or_else(|| integrity("empty checkpoint lacks terminal history"))?;
                if checkpoint.schema != "dolgorae.native-ledger-checkpoint/v1"
                    || checkpoint.run_id != run_id
                    || checkpoint.projection.ledger_head.sequence != manifest.durable_watermark
                    || checkpoint.projection.ledger_head.hash != manifest.chain_hash
                    || final_segment.last_sequence != manifest.durable_watermark
                    || final_segment.terminal_chain_hash != manifest.chain_hash
                {
                    return Err(integrity(
                        "terminal history reference disagrees with checkpoint",
                    ));
                }
                path = checked_path(root, &final_segment.content.path)?;
            }
            verify_secure_file(&path, DarwinSystem.current_uid())
                .map_err(LedgerError::SecurityPolicy)?;
            Ok((
                path,
                Some(sha256_hex(
                    &serde_json::to_vec(&selection).map_err(|e| integrity(e.to_string()))?,
                )),
            ))
        }
        None => Ok((root.join("audit.jsonl"), None)),
    }
}

fn checked_manifest_using(
    root: &Path,
    run_id: Uuid,
    selection: &Selection,
    budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<Manifest, LedgerError> {
    if selection.schema != "dolgorae.native-ledger-selection/v1" {
        return Err(integrity("unsupported ledger selection"));
    }
    let manifest: Manifest =
        read_content_json_using(root, &selection.manifest, MAX_METADATA_BYTES, budget)?;
    if manifest.schema != "dolgorae.native-ledger-generation/v1"
        || manifest.run_id != run_id
        || manifest.generation < 2
        || manifest.finalization_reserved_bytes != crate::ledger::FINALIZATION_RESERVED_BYTES
    {
        return Err(integrity("invalid selected generation identity"));
    }
    Ok(manifest)
}

fn selected_manifest(
    root: &Path,
    run_id: Uuid,
) -> Result<Option<(Selection, Manifest)>, LedgerError> {
    selected_manifest_using(root, run_id, None)
}
fn selected_manifest_using(
    root: &Path,
    run_id: Uuid,
    mut budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<Option<(Selection, Manifest)>, LedgerError> {
    let selection = match read_json_using::<Selection>(
        &root.join("ledger-selection.json"),
        MAX_METADATA_BYTES,
        budget.as_deref_mut(),
    ) {
        Err(LedgerError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
        other => Some(other?),
    };
    let selected = selection
        .map(|s| checked_manifest_using(root, run_id, &s, budget.as_deref_mut()).map(|m| (s, m)))
        .transpose()?;
    let generations = root.join("ledger-generations");
    let entries = match fs::read_dir(&generations) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(selected),
        other => other?,
    };
    verify_secure_directory(&generations, DarwinSystem.current_uid())
        .map_err(LedgerError::SecurityPolicy)?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_SEGMENTS {
            return Err(integrity("retained generation namespace exceeds its bound"));
        }
        let path = entry?.path();
        verify_secure_directory(&path, DarwinSystem.current_uid())
            .map_err(LedgerError::SecurityPolicy)?;
        match read_json_using::<Selection>(
            &path.join("selection.json"),
            MAX_METADATA_BYTES,
            budget.as_deref_mut(),
        ) {
            Ok(witness) => {
                let activated =
                    checked_manifest_using(root, run_id, &witness, budget.as_deref_mut())?;
                if selected.as_ref().is_none_or(|(s, m)| {
                    m.generation < activated.generation
                        || (m.generation == activated.generation && *s != witness)
                }) {
                    return Err(integrity(
                        "selected generation is missing or older than retained activation",
                    ));
                }
            }
            Err(LedgerError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                // Prepared append files are empty. A nonempty one requires its activation proof.
                match secure_open(&path.join("append.jsonl")) {
                    Ok(file) if file.metadata()?.len() != 0 => {
                        return Err(integrity("append history lacks its activation witness"));
                    }
                    Ok(_) => {}
                    Err(LedgerError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(selected)
}

/// Metadata fence for display reuse. This does not prove accepted-operation absence.
/// Every retained activation witness and selected content file participates, so
/// deleting a selector or modifying a checkpoint cannot reuse an older prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObservationFileIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub uid: u32,
    pub links: u64,
    pub length: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
}

pub(crate) fn observation_file_identity(
    path: &Path,
) -> Result<ObservationFileIdentity, LedgerError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        verify_secure_directory(path, DarwinSystem.current_uid())
            .map_err(LedgerError::SecurityPolicy)?;
    } else {
        verify_secure_file(path, DarwinSystem.current_uid())
            .map_err(LedgerError::SecurityPolicy)?;
    }
    Ok(ObservationFileIdentity {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        links: metadata.nlink(),
        length: if metadata.is_dir() { 0 } else { metadata.len() },
        modified: if metadata.is_dir() {
            (0, 0)
        } else {
            (metadata.mtime(), metadata.mtime_nsec())
        },
        changed: if metadata.is_dir() {
            (0, 0)
        } else {
            (metadata.ctime(), metadata.ctime_nsec())
        },
    })
}
pub(crate) fn observation_identity(
    root: &Path,
    referenced: &[PathBuf],
) -> Result<Vec<ObservationFileIdentity>, LedgerError> {
    observation_identity_using(root, referenced, None)
}
pub(crate) fn observation_identity_using(
    root: &Path,
    referenced: &[PathBuf],
    mut budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<Vec<ObservationFileIdentity>, LedgerError> {
    let mut sources = vec![
        observation_file_identity(root)?,
        observation_file_identity(&root.join("audit.jsonl"))?,
    ];
    match observation_file_identity(&root.join("ledger-selection.json")) {
        Ok(source) => sources.push(source),
        Err(LedgerError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let namespace = root.join("ledger-generations");
    match fs::read_dir(&namespace) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(entries) => {
            sources.push(observation_file_identity(&namespace)?);
            for (index, entry) in entries.enumerate() {
                if index >= MAX_SEGMENTS {
                    return Err(integrity("retained generation namespace exceeds its bound"));
                }
                let directory = entry?.path();
                verify_secure_directory(&directory, DarwinSystem.current_uid())
                    .map_err(LedgerError::SecurityPolicy)?;
                sources.push(observation_file_identity(&directory)?);
                match read_json_using::<Selection>(
                    &directory.join("selection.json"),
                    MAX_METADATA_BYTES,
                    budget.as_deref_mut(),
                ) {
                    Ok(witness) => sources.push(observation_file_identity(&checked_path(
                        root,
                        &witness.manifest.path,
                    )?)?),
                    Err(LedgerError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                for (index, file) in fs::read_dir(&directory)?.enumerate() {
                    if index >= 16 {
                        return Err(integrity("generation file namespace exceeds its bound"));
                    }
                    sources.push(observation_file_identity(&file?.path())?);
                }
            }
        }
    }
    for path in referenced {
        sources.push(observation_file_identity(path)?);
        let mut ancestor = path.parent();
        while let Some(directory) = ancestor {
            verify_secure_directory(directory, DarwinSystem.current_uid())
                .map_err(LedgerError::SecurityPolicy)?;
            sources.push(observation_file_identity(directory)?);
            if directory == root {
                break;
            }
            ancestor = directory.parent();
        }
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    sources.dedup_by(|a, b| a.path == b.path);
    Ok(sources)
}

/// Caller owns the audit writer lock; retain activation before its first append.
pub(crate) fn retain_activation(root: &Path, run_id: Uuid) -> Result<(), LedgerError> {
    if let Some((selection, manifest)) = selected_manifest(root, run_id)? {
        let parent = Path::new(&manifest.append_path)
            .parent()
            .ok_or_else(|| integrity("missing append parent"))?;
        let relative = parent.join("selection.json").to_string_lossy().into_owned();
        create_json(root, relative, &selection)?;
        sync_directory(
            checked_path(root, &manifest.append_path)?
                .parent()
                .expect("checked append parent"),
        )?;
    }
    Ok(())
}

/// Metadata provenance only; this does not prove ledger history or absence.
pub(crate) fn observed_generation(root: &Path, run_id: Uuid) -> Result<u64, LedgerError> {
    Ok(selected_manifest(root, run_id)?.map_or(1, |(_, manifest)| manifest.generation))
}

pub(crate) fn observation_selection(
    root: &Path,
    run_id: Uuid,
    budget: &mut crate::ledger::ObservationBudget,
) -> Result<(u64, String, Vec<ObservationFileIdentity>), LedgerError> {
    let selected = selected_manifest_using(root, run_id, Some(budget))?;
    let generation = selected
        .as_ref()
        .map_or(1, |(_, manifest)| manifest.generation);
    let identity =
        sha256_hex(&serde_json::to_vec(&selected).map_err(|e| integrity(e.to_string()))?);
    let mut referenced = Vec::new();
    if let Some((selection, manifest)) = &selected {
        referenced.extend([
            checked_path(root, &selection.manifest.path)?,
            checked_path(root, &manifest.checkpoint.path)?,
            checked_path(root, &manifest.receipt_index.path)?,
            checked_path(root, &manifest.append_path)?,
        ]);
        let checkpoint: Checkpoint =
            read_content_json_using(root, &manifest.checkpoint, MAX_METADATA_BYTES, Some(budget))?;
        referenced.extend(
            checkpoint
                .segments
                .iter()
                .map(|segment| checked_path(root, &segment.content.path))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    let sources = observation_identity_using(root, &referenced, Some(budget))?;
    budget.check()?;
    Ok((generation, identity, sources))
}

pub(crate) fn pin(root: &Path, run_id: Uuid) -> Result<PinnedGeneration, LedgerError> {
    pin_using(root, run_id, None)
}
pub(crate) fn pin_using(
    root: &Path,
    run_id: Uuid,
    mut budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<PinnedGeneration, LedgerError> {
    let Some((selection, manifest)) = selected_manifest_using(root, run_id, budget.as_deref_mut())?
    else {
        return Ok(PinnedGeneration {
            manifest: None,
            historical: Vec::new(),
            append_path: root.join("audit.jsonl"),
            checkpoint_projection: None,
            segments: Vec::new(),
            base_bytes: 0,
            observation_paths: Vec::new(),
        });
    };
    let checkpoint: Checkpoint = read_content_json_using(
        root,
        &manifest.checkpoint,
        MAX_METADATA_BYTES,
        budget.as_deref_mut(),
    )?;
    if checkpoint.schema != "dolgorae.native-ledger-checkpoint/v1"
        || checkpoint.run_id != run_id
        || checkpoint.segments.len() > MAX_SEGMENTS
        || checkpoint.projection.ledger_head.sequence != manifest.durable_watermark
        || checkpoint.projection.ledger_head.hash != manifest.chain_hash
    {
        return Err(integrity("checkpoint does not match the selected manifest"));
    }
    let index: Vec<ReceiptIndexEntry> = read_content_json_using(
        root,
        &manifest.receipt_index,
        MAX_LEDGER_BYTES,
        budget.as_deref_mut(),
    )?;
    let mut expected_sequence = 1;
    let mut previous = GENESIS_PREVIOUS_HASH.to_owned();
    let mut historical = Vec::new();
    for segment in &checkpoint.segments {
        if segment.first_sequence != expected_sequence
            || segment.last_sequence < segment.first_sequence
            || segment.previous_chain_hash != previous
            || segment.last_sequence - segment.first_sequence >= MAX_LEDGER_RECORDS as u64
        {
            return Err(integrity("checkpoint segment chain is discontinuous"));
        }
        let mut file = open_content_using(
            root,
            &segment.content,
            MAX_LEDGER_BYTES,
            budget.as_deref_mut(),
        )?;
        if segment.content.byte_length == 0 {
            return Err(integrity("empty immutable history segment"));
        }
        file.seek(SeekFrom::End(-1))?;
        let mut final_byte = [0];
        if let Some(budget) = budget.as_deref_mut() {
            budget.charge_bytes(1)?;
        }
        file.read_exact(&mut final_byte)?;
        if final_byte[0] != b'\n' {
            return Err(integrity("immutable history segment has a torn tail"));
        }
        file.seek(SeekFrom::Start(0))?;
        historical.push(file);
        expected_sequence = segment
            .last_sequence
            .checked_add(1)
            .ok_or_else(|| integrity("sequence overflow"))?;
        previous.clone_from(&segment.terminal_chain_hash);
    }
    if expected_sequence - 1 != manifest.durable_watermark
        || previous != manifest.chain_hash
        || index
            .iter()
            .any(|entry| entry.sequence > manifest.durable_watermark || entry.sequence == 0)
    {
        return Err(integrity(
            "checkpoint watermark or receipt index is inconsistent",
        ));
    }
    let base_bytes = manifest
        .checkpoint
        .byte_length
        .saturating_add(manifest.receipt_index.byte_length)
        .saturating_add(selection.manifest.byte_length)
        .saturating_add(
            2 * serde_json::to_vec(&selection)
                .map_err(|e| integrity(e.to_string()))?
                .len() as u64,
        );
    if base_bytes > MAX_LEDGER_BYTES {
        return Err(LedgerError::Capacity(
            "selected generation metadata exceeds byte bound".to_owned(),
        ));
    }
    let append_path = checked_path(root, &manifest.append_path)?;
    verify_secure_file(&append_path, DarwinSystem.current_uid())
        .map_err(LedgerError::SecurityPolicy)?;
    let mut observation_paths = vec![
        checked_path(root, &selection.manifest.path)?,
        checked_path(root, &manifest.checkpoint.path)?,
        checked_path(root, &manifest.receipt_index.path)?,
        append_path.clone(),
    ];
    for segment in &checkpoint.segments {
        observation_paths.push(checked_path(root, &segment.content.path)?);
    }
    Ok(PinnedGeneration {
        observation_paths,
        manifest: Some(manifest),
        historical,
        append_path,
        checkpoint_projection: Some(checkpoint.projection),
        segments: checkpoint.segments,
        base_bytes,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptIndexEntry {
    sequence: u64,
    hash: String,
    kind: AuditKind,
}
fn receipt_index(records: &[AuditRecord]) -> Vec<ReceiptIndexEntry> {
    records
        .iter()
        .filter(|record| {
            matches!(
                record.kind(),
                AuditKind::RunCreated
                    | AuditKind::WriteContinuationCreated
                    | AuditKind::TurnIntent
                    | AuditKind::TurnStarted
                    | AuditKind::TurnTerminal
                    | AuditKind::MutationAdmitted
                    | AuditKind::MutationCompleted
                    | AuditKind::IdempotencyReserved
                    | AuditKind::Reconciliation
                    | AuditKind::OutcomeUnknown
            )
        })
        .map(|record| ReceiptIndexEntry {
            sequence: record.sequence(),
            hash: record.hash().to_owned(),
            kind: record.kind(),
        })
        .collect()
}

pub(crate) fn validate_checkpoint(
    root: &Path,
    pin: &PinnedGeneration,
    records: &[AuditRecord],
) -> Result<(), LedgerError> {
    validate_checkpoint_using(root, pin, records, None)
}
pub(crate) fn validate_checkpoint_using(
    root: &Path,
    pin: &PinnedGeneration,
    records: &[AuditRecord],
    budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<(), LedgerError> {
    let Some(manifest) = &pin.manifest else {
        return Ok(());
    };
    let head = usize::try_from(manifest.durable_watermark)
        .map_err(|_| integrity("checkpoint watermark overflows"))?;
    let prefix = records
        .get(..head)
        .ok_or_else(|| integrity("history is below checkpoint watermark"))?;
    for segment in &pin.segments {
        let first = prefix
            .get((segment.first_sequence - 1) as usize)
            .ok_or_else(|| integrity("segment starts outside retained history"))?;
        let last = prefix
            .get((segment.last_sequence - 1) as usize)
            .ok_or_else(|| integrity("segment ends outside retained history"))?;
        if first.previous_hash() != segment.previous_chain_hash
            || last.hash() != segment.terminal_chain_hash
        {
            return Err(integrity(
                "segment chain anchor disagrees with retained history",
            ));
        }
    }
    let projected = if let Some(budget) = budget.as_deref() {
        crate::ledger::project_observation(manifest.run_id, prefix, budget)?
    } else {
        crate::ledger::project(manifest.run_id, prefix)?
    };
    if Some(&projected) != pin.checkpoint_projection.as_ref() {
        return Err(integrity(
            "checkpoint projection disagrees with chain replay",
        ));
    }
    let index: Vec<ReceiptIndexEntry> =
        read_content_json_using(root, &manifest.receipt_index, MAX_LEDGER_BYTES, budget)?;
    if index != receipt_index(prefix) {
        return Err(integrity(
            "retained receipt index disagrees with accepted history",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SwitchBarrier {
    OwnerRetained,
    SegmentSynced,
    CheckpointSynced,
    IndexSynced,
    ManifestSynced,
    DirectorySynced,
    Selected,
    SelectionSynced,
    WorkspaceSelected,
    Completed,
}
#[cfg(test)]
pub(crate) const SWITCH_BARRIERS: [SwitchBarrier; 8] = [
    SwitchBarrier::OwnerRetained,
    SwitchBarrier::SegmentSynced,
    SwitchBarrier::CheckpointSynced,
    SwitchBarrier::IndexSynced,
    SwitchBarrier::ManifestSynced,
    SwitchBarrier::DirectorySynced,
    SwitchBarrier::Selected,
    SwitchBarrier::SelectionSynced,
];

/// Caller owns workspace exclusive lease, Run lifecycle lease and audit flock.
/// `operation` is the retained maintenance owner, so retries use the same files.
pub(crate) fn rollover(
    root: &Path,
    run_id: Uuid,
    operation: Uuid,
    old: &PinnedGeneration,
    records: &[AuditRecord],
    projection: &RunStateProjection,
    barrier: &mut impl FnMut(SwitchBarrier) -> Result<(), LedgerError>,
) -> Result<Manifest, LedgerError> {
    let generation = old
        .manifest
        .as_ref()
        .map_or(2, |m| m.generation.saturating_add(1));
    let relative = format!("ledger-generations/{operation}");
    let directory = root.join(&relative);
    private_directory(&root.join("ledger-generations"))?;
    private_directory(&directory)?;
    let mut segments = if let Some(manifest) = &old.manifest {
        read_content_json::<Checkpoint>(root, &manifest.checkpoint, MAX_METADATA_BYTES)?.segments
    } else {
        Vec::new()
    };
    let first_sequence = old.manifest.as_ref().map_or(1, |m| m.durable_watermark + 1);
    let tail = records
        .iter()
        .filter(|record| record.sequence() >= first_sequence)
        .collect::<Vec<_>>();
    if !tail.is_empty() {
        if segments.len() >= MAX_SEGMENTS {
            return Err(LedgerError::Capacity(
                "checkpoint segment reference limit reached".to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        for record in &tail {
            bytes.extend_from_slice(&record.canonical_line()?);
        }
        if bytes.len() as u64 > MAX_LEDGER_BYTES || tail.len() > MAX_LEDGER_RECORDS {
            return Err(LedgerError::Capacity(
                "selected generation exceeds maintenance bounds".to_owned(),
            ));
        }
        let content = create_content(root, format!("{relative}/history.jsonl"), &bytes)?;
        segments.push(Segment {
            content,
            first_sequence,
            last_sequence: tail.last().expect("nonempty").sequence(),
            previous_chain_hash: tail[0].previous_hash().to_owned(),
            terminal_chain_hash: tail.last().expect("nonempty").hash().to_owned(),
        });
    }
    barrier(SwitchBarrier::SegmentSynced)?;
    let checkpoint = create_json(
        root,
        format!("{relative}/checkpoint.json"),
        &Checkpoint {
            schema: "dolgorae.native-ledger-checkpoint/v1".to_owned(),
            run_id,
            projection: projection.clone(),
            segments,
        },
    )?;
    barrier(SwitchBarrier::CheckpointSynced)?;
    let receipt_index = create_json(
        root,
        format!("{relative}/receipts.json"),
        &receipt_index(records),
    )?;
    barrier(SwitchBarrier::IndexSynced)?;
    create_content(root, format!("{relative}/append.jsonl"), b"")?;
    let manifest = Manifest {
        schema: "dolgorae.native-ledger-generation/v1".to_owned(),
        generation,
        run_id,
        checkpoint,
        receipt_index,
        append_path: format!("{relative}/append.jsonl"),
        durable_watermark: projection.ledger_head.sequence,
        chain_hash: projection.ledger_head.hash.clone(),
        finalization_reserved_bytes: crate::ledger::FINALIZATION_RESERVED_BYTES,
    };
    let content = create_json(root, format!("{relative}/manifest.json"), &manifest)?;
    let metadata_bytes = manifest
        .checkpoint
        .byte_length
        .saturating_add(manifest.receipt_index.byte_length)
        .saturating_add(content.byte_length);
    let bytes = serde_json::to_vec(&Selection {
        schema: "dolgorae.native-ledger-selection/v1".to_owned(),
        manifest: content,
    })
    .map_err(|e| integrity(e.to_string()))?;
    if metadata_bytes.saturating_add(2 * bytes.len() as u64)
        > MAX_LEDGER_BYTES - crate::ledger::FINALIZATION_RESERVED_BYTES
    {
        return Err(LedgerError::Capacity(
            "checkpoint and selection metadata leave insufficient finalization reserve".to_owned(),
        ));
    }
    barrier(SwitchBarrier::ManifestSynced)?;
    sync_directory(&directory)?;
    sync_directory(&root.join("ledger-generations"))?;
    sync_directory(root)?;
    barrier(SwitchBarrier::DirectorySynced)?;
    let temporary = root.join(format!(".ledger-selection-{operation}.tmp"));
    if temporary.exists() {
        verify_secure_file(&temporary, DarwinSystem.current_uid())
            .map_err(LedgerError::SecurityPolicy)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, root.join("ledger-selection.json"))?;
    barrier(SwitchBarrier::Selected)?;
    sync_directory(root)?;
    retain_activation(root, run_id)?;
    barrier(SwitchBarrier::SelectionSynced)?;
    Ok(manifest)
}

fn private_directory(path: &Path) -> Result<(), LedgerError> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    verify_secure_directory(path, DarwinSystem.current_uid()).map_err(LedgerError::SecurityPolicy)
}
fn checked_path(root: &Path, relative: &str) -> Result<PathBuf, LedgerError> {
    let path = Path::new(relative);
    if relative.is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(integrity("invalid generation-relative path"));
    }
    let full = root.join(path);
    let mut ancestor = full.parent();
    while let Some(parent) = ancestor {
        verify_secure_directory(parent, DarwinSystem.current_uid())
            .map_err(LedgerError::SecurityPolicy)?;
        if parent == root {
            break;
        }
        ancestor = parent.parent();
    }
    Ok(full)
}
fn secure_open(path: &Path) -> Result<File, LedgerError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    verify_secure_file(path, DarwinSystem.current_uid()).map_err(LedgerError::SecurityPolicy)?;
    Ok(file)
}
fn read_json_using<T: serde::de::DeserializeOwned>(
    path: &Path,
    maximum: u64,
    mut budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<T, LedgerError> {
    let file = secure_open(path)?;
    if file.metadata()?.len() > maximum {
        return Err(integrity("generation metadata exceeds its bound"));
    }
    let reserved = if let Some(budget) = budget.as_deref_mut() {
        let length = file.metadata()?.len();
        budget.charge_bytes(length)?;
        length
    } else {
        maximum + 1
    };
    let bytes = read_reserved(&mut file.take(reserved), budget)?;
    if bytes.len() as u64 > maximum {
        return Err(integrity("generation metadata exceeds its bound"));
    }
    crate::jcs::parse(std::str::from_utf8(&bytes).map_err(|e| integrity(e.to_string()))?)
        .map_err(|e| integrity(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| integrity(e.to_string()))
}
fn open_content(root: &Path, content: &Content, maximum: u64) -> Result<File, LedgerError> {
    open_content_using(root, content, maximum, None)
}
fn open_content_using(
    root: &Path,
    content: &Content,
    maximum: u64,
    mut budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<File, LedgerError> {
    let mut file = secure_open(&checked_path(root, &content.path)?)?;
    if content.byte_length > maximum || file.metadata()?.len() != content.byte_length {
        return Err(integrity("generation content length mismatch"));
    }
    if let Some(budget) = budget.as_deref_mut() {
        budget.charge_bytes(content.byte_length)?;
    }
    let mut reserved_reader = (&mut file).take(if budget.is_some() {
        content.byte_length
    } else {
        maximum.saturating_add(1)
    });
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut length = 0_u64;
    loop {
        if let Some(budget) = budget.as_deref_mut() {
            budget.check()?;
        }
        let n = reserved_reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        length += n as u64;
        if length > maximum {
            return Err(integrity("generation content exceeds its bound"));
        }
        digest.update(&buffer[..n]);
    }
    if format!("{:x}", digest.finalize()) != content.sha256 || length != content.byte_length {
        return Err(integrity("generation content digest mismatch"));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
fn read_content_json<T: serde::de::DeserializeOwned>(
    root: &Path,
    content: &Content,
    maximum: u64,
) -> Result<T, LedgerError> {
    read_content_json_using::<T>(root, content, maximum, None)
}
fn read_content_json_using<T: serde::de::DeserializeOwned>(
    root: &Path,
    content: &Content,
    maximum: u64,
    mut budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<T, LedgerError> {
    let file = open_content_using(root, content, maximum, budget.as_deref_mut())?;
    if let Some(budget) = budget.as_deref_mut() {
        budget.charge_bytes(content.byte_length)?;
    }
    let bytes = read_reserved(
        &mut file.take(if budget.is_some() {
            content.byte_length
        } else {
            maximum.saturating_add(1)
        }),
        budget,
    )?;
    if bytes.len() as u64 != content.byte_length {
        return Err(integrity(
            "generation content changed during its reserved read",
        ));
    }
    crate::jcs::parse(std::str::from_utf8(&bytes).map_err(|e| integrity(e.to_string()))?)
        .map_err(|e| integrity(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| integrity(e.to_string()))
}
fn read_reserved(
    reader: &mut impl std::io::Read,
    mut budget: Option<&mut crate::ledger::ObservationBudget>,
) -> Result<Vec<u8>, LedgerError> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        if let Some(budget) = budget.as_deref_mut() {
            budget.check()?;
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    Ok(bytes)
}
fn create_json(root: &Path, path: String, value: &impl Serialize) -> Result<Content, LedgerError> {
    let bytes = serde_json::to_vec(value).map_err(|e| integrity(e.to_string()))?;
    create_content(root, path, &bytes)
}
fn create_content(root: &Path, path: String, bytes: &[u8]) -> Result<Content, LedgerError> {
    let content = Content {
        path,
        byte_length: bytes.len() as u64,
        sha256: sha256_hex(bytes),
    };
    let full = checked_path(root, &content.path)?;
    let temporary = full.with_extension(format!("{}.tmp", Uuid::now_v7()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::hard_link(&temporary, &full) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                open_content(root, &content, bytes.len() as u64)?.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok::<_, LedgerError>(())
    })();
    let cleanup = fs::remove_file(&temporary);
    result?;
    cleanup?;
    Ok(content)
}
fn integrity(reason: impl Into<String>) -> LedgerError {
    LedgerError::Integrity(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Ledger, ObservedLedger};
    use std::os::unix::fs::DirBuilderExt as _;

    struct Tree {
        root: PathBuf,
        run_id: Uuid,
    }
    impl Tree {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dolgorae-segments-{}", Uuid::now_v7()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("recovery"))
                .unwrap();
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join("audit.jsonl"))
                .unwrap();
            Self {
                root,
                run_id: Uuid::now_v7(),
            }
        }
        fn seed(&self) -> (Vec<AuditRecord>, RunStateProjection) {
            let mut ledger = Ledger::open(&self.root, self.run_id).unwrap();
            for (kind, payload) in [
                (AuditKind::RunCreated, serde_json::json!({})),
                (
                    AuditKind::ThreadBound,
                    serde_json::json!({"thread_id":"thread-a"}),
                ),
                (
                    AuditKind::TurnStarted,
                    serde_json::json!({"turn_id":"turn-a"}),
                ),
                (
                    AuditKind::TurnTerminal,
                    serde_json::json!({"turn_id":"turn-a"}),
                ),
            ] {
                ledger.append_required_payload(kind, &payload, 1).unwrap();
            }
            (
                ledger.durable_records().unwrap().to_vec(),
                ledger.projection().unwrap(),
            )
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn display_prefix_reselects_generation_and_rejects_checkpoint_or_receipt_damage() {
        for damaged in [
            "checkpoint.json",
            "receipts.json",
            "selection.json",
            "history.jsonl",
        ] {
            let tree = Tree::new();
            let (records, projection) = tree.seed();
            let old_display = ObservedLedger::open_display(&tree.root, tree.run_id, 4).unwrap();
            let old_pin = pin(&tree.root, tree.run_id).unwrap();
            let operation = Uuid::now_v7();
            rollover(
                &tree.root,
                tree.run_id,
                operation,
                &old_pin,
                &records,
                &projection,
                &mut |_| Ok(()),
            )
            .unwrap();
            let display = ObservedLedger::open_display(&tree.root, tree.run_id, 4).unwrap();
            let mut expected = Vec::new();
            old_display.write_canonical_history(&mut expected).unwrap();
            let mut actual = Vec::new();
            display.write_canonical_history(&mut actual).unwrap();
            assert_eq!(actual, expected);
            let path = tree
                .root
                .join("ledger-generations")
                .join(operation.to_string())
                .join(damaged);
            let mut bytes = fs::read(&path).unwrap();
            bytes[0] = b'!';
            fs::write(path, bytes).unwrap();
            assert!(
                ObservedLedger::open_display(&tree.root, tree.run_id, 4).is_err(),
                "cached {damaged} damage was hidden"
            );
            // Already pinned snapshots retain their original coherent bytes.
            let mut retained = Vec::new();
            display.write_canonical_history(&mut retained).unwrap();
            assert_eq!(retained, expected);
        }
    }

    #[test]
    fn interrupted_generation_switch_retains_exact_history_and_reader_pin() {
        for crash in SWITCH_BARRIERS
            .into_iter()
            .filter(|b| *b != SwitchBarrier::OwnerRetained)
        {
            let tree = Tree::new();
            let (records, projection) = tree.seed();
            let legacy = fs::read(tree.root.join("audit.jsonl")).unwrap();
            let pinned_reader = pin(&tree.root, tree.run_id).unwrap();
            let owner = Uuid::now_v7();
            let result = rollover(
                &tree.root,
                tree.run_id,
                owner,
                &pinned_reader,
                &records,
                &projection,
                &mut |barrier| {
                    if barrier == crash {
                        Err(LedgerError::Io(std::io::Error::other("simulated crash")))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err());
            let after = pin(&tree.root, tree.run_id).unwrap();
            assert_eq!(
                after.manifest.is_some(),
                matches!(
                    crash,
                    SwitchBarrier::Selected | SwitchBarrier::SelectionSynced
                )
            );
            let observed = ObservedLedger::open_complete(
                &tree.root,
                tree.run_id,
                projection.ledger_head.sequence,
            )
            .unwrap();
            assert_eq!(observed.records(), records);
            let mut exported = Vec::new();
            observed.write_canonical_history(&mut exported).unwrap();
            assert_eq!(exported, legacy);
            if after.manifest.is_none() {
                rollover(
                    &tree.root,
                    tree.run_id,
                    owner,
                    &pinned_reader,
                    &records,
                    &projection,
                    &mut |_| Ok(()),
                )
                .unwrap();
            } else {
                sync_directory(&tree.root).unwrap();
            }
            assert_eq!(fs::read(&pinned_reader.append_path).unwrap(), legacy);
            let mut reopened = Ledger::open(&tree.root, tree.run_id).unwrap();
            assert_eq!(reopened.durable_records().unwrap(), records);
            reopened
                .append_required_payload(
                    AuditKind::ProfileObserved,
                    &serde_json::json!({"checkpoint":"continued"}),
                    2,
                )
                .unwrap();
            assert_eq!(
                reopened.next_sequence(),
                projection.ledger_head.sequence + 2
            );
            assert_eq!(
                reopened.durable_records().unwrap()[records.len()].previous_hash(),
                projection.ledger_head.hash
            );
            assert_eq!(fs::read(tree.root.join("audit.jsonl")).unwrap(), legacy);
            drop(reopened);
            let second_records = ObservedLedger::open_complete(
                &tree.root,
                tree.run_id,
                projection.ledger_head.sequence + 1,
            )
            .unwrap();
            let second_projection =
                crate::ledger::project(tree.run_id, second_records.records()).unwrap();
            let second_pin = pin(&tree.root, tree.run_id).unwrap();
            rollover(
                &tree.root,
                tree.run_id,
                Uuid::now_v7(),
                &second_pin,
                second_records.records(),
                &second_projection,
                &mut |_| Ok(()),
            )
            .unwrap();
            let final_ledger = Ledger::open(&tree.root, tree.run_id).unwrap();
            assert_eq!(
                final_ledger.durable_records().unwrap(),
                second_records.records()
            );
            assert_eq!(final_ledger.projection().unwrap(), second_projection);
            let mut exported = Vec::new();
            second_records
                .write_canonical_history(&mut exported)
                .unwrap();
            assert_eq!(&exported[..legacy.len()], legacy);
            assert!(exported.len() > legacy.len());
            assert_eq!(fs::read(&pinned_reader.append_path).unwrap(), legacy);
        }
    }

    #[test]
    fn missing_or_rolled_back_selector_cannot_rewind_activated_history() {
        let tree = Tree::new();
        let (records, projection) = tree.seed();
        let old = pin(&tree.root, tree.run_id).unwrap();
        rollover(
            &tree.root,
            tree.run_id,
            Uuid::now_v7(),
            &old,
            &records,
            &projection,
            &mut |_| Ok(()),
        )
        .unwrap();
        let first_selection = fs::read(tree.root.join("ledger-selection.json")).unwrap();
        let mut ledger = Ledger::open(&tree.root, tree.run_id).unwrap();
        ledger
            .append_required_payload(
                AuditKind::ProfileObserved,
                &serde_json::json!({"accepted":"after-checkpoint"}),
                2,
            )
            .unwrap();
        let new_records = ledger.durable_records().unwrap().to_vec();
        let new_projection = ledger.projection().unwrap();
        drop(ledger);
        let state = fs::read(tree.root.join("state.json")).unwrap();
        let append = pin(&tree.root, tree.run_id).unwrap().append_path;
        let accepted = fs::read(&append).unwrap();
        fs::remove_file(tree.root.join("ledger-selection.json")).unwrap();
        assert!(matches!(
            Ledger::open(&tree.root, tree.run_id),
            Err(LedgerError::Integrity(_))
        ));
        assert!(
            ObservedLedger::open_complete(
                &tree.root,
                tree.run_id,
                new_projection.ledger_head.sequence
            )
            .is_err()
        );
        assert_eq!(fs::read(tree.root.join("state.json")).unwrap(), state);
        assert_eq!(fs::read(&append).unwrap(), accepted);
        fs::write(tree.root.join("ledger-selection.json"), &first_selection).unwrap();
        fs::set_permissions(
            tree.root.join("ledger-selection.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let prior = pin(&tree.root, tree.run_id).unwrap();
        rollover(
            &tree.root,
            tree.run_id,
            Uuid::now_v7(),
            &prior,
            &new_records,
            &new_projection,
            &mut |_| Ok(()),
        )
        .unwrap();
        fs::write(tree.root.join("ledger-selection.json"), first_selection).unwrap();
        assert!(matches!(
            Ledger::open(&tree.root, tree.run_id),
            Err(LedgerError::Integrity(_))
        ));
        assert_eq!(fs::read(tree.root.join("state.json")).unwrap(), state);
        assert_eq!(fs::read(&append).unwrap(), accepted);
    }

    #[test]
    fn corrupted_segment_and_checkpoint_never_become_capacity_or_empty_history() {
        for target in [
            "history.jsonl",
            "checkpoint.json",
            "receipts.json",
            "manifest.json",
        ] {
            let tree = Tree::new();
            let (records, projection) = tree.seed();
            let owner = Uuid::now_v7();
            rollover(
                &tree.root,
                tree.run_id,
                owner,
                &pin(&tree.root, tree.run_id).unwrap(),
                &records,
                &projection,
                &mut |_| Ok(()),
            )
            .unwrap();
            let path = tree
                .root
                .join(format!("ledger-generations/{owner}/{target}"));
            let mut bytes = fs::read(&path).unwrap();
            bytes[0] ^= 1;
            fs::write(&path, bytes).unwrap();
            assert!(matches!(
                Ledger::open(&tree.root, tree.run_id),
                Err(LedgerError::Integrity(_))
            ));
            assert!(matches!(
                ObservedLedger::open_complete(&tree.root, tree.run_id, 4),
                Err(LedgerError::Integrity(_))
            ));
        }
    }

    #[test]
    fn workspace_exclusive_lease_refuses_live_ledgers_and_closes_new_admission() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-maintenance-gate-{}", Uuid::now_v7()));
        for path in [&root, &root.join("runtime"), &root.join("runtime/locks")] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        let first = WorkspaceLease::acquire(&root, false).unwrap();
        let second = WorkspaceLease::acquire(&root, false).unwrap();
        assert!(matches!(
            WorkspaceLease::acquire(&root, true),
            Err(LedgerError::WriterBusy(_))
        ));
        drop(first);
        drop(second);
        let maintenance = WorkspaceLease::acquire(&root, true).unwrap();
        assert!(matches!(
            WorkspaceLease::acquire(&root, false),
            Err(LedgerError::WriterBusy(_))
        ));
        assert!(matches!(
            WorkspaceLease::acquire(&root, true),
            Err(LedgerError::WriterBusy(_))
        ));
        drop(maintenance);
        WorkspaceLease::acquire(&root, false).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn complete_observation_reserved_read_checks_deadline_between_chunks() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        struct Clock(Arc<AtomicU64>);
        impl crate::ledger::LedgerClock for Clock {
            fn monotonic_millis(&self) -> u64 {
                self.0.load(Ordering::Relaxed)
            }
            fn timestamp(&self) -> String {
                "2026-10-26T00:00:00.000000Z".to_owned()
            }
        }
        struct ReadCut {
            clock: Arc<AtomicU64>,
            first: bool,
        }
        impl std::io::Read for ReadCut {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if !self.first {
                    return Ok(0);
                }
                self.first = false;
                bytes[0] = b'x';
                self.clock
                    .store(crate::ledger::REPLAY_BUDGET_MILLIS, Ordering::Relaxed);
                Ok(1)
            }
        }
        let clock = Arc::new(AtomicU64::new(0));
        let mut budget = crate::ledger::ObservationBudget::new(
            Box::new(Clock(clock.clone())),
            crate::ledger::MAX_LEDGER_BYTES,
            crate::ledger::MAX_LEDGER_RECORDS,
        );
        let mut reader = ReadCut { clock, first: true };
        assert!(matches!(
            read_reserved(&mut reader, Some(&mut budget)),
            Err(LedgerError::OperationTimeout(_))
        ));
    }

    #[test]
    fn complete_observation_accounts_immutable_prefix_before_replaying_it() {
        let tree = Tree::new();
        let (mut records, _) = tree.seed();
        let mut previous = records.last().unwrap().hash().to_owned();
        for _ in 0..256 {
            let record = AuditRecord::new(
                records.len() as u64 + 1,
                "2026-10-26T00:00:00.000000Z",
                tree.run_id,
                1,
                AuditKind::ProfileObserved,
                crate::jcs::parse(&serde_json::json!({"body":"x".repeat(2048)}).to_string())
                    .unwrap(),
                &previous,
            )
            .unwrap();
            previous = record.hash().to_owned();
            records.push(record);
        }
        let bytes = records
            .iter()
            .flat_map(|r| r.canonical_line().unwrap())
            .collect::<Vec<_>>();
        fs::write(tree.root.join("audit.jsonl"), &bytes).unwrap();
        let projection = crate::ledger::project(tree.run_id, &records).unwrap();
        let old = pin(&tree.root, tree.run_id).unwrap();
        rollover(
            &tree.root,
            tree.run_id,
            Uuid::now_v7(),
            &old,
            &records,
            &projection,
            &mut |_| Ok(()),
        )
        .unwrap();
        let mut ordinary = crate::ledger::ObservationBudget::default();
        let captured = ObservedLedger::open_complete_bounded(
            &tree.root,
            tree.run_id,
            records.len() as u64,
            &mut ordinary,
        )
        .unwrap();
        assert_eq!(captured.records(), records);
        // One history scan fits this allowance; verification plus replay does not.
        // No prefix hashing/metadata pass can occur outside the enclosing budget.
        let mut bounded = crate::ledger::ObservationBudget::new(
            Box::new(crate::ledger::SystemLedgerClock::default()),
            2 * bytes.len() as u64 - 1,
            crate::ledger::MAX_LEDGER_RECORDS,
        );
        assert!(matches!(
            ObservedLedger::open_complete_bounded(
                &tree.root,
                tree.run_id,
                records.len() as u64,
                &mut bounded
            ),
            Err(LedgerError::Capacity(_))
        ));
    }
}
