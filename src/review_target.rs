//! Immutable review-target capture and owner-bound settlement.

use crate::machine::{MachineError, new_uuid_v7};
use crate::workspace::{WorkspaceService, workspace_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use uuid::Uuid;

const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TARGET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Capture,
    Settle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TargetKind {
    Workspace,
    Staged,
    Dirty,
    Head,
    Commit,
    Range,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestEntry {
    path: String,
    mode: u32,
    size: u64,
    sha256: String,
    classification: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureManifest {
    schema: String,
    target_kind: TargetKind,
    requested_revision: Option<String>,
    resolved_base: Option<String>,
    resolved_head: Option<String>,
    layout: String,
    entries: Vec<ManifestEntry>,
    excluded: Vec<String>,
    whole_target_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRecord {
    schema: String,
    capture_ref: Uuid,
    revision: u64,
    backend_kind: String,
    backend_lifecycle_id: String,
    owner_digest: String,
    manifest_digest: String,
    whole_target_digest: String,
    settled: bool,
    accepted_receipt_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalReceipt {
    schema: String,
    backend_kind: String,
    backend_lifecycle_id: String,
    terminal_state: String,
    state_revision: u64,
    evidence_digest: String,
}

pub fn execute(operation: Operation, arguments: &[OsString]) -> Result<Value, MachineError> {
    match operation {
        Operation::Capture => capture(arguments),
        Operation::Settle => settle(arguments),
    }
}

/// Revalidate one published capture without consulting mutable source state.
///
/// The scoped review coordinator uses this immediately before settlement so a
/// Reviewer-visible byte or manifest replacement can never be accepted as the
/// captured target. The returned object contains identity data only; it does
/// not expose the settlement owner or its carrier.
pub fn inspect_capture_in_state_root(
    workspace_state_root: &Path,
    capture_ref: Uuid,
) -> Result<Value, MachineError> {
    let capture_root = workspace_state_root
        .join("review-targets")
        .join(capture_ref.to_string());
    let record: CaptureRecord = read_json(&capture_root.join("record.json"), 64 * 1024)?;
    if record.capture_ref != capture_ref || record.settled {
        return Err(target_error(
            "REVIEW_TARGET_STATE_INVALID",
            "capture is absent, mismatched, or already settled",
        ));
    }
    let source = capture_root.join("source");
    validate_capture_integrity(&capture_root, &source, &record)?;
    let manifest_bytes = read_bounded(&capture_root.join("manifest.json"), 8 * 1024 * 1024)?;
    let manifest: CaptureManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| target_error("REVIEW_TARGET_MUTATED", "capture manifest is invalid"))?;
    Ok(json!({
        "schema":"dolgorae-review-target-integrity-result/v1",
        "capture_ref":record.capture_ref,
        "capture_revision":record.revision,
        "target_kind":manifest.target_kind,
        "requested_revision":manifest.requested_revision,
        "resolved_base":manifest.resolved_base,
        "resolved_head":manifest.resolved_head,
        "manifest_digest":record.manifest_digest,
        "whole_target_digest":record.whole_target_digest,
        "immutable_root":source,
        "integrity":"verified"
    }))
}

fn capture(arguments: &[OsString]) -> Result<Value, MachineError> {
    let workspace = option_path(arguments, "--workspace")?;
    let kind = parse_kind(&required(arguments, "--kind")?)?;
    let revision = optional(arguments, "--revision")?;
    validate_revision(kind, revision.as_deref())?;
    let backend_kind = bounded_token(&required(arguments, "--backend-kind")?, "backend_kind")?;
    let lifecycle = bounded_token(
        &required(arguments, "--backend-lifecycle-id")?,
        "backend_lifecycle_id",
    )?;
    let owner_file = required_path(arguments, "--settlement-owner-file")?;
    require_absolute(&owner_file, "--settlement-owner-file")?;

    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let root = view.canonical_path.to_path_buf()?;
    require_outside_workspace(&owner_file, &root, "--settlement-owner-file", false)?;
    let targets = state_root(&root).join("review-targets");
    secure_directory(&targets)?;
    let capture_ref = new_uuid_v7();
    let temporary = targets.join(format!(".capture-{capture_ref}"));
    let verify = targets.join(format!(".verify-{capture_ref}"));
    let final_root = targets.join(capture_ref.to_string());
    fs::create_dir(&temporary).map_err(io_error)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    let mut owner_created = false;
    let result = (|| {
        let first = materialize(&root, &temporary.join("source"), kind, revision.as_deref())?;
        fs::create_dir(&verify).map_err(io_error)?;
        fs::set_permissions(&verify, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        let second = materialize(&root, &verify.join("source"), kind, revision.as_deref())?;
        if first != second {
            return Err(target_error(
                "REVIEW_TARGET_SOURCE_DRIFT",
                "review target changed while it was captured",
            ));
        }
        let manifest_bytes = serde_json::to_vec(&first).map_err(internal)?;
        write_new(&temporary.join("manifest.json"), &manifest_bytes, 0o444)?;
        let manifest_digest = digest(&manifest_bytes);
        let owner = new_uuid_v7().to_string();
        write_new(&owner_file, owner.as_bytes(), 0o600)?;
        owner_created = true;
        let record = CaptureRecord {
            schema: "dolgorae-review-target-capture-record/v1".to_owned(),
            capture_ref,
            revision: 1,
            backend_kind: backend_kind.clone(),
            backend_lifecycle_id: lifecycle.clone(),
            owner_digest: digest(owner.as_bytes()),
            manifest_digest: manifest_digest.clone(),
            whole_target_digest: first.whole_target_digest.clone(),
            settled: false,
            accepted_receipt_digest: None,
        };
        write_record(&temporary.join("record.json"), &record)?;
        fs::rename(&temporary, &final_root).map_err(io_error)?;
        Ok(json!({
            "schema":"dolgorae-review-target-capture-result/v1",
            "capture_ref":capture_ref,
            "capture_revision":1,
            "target_kind":kind,
            "resolved_base":first.resolved_base,
            "resolved_head":first.resolved_head,
            "manifest_digest":manifest_digest,
            "whole_target_digest":first.whole_target_digest,
            "included_file_count":first.entries.len(),
            "excluded":first.excluded,
            "backend_kind":backend_kind,
            "backend_lifecycle_id":lifecycle,
            "owner_binding_digest":record.owner_digest,
            "immutable_root":final_root.join("source")
        }))
    })();
    let _ = remove_tree(&verify);
    if result.is_err() {
        let _ = remove_tree(&temporary);
        if owner_created {
            let _ = fs::remove_file(&owner_file);
        }
    }
    result
}

fn settle(arguments: &[OsString]) -> Result<Value, MachineError> {
    let workspace = option_path(arguments, "--workspace")?;
    let capture_ref = required(arguments, "--capture-ref")?
        .parse::<Uuid>()
        .map_err(|_| invalid("--capture-ref", "capture reference must be a UUID"))?;
    let expected = required(arguments, "--expected-revision")?
        .parse::<u64>()
        .map_err(|_| invalid("--expected-revision", "revision must be an integer"))?;
    let owner_file = required_path(arguments, "--settlement-owner-file")?;
    let receipt_file = required_path(arguments, "--terminal-receipt-file")?;
    require_absolute(&owner_file, "--settlement-owner-file")?;
    require_absolute(&receipt_file, "--terminal-receipt-file")?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let root = view.canonical_path.to_path_buf()?;
    require_outside_workspace(&owner_file, &root, "--settlement-owner-file", true)?;
    require_outside_workspace(&receipt_file, &root, "--terminal-receipt-file", true)?;
    let capture_root = state_root(&root)
        .join("review-targets")
        .join(capture_ref.to_string());
    settle_at(
        &capture_root,
        capture_ref,
        expected,
        &owner_file,
        &receipt_file,
    )
}

pub fn settle_capture_in_state_root(
    workspace_state_root: &Path,
    capture_ref: Uuid,
    expected_revision: u64,
    owner_file: &Path,
    receipt_file: &Path,
) -> Result<Value, MachineError> {
    require_absolute(workspace_state_root, "workspace_state_root")?;
    require_absolute(owner_file, "settlement_owner_file")?;
    require_absolute(receipt_file, "terminal_receipt_file")?;
    let capture_root = workspace_state_root
        .join("review-targets")
        .join(capture_ref.to_string());
    settle_at(
        &capture_root,
        capture_ref,
        expected_revision,
        owner_file,
        receipt_file,
    )
}

fn settle_at(
    capture_root: &Path,
    capture_ref: Uuid,
    expected: u64,
    owner_file: &Path,
    receipt_file: &Path,
) -> Result<Value, MachineError> {
    let _guard = SettlementGuard::acquire(capture_root)?;
    let record_path = capture_root.join("record.json");
    let mut record: CaptureRecord = read_json(&record_path, 64 * 1024)?;
    if record.capture_ref != capture_ref {
        return Err(target_error(
            "REVIEW_TARGET_STALE_REVISION",
            "capture revision does not match",
        ));
    }
    let owner = read_private(owner_file, 1024)?;
    if digest(&owner) != record.owner_digest {
        return Err(target_error(
            "REVIEW_TARGET_FOREIGN_OWNER",
            "settlement owner does not match",
        ));
    }
    let receipt_bytes = read_private(receipt_file, 64 * 1024)?;
    let receipt: TerminalReceipt = serde_json::from_slice(&receipt_bytes).map_err(|_| {
        invalid(
            "--terminal-receipt-file",
            "receipt is not a checked v1 object",
        )
    })?;
    validate_receipt(&record, &receipt)?;
    let receipt_digest = digest(&receipt_bytes);
    if record.settled {
        if expected.checked_add(1) == Some(record.revision)
            && record.accepted_receipt_digest.as_deref() == Some(&receipt_digest)
        {
            cleanup_settlement_source(capture_root)?;
            return Ok(settlement_result(&record, true));
        }
        return Err(target_error(
            "REVIEW_TARGET_SETTLEMENT_CONFLICT",
            "settlement replay differs",
        ));
    }
    if record.revision != expected {
        return Err(target_error(
            "REVIEW_TARGET_STALE_REVISION",
            "capture revision does not match",
        ));
    }
    let source = capture_root.join("source");
    let settling = capture_root.join(".settlement-source");
    if source.exists() && settling.exists() {
        return Err(target_error(
            "REVIEW_TARGET_STATE_INVALID",
            "capture has conflicting settlement source trees",
        ));
    }
    if source.exists() {
        validate_capture_integrity(capture_root, &source, &record)?;
        fs::rename(&source, &settling).map_err(io_error)?;
    } else if settling.exists() {
        validate_capture_integrity(capture_root, &settling, &record)?;
    } else {
        return Err(target_error(
            "REVIEW_TARGET_STATE_INVALID",
            "capture source is missing before settlement",
        ));
    }
    record.settled = true;
    record.revision += 1;
    record.accepted_receipt_digest = Some(receipt_digest);
    write_record(&record_path, &record)?;
    cleanup_settlement_source(capture_root)?;
    Ok(settlement_result(&record, false))
}

struct SettlementGuard(File);

impl SettlementGuard {
    fn acquire(root: &Path) -> Result<Self, MachineError> {
        let path = root.join(".settlement-lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(io_error)?;
        file.try_lock().map_err(|_| {
            target_error(
                "REVIEW_TARGET_SETTLEMENT_CONCURRENT",
                "another settlement owns this capture",
            )
        })?;
        Ok(Self(file))
    }
}

impl Drop for SettlementGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn cleanup_settlement_source(root: &Path) -> Result<(), MachineError> {
    let path = root.join(".settlement-source");
    if path.exists() {
        remove_tree(&path)?;
    }
    Ok(())
}

fn settlement_result(record: &CaptureRecord, replay: bool) -> Value {
    json!({
        "schema":"dolgorae-review-target-settlement-result/v1",
        "capture_ref":record.capture_ref,
        "capture_revision":record.revision,
        "state":"settled",
        "source_bytes_removed":true,
        "replay":replay,
        "receipt_digest":record.accepted_receipt_digest
    })
}

fn validate_capture_integrity(
    root: &Path,
    source: &Path,
    record: &CaptureRecord,
) -> Result<(), MachineError> {
    let manifest_bytes = read_bounded(&root.join("manifest.json"), 8 * 1024 * 1024)?;
    if digest(&manifest_bytes) != record.manifest_digest {
        return Err(target_error(
            "REVIEW_TARGET_MUTATED",
            "capture manifest changed after publication",
        ));
    }
    let manifest: CaptureManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| target_error("REVIEW_TARGET_MUTATED", "capture manifest is invalid"))?;
    if manifest.whole_target_digest != record.whole_target_digest {
        return Err(target_error(
            "REVIEW_TARGET_MUTATED",
            "capture identity changed after publication",
        ));
    }
    let mut observed = Vec::new();
    collect_materialized_entries(source, source, &mut observed)?;
    observed.sort_by(|a, b| a.path.cmp(&b.path));
    if observed != manifest.entries {
        return Err(target_error(
            "REVIEW_TARGET_MUTATED",
            "captured bytes changed after publication",
        ));
    }
    Ok(())
}

fn collect_materialized_entries(
    source_root: &Path,
    current: &Path,
    entries: &mut Vec<ManifestEntry>,
) -> Result<(), MachineError> {
    let metadata = fs::symlink_metadata(current).map_err(io_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(target_error(
            "REVIEW_TARGET_MUTATED",
            "capture layout contains an unsafe entry",
        ));
    }
    for entry in fs::read_dir(current).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_materialized_entries(source_root, &path, entries)?;
            continue;
        }
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(target_error(
                "REVIEW_TARGET_MUTATED",
                "capture layout contains an unsafe entry",
            ));
        }
        let relative = path
            .strip_prefix(source_root)
            .map_err(internal)?
            .to_str()
            .ok_or_else(|| target_error("REVIEW_TARGET_MUTATED", "capture path is opaque"))?
            .to_owned();
        let bytes = read_bounded(&path, MAX_FILE_BYTES)?;
        entries.push(ManifestEntry {
            path: relative,
            mode: if metadata.permissions().mode() & 0o111 != 0 {
                0o555
            } else {
                0o444
            },
            size: bytes.len() as u64,
            sha256: digest(&bytes),
            classification: classify(&bytes).to_owned(),
        });
    }
    Ok(())
}

fn validate_receipt(record: &CaptureRecord, receipt: &TerminalReceipt) -> Result<(), MachineError> {
    if receipt.schema != "dolgorae-review-target-terminal-receipt/v1"
        || receipt.backend_kind != record.backend_kind
        || receipt.backend_lifecycle_id != record.backend_lifecycle_id
    {
        return Err(target_error(
            "REVIEW_TARGET_LIFECYCLE_MISMATCH",
            "terminal receipt binding does not match",
        ));
    }
    if !matches!(
        receipt.terminal_state.as_str(),
        "completed" | "failed" | "cancelled" | "timed_out"
    ) || receipt.state_revision == 0
        || !is_digest(&receipt.evidence_digest)
    {
        return Err(target_error(
            "REVIEW_TARGET_TERMINAL_EVIDENCE_INVALID",
            "terminal receipt is not authoritative",
        ));
    }
    Ok(())
}

fn materialize(
    root: &Path,
    destination: &Path,
    kind: TargetKind,
    revision: Option<&str>,
) -> Result<CaptureManifest, MachineError> {
    fs::create_dir(destination).map_err(|error| io_context("create target root", error))?;
    ensure_index_resolved(root)?;
    let (layout, base, head, before, after) = match kind {
        TargetKind::Workspace => ("current", None, head(root)?, None, Source::Worktree),
        TargetKind::Staged => {
            let head = head(root)?;
            (
                "transition",
                head.clone(),
                head.clone(),
                source_tree(head.as_deref()),
                Source::Index,
            )
        }
        TargetKind::Dirty => {
            let head = head(root)?;
            (
                "transition",
                head.clone(),
                head.clone(),
                source_tree(head.as_deref()),
                Source::Worktree,
            )
        }
        TargetKind::Head => {
            let commit = resolve_commit(root, "HEAD")?;
            (
                "current",
                None,
                Some(commit.clone()),
                None,
                Source::Tree(commit),
            )
        }
        TargetKind::Commit => {
            let commit = resolve_commit(root, revision.expect("validated"))?;
            let parent = first_parent(root, &commit)?;
            (
                "transition",
                parent.clone(),
                Some(commit.clone()),
                source_tree(parent.as_deref()),
                Source::Tree(commit),
            )
        }
        TargetKind::Range => {
            let expression = revision.expect("validated");
            let (left, right, three) = split_range(expression)?;
            let right = resolve_commit(root, right)?;
            let base = if three {
                merge_base(root, left, &right)?
            } else {
                resolve_commit(root, left)?
            };
            (
                "transition",
                Some(base.clone()),
                Some(right.clone()),
                Some(Source::Tree(base)),
                Source::Tree(right),
            )
        }
    };
    let mut entries = Vec::new();
    if let Some(source) = before {
        materialize_source(
            root,
            &destination.join("before"),
            source,
            "before",
            &mut entries,
        )?;
    }
    let after_name = if layout == "current" {
        "current"
    } else {
        "after"
    };
    materialize_source(
        root,
        &destination.join(after_name),
        after,
        after_name,
        &mut entries,
    )?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    readonly_directories(destination)?;
    let preimage =
        serde_json::to_vec(&(kind, revision, &base, &head, &entries)).map_err(internal)?;
    Ok(CaptureManifest {
        schema: "dolgorae-review-target-manifest/v1".to_owned(),
        target_kind: kind,
        requested_revision: revision.map(ToOwned::to_owned),
        resolved_base: base,
        resolved_head: head,
        layout: layout.to_owned(),
        entries,
        excluded: vec![
            "ignored_files".to_owned(),
            "git_metadata".to_owned(),
            "private_tool_state".to_owned(),
        ],
        whole_target_digest: digest(&preimage),
    })
}

#[derive(Clone)]
enum Source {
    Empty,
    Worktree,
    Index,
    Tree(String),
}

fn source_tree(value: Option<&str>) -> Option<Source> {
    Some(match value {
        Some(value) => Source::Tree(value.to_owned()),
        None => Source::Empty,
    })
}

fn materialize_source(
    root: &Path,
    destination: &Path,
    source: Source,
    prefix: &str,
    entries: &mut Vec<ManifestEntry>,
) -> Result<(), MachineError> {
    fs::create_dir(destination)
        .map_err(|error| io_context("create materialization root", error))?;
    let candidates = match source {
        Source::Empty => Vec::new(),
        Source::Worktree => worktree_entries(root)?,
        Source::Index => index_entries(root)?,
        Source::Tree(commit) => tree_entries(root, &commit)?,
    };
    if candidates.len() > MAX_FILES {
        return Err(target_error(
            "REVIEW_TARGET_LIMIT_EXCEEDED",
            "target has too many files",
        ));
    }
    let mut total = 0_u64;
    for candidate in candidates {
        total = total
            .checked_add(candidate.bytes.len() as u64)
            .ok_or_else(|| target_error("REVIEW_TARGET_LIMIT_EXCEEDED", "target size overflow"))?;
        if candidate.bytes.len() as u64 > MAX_FILE_BYTES || total > MAX_TARGET_BYTES {
            return Err(target_error(
                "REVIEW_TARGET_LIMIT_EXCEEDED",
                "target exceeds byte limits",
            ));
        }
        screen(&candidate.bytes)?;
        let output = safe_join(destination, &candidate.path)?;
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        write_new(&output, &candidate.bytes, candidate.mode)?;
        entries.push(ManifestEntry {
            path: format!("{prefix}/{}", candidate.path),
            mode: candidate.mode,
            size: candidate.bytes.len() as u64,
            sha256: digest(&candidate.bytes),
            classification: classify(&candidate.bytes).to_owned(),
        });
    }
    readonly_directories(destination)?;
    Ok(())
}

struct Candidate {
    path: String,
    mode: u32,
    bytes: Vec<u8>,
}

fn worktree_entries(root: &Path) -> Result<Vec<Candidate>, MachineError> {
    let bytes = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut result = Vec::new();
    for raw in bytes.split(|b| *b == 0).filter(|v| !v.is_empty()) {
        let path = std::str::from_utf8(raw)
            .map_err(|_| target_error("REVIEW_TARGET_UNSAFE_PATH", "opaque paths are unsupported"))?
            .to_owned();
        if is_private_tool_path(&path) {
            continue;
        }
        let full = safe_join(root, &path)?;
        let metadata = match fs::symlink_metadata(&full) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(error)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(target_error(
                "REVIEW_TARGET_UNSAFE_FILE",
                "links and special files are unsupported",
            ));
        }
        let bytes = read_bounded(&full, MAX_FILE_BYTES)?;
        let mode = if metadata.permissions().mode() & 0o111 != 0 {
            0o555
        } else {
            0o444
        };
        result.push(Candidate { path, mode, bytes });
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    result.dedup_by(|a, b| a.path == b.path);
    Ok(result)
}

fn index_entries(root: &Path) -> Result<Vec<Candidate>, MachineError> {
    parse_ls_files(root, &git(root, &["ls-files", "--stage", "-z"])?)
}

fn ensure_index_resolved(root: &Path) -> Result<(), MachineError> {
    if !git(root, &["ls-files", "--unmerged", "-z"])?.is_empty() {
        Err(target_error(
            "REVIEW_TARGET_CONFLICT",
            "unresolved index conflict",
        ))
    } else {
        Ok(())
    }
}

fn parse_ls_files(root: &Path, bytes: &[u8]) -> Result<Vec<Candidate>, MachineError> {
    let mut result = Vec::new();
    for raw in bytes.split(|b| *b == 0).filter(|v| !v.is_empty()) {
        let tab = raw
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| internal("invalid index record"))?;
        let header = std::str::from_utf8(&raw[..tab]).map_err(internal)?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[2] != "0" {
            return Err(target_error(
                "REVIEW_TARGET_CONFLICT",
                "unresolved index conflict",
            ));
        }
        let path = std::str::from_utf8(&raw[tab + 1..])
            .map_err(|_| target_error("REVIEW_TARGET_UNSAFE_PATH", "opaque paths are unsupported"))?
            .to_owned();
        if is_private_tool_path(&path) {
            continue;
        }
        result.push(Candidate {
            path,
            mode: git_mode(fields[0])?,
            bytes: git(root, &["cat-file", "blob", fields[1]])?,
        });
    }
    Ok(result)
}

fn tree_entries(root: &Path, commit: &str) -> Result<Vec<Candidate>, MachineError> {
    let bytes = git(root, &["ls-tree", "-rz", "--full-tree", commit])?;
    let mut result = Vec::new();
    for raw in bytes.split(|b| *b == 0).filter(|v| !v.is_empty()) {
        let tab = raw
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| internal("invalid tree record"))?;
        let header = std::str::from_utf8(&raw[..tab]).map_err(internal)?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[1] != "blob" {
            return Err(target_error(
                "REVIEW_TARGET_UNSAFE_FILE",
                "gitlinks and non-blob entries are unsupported",
            ));
        }
        let path = std::str::from_utf8(&raw[tab + 1..])
            .map_err(|_| target_error("REVIEW_TARGET_UNSAFE_PATH", "opaque paths are unsupported"))?
            .to_owned();
        if is_private_tool_path(&path) {
            continue;
        }
        result.push(Candidate {
            path,
            mode: git_mode(fields[0])?,
            bytes: git(root, &["cat-file", "blob", fields[2]])?,
        });
    }
    Ok(result)
}

fn git_mode(value: &str) -> Result<u32, MachineError> {
    match value {
        "100644" => Ok(0o444),
        "100755" => Ok(0o555),
        "120000" => Err(target_error(
            "REVIEW_TARGET_UNSAFE_FILE",
            "symbolic links are unsupported",
        )),
        _ => Err(target_error(
            "REVIEW_TARGET_UNSAFE_FILE",
            "unsupported Git file mode",
        )),
    }
}

fn head(root: &Path) -> Result<Option<String>, MachineError> {
    match git_status(root, &["rev-parse", "--verify", "HEAD^{commit}"])? {
        Some(value) => Ok(Some(line(&value)?)),
        None => Ok(None),
    }
}

fn resolve_commit(root: &Path, value: &str) -> Result<String, MachineError> {
    git_status(
        root,
        &["rev-parse", "--verify", &format!("{value}^{{commit}}")],
    )?
    .map(|v| line(&v))
    .transpose()?
    .ok_or_else(|| {
        target_error(
            "REVIEW_TARGET_REVISION_INVALID",
            "revision does not resolve to a commit",
        )
    })
}

fn first_parent(root: &Path, commit: &str) -> Result<Option<String>, MachineError> {
    let bytes = git(root, &["rev-list", "--parents", "-n", "1", commit])?;
    let value = std::str::from_utf8(&bytes).map_err(internal)?.trim();
    Ok(value.split_whitespace().nth(1).map(ToOwned::to_owned))
}

fn merge_base(root: &Path, left: &str, right: &str) -> Result<String, MachineError> {
    let left = resolve_commit(root, left)?;
    line(&git(root, &["merge-base", &left, right])?)
}

fn split_range(value: &str) -> Result<(&str, &str, bool), MachineError> {
    if let Some((a, b)) = value.split_once("...")
        && !a.is_empty()
        && !b.is_empty()
        && !a.ends_with('.')
        && !b.starts_with('.')
        && !b.contains("...")
    {
        return Ok((a, b, true));
    }
    if !value.contains("...")
        && let Some((a, b)) = value.split_once("..")
        && !a.is_empty()
        && !b.is_empty()
        && !a.ends_with('.')
        && !b.starts_with('.')
        && !b.contains("..")
    {
        return Ok((a, b, false));
    }
    Err(target_error(
        "REVIEW_TARGET_REVISION_INVALID",
        "range must contain exactly one .. or ... operator",
    ))
}

fn git(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, MachineError> {
    git_status(root, arguments)?
        .ok_or_else(|| target_error("REVIEW_TARGET_GIT_FAILED", "Git command failed"))
}

fn git_status(root: &Path, arguments: &[&str]) -> Result<Option<Vec<u8>>, MachineError> {
    let output = Command::new("/usr/bin/git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.fsmonitor=false", "-c", "gc.auto=0", "-C"])
        .arg(root)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| io_context("launch Git", error))?;
    Ok(output.status.success().then_some(output.stdout))
}

fn line(bytes: &[u8]) -> Result<String, MachineError> {
    let value = std::str::from_utf8(bytes).map_err(internal)?.trim();
    if value.is_empty() || value.contains(char::is_whitespace) {
        return Err(internal("Git returned an invalid identity"));
    }
    Ok(value.to_owned())
}

fn state_root(root: &Path) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    home.join("Library/Application Support/Dolgorae/workspaces")
        .join(workspace_id(root))
}

fn secure_directory(path: &Path) -> Result<(), MachineError> {
    fs::create_dir_all(path).map_err(io_error)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

fn readonly_directories(path: &Path) -> Result<(), MachineError> {
    for entry in fs::read_dir(path).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if entry.file_type().map_err(io_error)?.is_dir() {
            readonly_directories(&entry.path())?;
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(io_error)
}

fn remove_tree(path: &Path) -> Result<(), MachineError> {
    if !path.exists() {
        return Ok(());
    }
    make_directories_writable(path)?;
    fs::remove_dir_all(path).map_err(io_error)
}

fn make_directories_writable(path: &Path) -> Result<(), MachineError> {
    if path.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        for entry in fs::read_dir(path).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            if entry.file_type().map_err(io_error)?.is_dir() {
                make_directories_writable(&entry.path())?;
            }
        }
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, MachineError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || relative.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(target_error(
            "REVIEW_TARGET_UNSAFE_PATH",
            "path is not a safe relative path",
        ));
    }
    Ok(root.join(path))
}

fn screen(bytes: &[u8]) -> Result<(), MachineError> {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    for marker in [
        "-----begin private key-----",
        "-----begin rsa private key-----",
        "-----begin ec private key-----",
        "-----begin openssh private key-----",
        "-----begin encrypted private key-----",
        "authorization: bearer ",
        "aws_secret_access_key",
        "aws_session_token",
        "openai_api_key",
        "github_token",
        "github_pat_",
        "ghp_",
        "sk-proj-",
        "sk-ant-",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "ya29.",
    ] {
        if text.contains(marker) {
            return Err(target_error(
                "REVIEW_TARGET_SECRET_DETECTED",
                "recognized credential material is not capturable",
            ));
        }
    }
    Ok(())
}

fn classify(bytes: &[u8]) -> &'static str {
    if std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0) {
        "source_text"
    } else {
        "source_binary"
    }
}

fn is_private_tool_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        matches!(
            component,
            Component::Normal(name)
                if [".dolgorae", ".podway", ".mulgae", ".gaori"]
                    .iter()
                    .any(|private| name == OsStr::new(private))
        )
    })
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), MachineError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn write_record(path: &Path, record: &CaptureRecord) -> Result<(), MachineError> {
    let bytes = serde_json::to_vec(record).map_err(internal)?;
    let temporary = path.with_extension(format!("tmp-{}", new_uuid_v7()));
    write_new(&temporary, &bytes, 0o600)?;
    fs::rename(temporary, path).map_err(io_error)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, limit: u64) -> Result<T, MachineError> {
    serde_json::from_slice(&read_bounded(path, limit)?)
        .map_err(|_| target_error("REVIEW_TARGET_STATE_INVALID", "capture state is invalid"))
}

fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>, MachineError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != current_uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(target_error(
            "REVIEW_TARGET_CARRIER_INVALID",
            "carrier must be a same-uid private regular file",
        ));
    }
    read_bounded(path, limit)
}

fn current_uid() -> u32 {
    Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(u32::MAX)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, MachineError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(io_error)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(target_error(
            "REVIEW_TARGET_LIMIT_EXCEEDED",
            "file exceeds byte limit",
        ));
    }
    Ok(bytes)
}

fn parse_kind(value: &str) -> Result<TargetKind, MachineError> {
    match value {
        "workspace" => Ok(TargetKind::Workspace),
        "staged" => Ok(TargetKind::Staged),
        "dirty" => Ok(TargetKind::Dirty),
        "head" => Ok(TargetKind::Head),
        "commit" => Ok(TargetKind::Commit),
        "range" => Ok(TargetKind::Range),
        _ => Err(invalid("--kind", "unsupported review target kind")),
    }
}

fn validate_revision(kind: TargetKind, revision: Option<&str>) -> Result<(), MachineError> {
    match (kind, revision) {
        (TargetKind::Commit | TargetKind::Range, Some(value))
            if !value.is_empty() && value.len() <= 4096 =>
        {
            Ok(())
        }
        (TargetKind::Commit | TargetKind::Range, _) => {
            Err(invalid("--revision", "commit and range require a revision"))
        }
        (_, None) => Ok(()),
        _ => Err(invalid("--revision", "this target kind rejects a revision")),
    }
}

fn bounded_token(value: &str, field: &str) -> Result<String, MachineError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(invalid(
            field,
            "value is empty, oversized, or contains control characters",
        ))
    } else {
        Ok(value.to_owned())
    }
}

fn required(arguments: &[OsString], flag: &str) -> Result<String, MachineError> {
    optional(arguments, flag)?.ok_or_else(|| invalid(flag, "required UTF-8 option is missing"))
}
fn optional(arguments: &[OsString], flag: &str) -> Result<Option<String>, MachineError> {
    arguments
        .windows(2)
        .find(|w| w[0] == OsStr::new(flag))
        .map(|w| {
            w[1].to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| invalid(flag, "value must be UTF-8"))
        })
        .transpose()
}
fn required_path(arguments: &[OsString], flag: &str) -> Result<PathBuf, MachineError> {
    arguments
        .windows(2)
        .find(|w| w[0] == OsStr::new(flag))
        .map(|w| PathBuf::from(&w[1]))
        .ok_or_else(|| invalid(flag, "required path is missing"))
}
fn option_path(arguments: &[OsString], flag: &str) -> Result<Option<PathBuf>, MachineError> {
    crate::cli::option_path(arguments, flag).map_err(|e| invalid(flag, e))
}
fn require_absolute(path: &Path, field: &str) -> Result<(), MachineError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(invalid(field, "path must be absolute"))
    }
}
fn require_outside_workspace(
    path: &Path,
    workspace: &Path,
    field: &str,
    existing: bool,
) -> Result<(), MachineError> {
    let resolved = if existing {
        path.canonicalize().map_err(io_error)?
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| invalid(field, "path requires a parent directory"))?
            .canonicalize()
            .map_err(io_error)?;
        parent.join(
            path.file_name()
                .ok_or_else(|| invalid(field, "path requires a file name"))?,
        )
    };
    if resolved.starts_with(workspace) {
        Err(invalid(
            field,
            "carrier must be outside the source workspace",
        ))
    } else {
        Ok(())
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn invalid(field: impl Into<String>, reason: impl Into<String>) -> MachineError {
    MachineError::invalid_argument(field, reason)
}
fn target_error(code: &'static str, message: &'static str) -> MachineError {
    MachineError::new(code, message, false, json!({"required_action":"none"}))
}
fn io_error(error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "REVIEW_TARGET_IO_FAILURE",
        "review target I/O failed",
        false,
        json!({"invariant":error.to_string()}),
    )
}
fn io_context(context: &'static str, error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "REVIEW_TARGET_IO_FAILURE",
        "review target I/O failed",
        false,
        json!({"invariant":format!("{context}: {error}")}),
    )
}
fn internal(error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "review target coordinator failed",
        false,
        json!({"invariant":error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(root: &Path, arguments: &[&str]) {
        assert!(
            Command::new("/usr/bin/git")
                .arg("-C")
                .arg(root)
                .args(arguments)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn revision_contract_is_closed() {
        assert!(validate_revision(TargetKind::Workspace, None).is_ok());
        assert!(validate_revision(TargetKind::Workspace, Some("HEAD")).is_err());
        assert!(validate_revision(TargetKind::Commit, Some("HEAD")).is_ok());
        assert!(validate_revision(TargetKind::Range, None).is_err());
    }

    #[test]
    fn range_operator_is_preserved() {
        assert_eq!(split_range("A..B").unwrap(), ("A", "B", false));
        assert_eq!(split_range("A...B").unwrap(), ("A", "B", true));
        assert!(split_range("A....B").is_err());
    }

    #[test]
    fn secret_markers_fail_closed() {
        assert!(screen(b"-----BEGIN PRIVATE KEY-----").is_err());
        assert!(screen(b"-----BEGIN ENCRYPTED PRIVATE KEY-----").is_err());
        assert!(screen(b"token=sk-proj-example").is_err());
        assert!(screen(b"token=glpat-example").is_err());
        assert!(screen(b"ordinary source").is_ok());
    }

    #[test]
    fn six_scope_materialization_is_immutable_and_non_mutating() {
        let root = std::env::temp_dir().join(format!("dolgorae-target-test-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        run_git(&root, &["init", "-q"]);
        fs::write(root.join("tracked.txt"), b"base\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Dolgorae Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "base",
            ],
        );
        let first = resolve_commit(&root, "HEAD").unwrap();
        fs::write(root.join("tracked.txt"), b"staged\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Dolgorae Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "second",
            ],
        );
        let second = resolve_commit(&root, "HEAD").unwrap();
        fs::write(root.join("tracked.txt"), b"index\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        fs::write(root.join("tracked.txt"), b"worktree\n").unwrap();
        fs::write(root.join("untracked.txt"), b"untracked\n").unwrap();
        let status = git(&root, &["status", "--porcelain=v2", "-z"]).unwrap();

        let cases = vec![
            (TargetKind::Workspace, None),
            (TargetKind::Staged, None),
            (TargetKind::Dirty, None),
            (TargetKind::Head, None),
            (TargetKind::Commit, Some(second.clone())),
            (TargetKind::Range, Some(format!("{first}..{second}"))),
            (TargetKind::Range, Some(format!("{first}...{second}"))),
        ];
        for (index, (kind, revision)) in cases.into_iter().enumerate() {
            let destination = root
                .parent()
                .unwrap()
                .join(format!("dolgorae-target-output-{}-{index}", new_uuid_v7()));
            let manifest = materialize(&root, &destination, kind, revision.as_deref()).unwrap();
            assert!(!manifest.whole_target_digest.is_empty());
            assert_eq!(
                git(&root, &["status", "--porcelain=v2", "-z"]).unwrap(),
                status
            );
            remove_tree(&destination).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn settlement_guard_rejects_a_concurrent_owner() {
        let root = std::env::temp_dir().join(format!("dolgorae-settlement-test-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        let first = SettlementGuard::acquire(&root).unwrap();
        let error = match SettlementGuard::acquire(&root) {
            Ok(_) => panic!("concurrent guard unexpectedly acquired"),
            Err(error) => error,
        };
        assert_eq!(error.code, "REVIEW_TARGET_SETTLEMENT_CONCURRENT");
        drop(first);
        assert!(SettlementGuard::acquire(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_materialization_detects_capture_time_drift() {
        let root = std::env::temp_dir().join(format!("dolgorae-drift-test-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        run_git(&root, &["init", "-q"]);
        fs::write(root.join("tracked.txt"), b"first\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        let first_root = root
            .parent()
            .unwrap()
            .join(format!("first-{}", new_uuid_v7()));
        let second_root = root
            .parent()
            .unwrap()
            .join(format!("second-{}", new_uuid_v7()));
        let first = materialize(&root, &first_root, TargetKind::Workspace, None).unwrap();
        fs::write(root.join("tracked.txt"), b"second\n").unwrap();
        let second = materialize(&root, &second_root, TargetKind::Workspace, None).unwrap();
        assert_ne!(first, second);
        remove_tree(&first_root).unwrap();
        remove_tree(&second_root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
