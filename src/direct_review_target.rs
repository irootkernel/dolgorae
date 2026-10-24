//! Copy-free source admission and durable, host-issued candidate read receipts.

use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::{MachineError, new_uuid_v7};
use crate::review_target::{
    Candidate, ensure_index_resolved, first_parent, git, git_status, head, index_entries,
    is_private_tool_path, read_worktree_file, resolve_commit, safe_join, screen, split_range,
    tree_entries, worktree_entries,
};
use crate::workspace::{
    LosslessPath, SystemWorkspacePlatform, WorkspaceMode, WorkspacePlatform, atomic_create,
    create_directory, open_secure_file_if_present, verify_secure_directory, workspace_id,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const SAMPLED_SCOPE_LIMIT: &str = "Admission and acceptance fingerprints are sampled and may miss intervening edits. External writers are outside Dolgorae control; this review provides no snapshot isolation.";

const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SIDE_BYTES: usize = 64 * 1024 * 1024;
const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_CONTROL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 8192;
// JSON can expand every pathname byte into a six-byte escape. Reserve fixed
// entry fields and binding/selection metadata independently of source bytes.
const MAX_BINDING_BYTES: u64 = (2 * MAX_FILES * (6 * MAX_PATH_BYTES + 1024) + 1024 * 1024) as u64;
const MAX_RECEIPTS: usize = 16_384;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectEntry {
    pub side: String,
    pub path: String,
    pub source: String,
    pub mode: String,
    pub byte_length: u64,
    pub sha256: String,
    pub blob_oid: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectRange {
    pub mode: String,
    pub left_oid: String,
    pub right_oid: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectTarget {
    pub schema: String,
    pub kind: String,
    pub requested_revision: Option<String>,
    pub workspace_id: String,
    pub resolved_base: Option<String>,
    pub resolved_head: Option<String>,
    pub range: Option<DirectRange>,
    pub assurance: String,
    pub reading_boundary: String,
    pub entries: Vec<DirectEntry>,
    pub excluded: Vec<String>,
    pub admission_fingerprint: String,
    pub target_sha256: String,
}

impl DirectTarget {
    pub(crate) fn metadata_bytes(&self) -> Result<Vec<u8>, MachineError> {
        serde_json::to_vec(self).map_err(control_error)
    }

    /// Admit metadata only. No source tree, checkout, index write or Run is created.
    pub fn admit(
        root: &Path,
        mode: WorkspaceMode,
        kind: &str,
        revision: Option<&str>,
        review_id: Uuid,
    ) -> Result<Self, MachineError> {
        Self::admit_inner(root, mode, kind, revision, review_id).map_err(|error| {
            if matches!(
                error.code.as_str(),
                "REVIEW_TARGET_UNAVAILABLE"
                    | "REVIEW_TARGET_GIT_FAILED"
                    | "REVIEW_TARGET_REVISION_INVALID"
            ) {
                review_error(
                    "REVIEW_TARGET_UNAVAILABLE",
                    "the selected Git target has unavailable objects or identities",
                    "restore_target",
                    "admission",
                    "not_dispatched",
                    Some(review_id),
                )
            } else {
                error
            }
        })
    }

    fn admit_inner(
        root: &Path,
        mode: WorkspaceMode,
        kind: &str,
        revision: Option<&str>,
        review_id: Uuid,
    ) -> Result<Self, MachineError> {
        if review_id.get_version_num() != 7 {
            return Err(MachineError::invalid_argument(
                "review_id",
                "direct review requires an original UUIDv7 identity",
            ));
        }
        validate_selection(kind, revision)?;
        let root = fs::canonicalize(root).map_err(|_| unavailable(None))?;
        let is_git = mode == WorkspaceMode::Git;
        if !is_git && kind != "workspace" {
            return Err(MachineError::invalid_argument(
                "target.kind",
                "non-Git workspaces support only workspace review",
            ));
        }
        if is_git {
            if git_status(&root, &["rev-parse", "--is-inside-work-tree"])?.as_deref()
                != Some(b"true\n")
            {
                return Err(unavailable(Some(review_id)));
            }
            ensure_index_resolved(&root)?;
        }
        let mut target = Self {
            schema: "dolgorae-review-target/v2".to_owned(),
            kind: kind.to_owned(),
            requested_revision: revision.map(ToOwned::to_owned),
            workspace_id: workspace_id(&root),
            resolved_base: None,
            resolved_head: None,
            range: None,
            assurance: if matches!(kind, "workspace" | "staged" | "dirty") {
                "sampled_mutable"
            } else {
                "pinned_objects"
            }
            .to_owned(),
            reading_boundary: match kind {
                "staged" => "checked_index",
                "workspace" | "dirty" => "checked_workspace",
                _ => "checked_objects",
            }
            .to_owned(),
            entries: Vec::new(),
            excluded: ["ignored_files", "git_metadata", "private_tool_state"]
                .map(ToOwned::to_owned)
                .to_vec(),
            admission_fingerprint: String::new(),
            target_sha256: String::new(),
        };
        match kind {
            "workspace" => {
                target.add_side(
                    "current",
                    "worktree",
                    if is_git {
                        worktree_entries(&root)?
                    } else {
                        non_git_entries(&root)?
                    },
                )?;
            }
            "staged" | "dirty" => {
                let base = head(&root)?;
                target.resolved_base.clone_from(&base);
                target.resolved_head.clone_from(&base);
                if let Some(base) = base {
                    target.add_side("before", "git_object", tree_entries(&root, &base)?)?;
                }
                let (source, entries) = if kind == "staged" {
                    ("index", index_entries(&root)?)
                } else {
                    ("worktree", worktree_entries(&root)?)
                };
                target.add_side("after", source, entries)?;
            }
            "head" | "commit" => {
                let oid = resolve_commit(&root, revision.unwrap_or("HEAD"))?;
                if kind == "commit" {
                    target.resolved_base = first_parent(&root, &oid)?;
                    if let Some(base) = &target.resolved_base {
                        let entries = tree_entries(&root, base)?;
                        target.add_side("before", "git_object", entries)?;
                    }
                }
                target.add_side(
                    if kind == "head" { "current" } else { "after" },
                    "git_object",
                    tree_entries(&root, &oid)?,
                )?;
                target.resolved_head = Some(oid);
            }
            "range" => {
                let (left, right, three_dot) = split_range(revision.expect("validated range"))?;
                let left_oid = resolve_commit(&root, left)?;
                let right_oid = resolve_commit(&root, right)?;
                let base = if three_dot {
                    git_identity(&git(&root, &["merge-base", &left_oid, &right_oid])?)?
                } else {
                    left_oid.clone()
                };
                target.add_side("before", "git_object", tree_entries(&root, &base)?)?;
                target.add_side("after", "git_object", tree_entries(&root, &right_oid)?)?;
                target.resolved_base = Some(base);
                target.resolved_head = Some(right_oid.clone());
                target.range = Some(DirectRange {
                    mode: if three_dot { "three_dot" } else { "two_dot" }.to_owned(),
                    left_oid,
                    right_oid,
                });
            }
            _ => unreachable!("validated selection"),
        }
        target
            .entries
            .sort_by(|a, b| (&a.side, &a.path).cmp(&(&b.side, &b.path)));
        target.admission_fingerprint = value_digest(
            &json!({"base":target.resolved_base,"head":target.resolved_head,"entries":target.entries}),
        )?;
        target.target_sha256 = target.metadata_digest()?;
        // Sampling twice rejects known admission drift without claiming isolation.
        target.check_drift(&root, mode, review_id)?;
        Ok(target)
    }

    fn add_side(
        &mut self,
        side: &str,
        source: &str,
        candidates: Vec<Candidate>,
    ) -> Result<(), MachineError> {
        if candidates.len() > MAX_FILES {
            return Err(limit());
        }
        let mut bytes = 0_usize;
        let mut paths = BTreeSet::new();
        for candidate in candidates {
            validate_path(&candidate.path)?;
            bytes = bytes.checked_add(candidate.bytes.len()).ok_or_else(limit)?;
            if !paths.insert(candidate.path.clone())
                || candidate.bytes.len() > MAX_FILE_BYTES
                || bytes > MAX_SIDE_BYTES
            {
                return Err(limit());
            }
            screen(&candidate.bytes)?;
            self.entries.push(DirectEntry {
                side: side.to_owned(),
                path: candidate.path,
                source: source.to_owned(),
                mode: if candidate.mode & 0o111 == 0 {
                    "100644"
                } else {
                    "100755"
                }
                .to_owned(),
                byte_length: candidate.bytes.len() as u64,
                sha256: sha256_hex(&candidate.bytes),
                blob_oid: candidate.blob_oid,
            });
        }
        Ok(())
    }

    fn metadata_digest(&self) -> Result<String, MachineError> {
        let mut value = serde_json::to_value(self).map_err(control_error)?;
        value
            .as_object_mut()
            .expect("target object")
            .remove("target_sha256");
        value_digest(&value)
    }

    pub fn check_drift(
        &self,
        root: &Path,
        mode: WorkspaceMode,
        review_id: Uuid,
    ) -> Result<(), MachineError> {
        if self.workspace_id
            != workspace_id(&fs::canonicalize(root).map_err(|_| unavailable(Some(review_id)))?)
            || self.metadata_digest()? != self.target_sha256
        {
            return Err(provenance_invalid(review_id));
        }
        if self.assurance == "pinned_objects" {
            return Ok(());
        }
        let (base, mut current) = Self::sample_mutable(root, mode, &self.kind)?;
        current.sort_by(|a, b| (&a.side, &a.path).cmp(&(&b.side, &b.path)));
        let fingerprint = value_digest(&json!({"base":base,"head":base,"entries":current}))?;
        if fingerprint != self.admission_fingerprint {
            return Err(review_error(
                "REVIEW_TARGET_DRIFTED",
                "the admitted mutable review target changed",
                "submit_new_review",
                "acceptance",
                "inconclusive",
                Some(review_id),
            ));
        }
        Ok(())
    }

    fn sample_mutable(
        root: &Path,
        mode: WorkspaceMode,
        kind: &str,
    ) -> Result<(Option<String>, Vec<DirectEntry>), MachineError> {
        let mut sampled = Self {
            schema: String::new(),
            kind: kind.to_owned(),
            requested_revision: None,
            workspace_id: String::new(),
            resolved_base: None,
            resolved_head: None,
            range: None,
            assurance: String::new(),
            reading_boundary: String::new(),
            entries: Vec::new(),
            excluded: Vec::new(),
            admission_fingerprint: String::new(),
            target_sha256: String::new(),
        };
        let is_git = mode == WorkspaceMode::Git;
        if is_git {
            ensure_index_resolved(root)?;
        }
        let base = if kind == "workspace" {
            None
        } else {
            head(root)?
        };
        if kind != "workspace" {
            if let Some(base) = &base {
                sampled.add_side("before", "git_object", tree_entries(root, base)?)?;
            }
            let (source, entries) = if kind == "staged" {
                ("index", index_entries(root)?)
            } else {
                ("worktree", worktree_entries(root)?)
            };
            sampled.add_side("after", source, entries)?;
        } else {
            sampled.add_side(
                "current",
                "worktree",
                if is_git {
                    worktree_entries(root)?
                } else {
                    non_git_entries(root)?
                },
            )?;
        }
        Ok((base, sampled.entries))
    }

    fn file_bytes(
        &self,
        root: &Path,
        side: &str,
        path: &str,
        review_id: Uuid,
    ) -> Result<Vec<u8>, MachineError> {
        #[cfg(test)]
        tests::source_read_fault(root)?;
        validate_path(path)?;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.side == side && entry.path == path)
            .ok_or_else(|| unavailable(Some(review_id)))?;
        let bytes = if let Some(oid) = &entry.blob_oid {
            git_status(root, &["cat-file", "blob", oid])?
                .ok_or_else(|| unavailable(Some(review_id)))?
        } else {
            let (before, bytes) =
                read_worktree_file(root, path)?.ok_or_else(|| unavailable(Some(review_id)))?;
            let (after, check) =
                read_worktree_file(root, path)?.ok_or_else(|| unavailable(Some(review_id)))?;
            if before.dev() != after.dev()
                || before.ino() != after.ino()
                || before.mode() != after.mode()
                || before.len() != after.len()
                || before.mtime() != after.mtime()
                || before.mtime_nsec() != after.mtime_nsec()
                || before.ctime() != after.ctime()
                || before.ctime_nsec() != after.ctime_nsec()
                || bytes != check
            {
                return Err(review_error(
                    "REVIEW_TARGET_DRIFTED",
                    "candidate changed while being read",
                    "submit_new_review",
                    "acceptance",
                    "inconclusive",
                    Some(review_id),
                ));
            }
            bytes
        };
        screen(&bytes)?;
        if bytes.len() as u64 != entry.byte_length || sha256_hex(&bytes) != entry.sha256 {
            return Err(review_error(
                "REVIEW_TARGET_DRIFTED",
                "candidate no longer matches admission",
                "submit_new_review",
                "acceptance",
                "inconclusive",
                Some(review_id),
            ));
        }
        Ok(bytes)
    }

    fn content(
        &self,
        root: &Path,
        side: &str,
        path: &str,
        review_id: Uuid,
    ) -> Result<Vec<u8>, MachineError> {
        if side != "diff" {
            return self.file_bytes(root, side, path, review_id);
        }
        validate_path(path)?;
        let mut sources = Vec::new();
        for selected in ["before", "after"] {
            sources.push(
                if self
                    .entries
                    .iter()
                    .any(|entry| entry.side == selected && entry.path == path)
                {
                    self.file_bytes(root, selected, path, review_id)?
                } else {
                    Vec::new()
                },
            );
        }
        if !self
            .entries
            .iter()
            .any(|entry| entry.path == path && matches!(entry.side.as_str(), "before" | "after"))
        {
            return Err(unavailable(Some(review_id)));
        }
        replacement_diff(path, &sources[0], &sources[1])
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateReadRequest {
    pub side: String,
    pub path: String,
    pub byte_offset: u64,
    pub byte_length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateReadReceipt {
    pub schema: String,
    pub receipt_id: Uuid,
    pub review_id: Uuid,
    pub target_sha256: String,
    pub side: String,
    pub path: String,
    pub sha256: String,
    pub byte_offset: u64,
    pub byte_length: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReaderBinding {
    schema: String,
    review_id: Uuid,
    run_id: Uuid,
    workspace: LosslessPath,
    workspace_mode: WorkspaceMode,
    target: DirectTarget,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IssuedRead {
    thread_id: String,
    turn_id: String,
    call_id: String,
    request_sha256: String,
    receipt: CandidateReadReceipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunReaderBinding {
    schema: String,
    review_id: Uuid,
    run_id: Uuid,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryCheck {
    schema: String,
    review_id: Uuid,
    thread_id: String,
    turn_id: String,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryResponse {
    schema: String,
    review_id: Uuid,
    thread_id: String,
    turn_id: String,
    final_response: Option<crate::turn::FinalResponse>,
}

/// Controller-only metadata, receipts and bounded final responses; no source snapshots.
pub struct DirectTargetReader {
    root: PathBuf,
    binding: ReaderBinding,
}

impl DirectTargetReader {
    pub fn create(
        state_root: &Path,
        workspace: &Path,
        mode: WorkspaceMode,
        review_id: Uuid,
        run_id: Uuid,
        target: DirectTarget,
    ) -> Result<Self, MachineError> {
        let reader = Self::create_root(state_root, workspace, mode, review_id, run_id, target)?;
        let runs = state_root.join("direct-review-runs");
        create_or_verify(&runs)?;
        publish(
            &runs.join(format!("{run_id}.json")),
            &RunReaderBinding {
                schema: "dolgorae-direct-review-run/v1".to_owned(),
                review_id,
                run_id,
            },
        )?;
        Ok(reader)
    }

    pub(crate) fn create_for_task(
        state_root: &Path,
        workspace: &Path,
        mode: WorkspaceMode,
        task_id: Uuid,
        run_id: Uuid,
        request: &crate::review::ReviewTargetRequest,
    ) -> Result<Self, MachineError> {
        let root = state_root.join("direct-reviews").join(task_id.to_string());
        match fs::symlink_metadata(&root) {
            Ok(_) => {
                let reader = Self::open(state_root, task_id, run_id)?;
                if reader.workspace()? != fs::canonicalize(workspace).map_err(control_error)?
                    || reader.workspace_mode() != mode
                    || reader.target().kind != request.kind
                    || reader.target().requested_revision != request.revision
                {
                    return Err(provenance_invalid(task_id));
                }
                Ok(reader)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let target = DirectTarget::admit(
                    workspace,
                    mode,
                    &request.kind,
                    request.revision.as_deref(),
                    task_id,
                )?;
                Self::create_root(state_root, workspace, mode, task_id, run_id, target)
            }
            Err(error) => Err(control_error(error)),
        }
    }

    /// Bind an accepted task to its exact admitted target, without re-admission.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_for_admitted_task(
        state_root: &Path,
        workspace: &Path,
        mode: WorkspaceMode,
        task_id: Uuid,
        run_id: Uuid,
        target: DirectTarget,
        create: bool,
    ) -> Result<Self, MachineError> {
        let root = state_root.join("direct-reviews").join(task_id.to_string());
        match fs::symlink_metadata(&root) {
            Ok(_) => {
                let reader = Self::open(state_root, task_id, run_id)?;
                if reader.workspace()? != fs::canonicalize(workspace).map_err(control_error)?
                    || reader.workspace_mode() != mode
                    || reader.target() != &target
                {
                    return Err(provenance_invalid(task_id));
                }
                Ok(reader)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                Self::create_root(state_root, workspace, mode, task_id, run_id, target)
            }
            Err(error) => Err(control_error(error)),
        }
    }

    fn create_root(
        state_root: &Path,
        workspace: &Path,
        mode: WorkspaceMode,
        review_id: Uuid,
        run_id: Uuid,
        target: DirectTarget,
    ) -> Result<Self, MachineError> {
        if review_id.get_version_num() != 7 || run_id.get_version_num() != 7 {
            return Err(provenance_invalid(review_id));
        }
        verify_secure_directory(state_root, current_uid())?;
        target.check_drift(workspace, mode, review_id)?;
        let parent = state_root.join("direct-reviews");
        create_or_verify(&parent)?;
        let root = parent.join(review_id.to_string());
        create_directory(&root, 0o700).map_err(control_error)?;
        create_directory(&root.join("reads"), 0o700).map_err(control_error)?;
        create_directory(&root.join("history"), 0o700).map_err(control_error)?;
        let binding = ReaderBinding {
            schema: "dolgorae-direct-reader-binding/v1".to_owned(),
            review_id,
            run_id,
            workspace: LosslessPath::from_path(
                &fs::canonicalize(workspace).map_err(control_error)?,
            ),
            workspace_mode: mode,
            target,
        };
        publish_bounded(&root.join("binding.json"), &binding, MAX_BINDING_BYTES)?;
        publish(&root.join("reader.lock"), &json!({}))?;
        Ok(Self { root, binding })
    }

    pub fn open(state_root: &Path, review_id: Uuid, run_id: Uuid) -> Result<Self, MachineError> {
        verify_secure_directory(state_root, current_uid())?;
        let parent = state_root.join("direct-reviews");
        verify_secure_directory(&parent, current_uid())?;
        let root = parent.join(review_id.to_string());
        verify_secure_directory(&root, current_uid())?;
        verify_secure_directory(&root.join("reads"), current_uid())?;
        verify_secure_directory(&root.join("history"), current_uid())?;
        let binding: ReaderBinding =
            read_control_bounded(&root.join("binding.json"), MAX_BINDING_BYTES)?;
        if binding.schema != "dolgorae-direct-reader-binding/v1"
            || binding.review_id != review_id
            || binding.run_id != run_id
            || binding.target.metadata_digest()? != binding.target.target_sha256
        {
            return Err(provenance_invalid(review_id));
        }
        Ok(Self { root, binding })
    }

    pub fn target(&self) -> &DirectTarget {
        &self.binding.target
    }
    pub fn review_id(&self) -> Uuid {
        self.binding.review_id
    }
    pub fn run_id(&self) -> Uuid {
        self.binding.run_id
    }
    pub fn workspace_mode(&self) -> WorkspaceMode {
        self.binding.workspace_mode
    }
    pub fn workspace(&self) -> Result<PathBuf, MachineError> {
        self.binding.workspace.to_path_buf()
    }

    pub(crate) fn completed_history_response(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<crate::turn::FinalResponse>, MachineError> {
        let _lock = self.lock()?;
        let check = HistoryCheck {
            schema: "dolgorae-direct-review-history/v1".to_owned(),
            review_id: self.review_id(),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
        };
        let name = value_digest(&json!([thread_id, turn_id]))?;
        let pending: HistoryCheck = read_control(
            &self
                .root
                .join("history")
                .join(format!("{name}.pending.json")),
        )?;
        let complete: HistoryCheck = read_control(
            &self
                .root
                .join("history")
                .join(format!("{name}.complete.json")),
        )?;
        if pending != check || complete != check {
            return Err(provenance_invalid(self.review_id()));
        }
        let response: HistoryResponse = read_control(
            &self
                .root
                .join("history")
                .join(format!("{name}.response.json")),
        )?;
        if response.schema != "dolgorae-direct-review-history-response/v1"
            || response.review_id != check.review_id
            || response.thread_id != check.thread_id
            || response.turn_id != check.turn_id
        {
            return Err(provenance_invalid(self.review_id()));
        }
        Ok(response.final_response)
    }

    pub(crate) fn retained_history_response(
        &self,
        thread: &str,
        turn: &str,
    ) -> Result<Option<Option<crate::turn::FinalResponse>>, MachineError> {
        let key = value_digest(&json!([thread, turn]))?;
        let record = read_control_if_present::<HistoryResponse>(
            &self
                .root
                .join("history")
                .join(format!("{key}.response.json")),
            MAX_CONTROL_BYTES,
        )?;
        record
            .map(|record| {
                if record.schema != "dolgorae-direct-review-history-response/v1"
                    || record.review_id != self.review_id()
                    || record.thread_id != thread
                    || record.turn_id != turn
                {
                    return Err(provenance_invalid(self.review_id()));
                }
                Ok(record.final_response)
            })
            .transpose()
    }

    pub fn for_run(state_root: &Path, run_id: Uuid) -> Result<Option<Self>, MachineError> {
        verify_secure_directory(state_root, current_uid())?;
        let directory = state_root.join("direct-review-runs");
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(control_error(error)),
            Ok(_) => verify_secure_directory(&directory, current_uid())?,
        }
        let path = directory.join(format!("{run_id}.json"));
        if open_secure_file_if_present(&path, current_uid())?.is_none() {
            return Ok(None);
        }
        let binding: RunReaderBinding = read_control(&path)?;
        if binding.schema != "dolgorae-direct-review-run/v1" || binding.run_id != run_id {
            return Err(provenance_invalid(binding.review_id));
        }
        Self::open(state_root, binding.review_id, run_id).map(Some)
    }

    /// Thread/Turn/call identities come from the authenticated native tool frame.
    pub fn read(
        &self,
        thread_id: &str,
        turn_id: &str,
        call_id: &str,
        request: &CandidateReadRequest,
    ) -> Result<Value, MachineError> {
        for identity in [thread_id, turn_id, call_id] {
            if identity.is_empty() || identity.len() > 128 || identity.chars().any(char::is_control)
            {
                return Err(provenance_invalid(self.binding.review_id));
            }
        }
        if !matches!(
            request.side.as_str(),
            "current" | "before" | "after" | "diff" | "metadata"
        ) || request.byte_length > MAX_READ_BYTES
        {
            return Err(MachineError::invalid_argument(
                "candidate_read",
                "read side or byte count is invalid",
            ));
        }
        validate_path(&request.path)?;
        let guard = self.lock()?;
        let request_sha256 = value_digest(&serde_json::to_value(request).map_err(control_error)?)?;
        let key = value_digest(&json!([thread_id, turn_id, call_id]))?;
        let path = self.root.join("reads").join(format!("{key}.json"));
        let metadata = request.side == "metadata";
        let content = if metadata {
            if request.path != "target.json" {
                return Err(MachineError::invalid_argument(
                    "candidate_read",
                    "metadata path must be target.json",
                ));
            }
            self.binding.target.metadata_bytes()?
        } else {
            self.binding.target.content(
                &self.binding.workspace.to_path_buf()?,
                &request.side,
                &request.path,
                self.binding.review_id,
            )?
        };
        let receipt_schema = if metadata {
            "dolgorae-target-metadata-read-receipt/v1"
        } else {
            "dolgorae-candidate-read-receipt/v1"
        };
        let end = request
            .byte_offset
            .checked_add(request.byte_length)
            .ok_or_else(|| provenance_invalid(self.binding.review_id))?;
        if end > content.len() as u64 {
            return Err(MachineError::invalid_argument(
                "candidate_read",
                "read range exceeds admitted content",
            ));
        }
        let receipt = if path.exists() {
            let old: IssuedRead = read_control(&path)?;
            if old.thread_id != thread_id
                || old.turn_id != turn_id
                || old.call_id != call_id
                || old.request_sha256 != request_sha256
                || old.receipt.schema != receipt_schema
                || old.receipt.receipt_id.get_version_num() != 7
                || old.receipt.review_id != self.binding.review_id
                || old.receipt.target_sha256 != self.binding.target.target_sha256
                || old.receipt.side != request.side
                || old.receipt.path != request.path
                || old.receipt.byte_offset != request.byte_offset
                || old.receipt.byte_length != request.byte_length
                || old.receipt.sha256 != sha256_hex(&content)
            {
                return Err(provenance_invalid(self.binding.review_id));
            }
            old.receipt
        } else {
            if fs::read_dir(self.root.join("reads"))
                .map_err(control_error)?
                .take(MAX_RECEIPTS + 1)
                .count()
                >= MAX_RECEIPTS
            {
                return Err(limit());
            }
            let receipt = CandidateReadReceipt {
                schema: receipt_schema.to_owned(),
                receipt_id: new_uuid_v7(),
                review_id: self.binding.review_id,
                target_sha256: self.binding.target.target_sha256.clone(),
                side: request.side.clone(),
                path: request.path.clone(),
                sha256: sha256_hex(&content),
                byte_offset: request.byte_offset,
                byte_length: request.byte_length,
            };
            publish(
                &path,
                &IssuedRead {
                    thread_id: thread_id.to_owned(),
                    turn_id: turn_id.to_owned(),
                    call_id: call_id.to_owned(),
                    request_sha256,
                    receipt: receipt.clone(),
                },
            )?;
            receipt
        };
        let bytes = &content[request.byte_offset as usize..end as usize];
        let response = json!({"receipt":receipt,"encoding":"base64","content":base64::engine::general_purpose::STANDARD.encode(bytes)});
        drop(guard);
        Ok(response)
    }

    pub fn replay_read(
        &self,
        thread_id: &str,
        turn_id: &str,
        call_id: &str,
        request: &CandidateReadRequest,
    ) -> Result<Value, MachineError> {
        let key = value_digest(&json!([thread_id, turn_id, call_id]))?;
        if open_secure_file_if_present(
            &self.root.join("reads").join(format!("{key}.json")),
            current_uid(),
        )?
        .is_none()
        {
            return Err(provenance_invalid(self.review_id()));
        }
        self.read(thread_id, turn_id, call_id, request)
    }

    pub fn begin_history(&self, thread_id: &str, turn_id: &str) -> Result<(), MachineError> {
        for id in [thread_id, turn_id] {
            if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
                return Err(provenance_invalid(self.review_id()));
            }
        }
        let _guard = self.lock()?;
        let key = value_digest(&json!([thread_id, turn_id]))?;
        let record = HistoryCheck {
            schema: "dolgorae-direct-review-history/v1".to_owned(),
            review_id: self.review_id(),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
        };
        let path = self
            .root
            .join("history")
            .join(format!("{key}.pending.json"));
        if path.try_exists().map_err(control_error)? {
            if read_control::<HistoryCheck>(&path)? != record {
                return Err(provenance_invalid(self.review_id()));
            }
        } else {
            publish(&path, &record)?;
        }
        Ok(())
    }

    pub fn finish_history(
        &self,
        thread_id: &str,
        turn_id: &str,
        items: &[Value],
    ) -> Result<(), MachineError> {
        self.finish_history_response(thread_id, turn_id, items, None)
    }

    pub(crate) fn finish_history_response(
        &self,
        thread_id: &str,
        turn_id: &str,
        items: &[Value],
        final_response: Option<crate::turn::FinalResponse>,
    ) -> Result<(), MachineError> {
        for item in items {
            self.observe_item(thread_id, turn_id, item, true)?;
        }
        let _guard = self.lock()?;
        let key = value_digest(&json!([thread_id, turn_id]))?;
        let record: HistoryCheck = read_control(
            &self
                .root
                .join("history")
                .join(format!("{key}.pending.json")),
        )?;
        if record.schema != "dolgorae-direct-review-history/v1"
            || record.review_id != self.review_id()
            || record.thread_id != thread_id
            || record.turn_id != turn_id
        {
            return Err(provenance_invalid(self.review_id()));
        }
        let response = HistoryResponse {
            schema: "dolgorae-direct-review-history-response/v1".to_owned(),
            review_id: self.review_id(),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            final_response,
        };
        let response_path = self
            .root
            .join("history")
            .join(format!("{key}.response.json"));
        if response_path.try_exists().map_err(control_error)? {
            if read_control::<HistoryResponse>(&response_path)? != response {
                return Err(provenance_invalid(self.review_id()));
            }
        } else {
            publish(&response_path, &response)?;
        }
        let path = self
            .root
            .join("history")
            .join(format!("{key}.complete.json"));
        if path.try_exists().map_err(control_error)? {
            if read_control::<HistoryCheck>(&path)? != record {
                return Err(provenance_invalid(self.review_id()));
            }
        } else {
            publish(&path, &record)?;
        }
        Ok(())
    }

    pub fn pending_histories(&self) -> Result<Vec<(String, String)>, MachineError> {
        let mut pending = Vec::new();
        for (count, entry) in fs::read_dir(self.root.join("history"))
            .map_err(control_error)?
            .enumerate()
        {
            if count >= 3 * MAX_RECEIPTS {
                return Err(limit());
            }
            let entry = entry.map_err(control_error)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| control_error("invalid history path"))?;
            let Some(key) = name.strip_suffix(".pending.json") else {
                continue;
            };
            let record: HistoryCheck = read_control(&entry.path())?;
            if record.schema != "dolgorae-direct-review-history/v1"
                || record.review_id != self.review_id()
                || value_digest(&json!([record.thread_id, record.turn_id]))? != key
            {
                return Err(provenance_invalid(self.review_id()));
            }
            let complete = self
                .root
                .join("history")
                .join(format!("{key}.complete.json"));
            if open_secure_file_if_present(&complete, current_uid())?.is_some() {
                if read_control::<HistoryCheck>(&complete)? != record {
                    return Err(provenance_invalid(self.review_id()));
                }
                self.completed_history_response(&record.thread_id, &record.turn_id)?;
            } else {
                pending.push((record.thread_id, record.turn_id));
            }
        }
        pending.sort();
        Ok(pending)
    }

    pub fn record_unmediated_tool(&self) -> Result<(), MachineError> {
        let _guard = self.lock()?;
        let path = self.root.join("provenance-violation.json");
        if !path.exists() {
            publish(
                &path,
                &json!({"schema":"dolgorae-direct-review-violation/v1","review_id":self.binding.review_id,"run_id":self.binding.run_id}),
            )?;
        }
        Ok(())
    }

    /// Only host-correlated completed dynamic calls can be candidate tools.
    /// An unknown native item is a durable violation, never evidence of safety.
    pub fn observe_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item: &Value,
        completed: bool,
    ) -> Result<(), MachineError> {
        if item
            .get("threadId")
            .and_then(Value::as_str)
            .is_some_and(|id| id != thread_id)
            || item
                .get("turnId")
                .and_then(Value::as_str)
                .is_some_and(|id| id != turn_id)
            || item
                .get("parentThreadId")
                .is_some_and(|value| !value.is_null())
        {
            self.record_unmediated_tool()?;
            return Ok(());
        }
        let valid = match item.get("type").and_then(Value::as_str) {
            Some("userMessage" | "agentMessage" | "reasoning" | "plan") => true,
            Some("dynamicToolCall")
                if item.get("tool").and_then(Value::as_str) == Some("dolgorae_candidate_read") =>
            {
                if !completed {
                    true
                } else if let Some(call_id) = item.get("id").and_then(Value::as_str) {
                    let key = value_digest(&json!([thread_id, turn_id, call_id]))?;
                    let path = self.root.join("reads").join(format!("{key}.json"));
                    let response = if item.get("success").is_some_and(|v| !v.is_null())
                        || item.get("contentItems").is_some_and(|v| !v.is_null())
                    {
                        Some(self.completed_host_response(item)?)
                    } else {
                        None
                    };
                    if let Some(Err(error)) = &response {
                        if matches!(
                            error.code.as_str(),
                            "REVIEW_TARGET_UNAVAILABLE"
                                | "REVIEW_TARGET_DRIFTED"
                                | "REVIEW_CANDIDATE_PROVENANCE_INVALID"
                        ) {
                            return Err(error.clone());
                        }
                        self.reverify_failed_host_read(error)?;
                        return Ok(());
                    }
                    match read_control_if_present::<IssuedRead>(&path, MAX_CONTROL_BYTES)? {
                        Some(read) => {
                            read.thread_id == thread_id
                                && read.turn_id == turn_id
                                && read.call_id == call_id
                                && read.receipt.review_id == self.binding.review_id
                                && read.receipt.target_sha256 == self.binding.target.target_sha256
                                && response.as_ref().is_none_or(|value| {
                                    value.as_ref().is_ok_and(|receipt| receipt == &read.receipt)
                                })
                        }
                        None => return Err(control_error("candidate receipt is unavailable")),
                    }
                } else {
                    false
                }
            }
            _ => false,
        };
        if !valid {
            self.record_unmediated_tool()?;
        }
        Ok(())
    }

    /// This is the existing native dynamic-tool response, not a new control
    /// record. A missing response cannot prove either issuance or a violation.
    fn completed_host_response(
        &self,
        item: &Value,
    ) -> Result<Result<CandidateReadReceipt, MachineError>, MachineError> {
        let malformed = || control_error("candidate host response cannot be verified");
        if !matches!(
            item.get("status").and_then(Value::as_str),
            Some("completed" | "failed")
        ) || item.get("namespace").is_some_and(|v| !v.is_null())
        {
            return Err(malformed());
        }
        let success = item
            .get("success")
            .and_then(Value::as_bool)
            .ok_or_else(malformed)?;
        let content = item
            .get("contentItems")
            .and_then(Value::as_array)
            .filter(|items| items.len() == 1)
            .ok_or_else(malformed)?;
        let fields = content[0]
            .as_object()
            .filter(|fields| fields.len() == 2)
            .ok_or_else(malformed)?;
        if fields.get("type").and_then(Value::as_str) != Some("inputText") {
            return Err(malformed());
        }
        let text = fields
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(malformed)?;
        if text.len() > MAX_READ_BYTES as usize * 2 + 16384 {
            return Err(malformed());
        }
        let body = crate::jcs::parse(text)
            .and_then(|v| crate::jcs::canonicalize(&v))
            .map_err(|_| malformed())?;
        let body: Value = serde_json::from_slice(&body).map_err(|_| malformed())?;
        if !success {
            let object = body
                .as_object()
                .filter(|fields| fields.len() == 1)
                .ok_or_else(malformed)?;
            let value = object
                .get("error")
                .and_then(Value::as_object)
                .filter(|fields| {
                    fields.len() == 4
                        && ["code", "message", "retryable", "details"]
                            .iter()
                            .all(|key| fields.contains_key(*key))
                })
                .ok_or_else(malformed)?;
            let error: MachineError =
                serde_json::from_value(Value::Object(value.clone())).map_err(|_| malformed())?;
            if !matches!(
                error.code.as_str(),
                "INVALID_ARGUMENT"
                    | "REVIEW_TARGET_UNAVAILABLE"
                    | "REVIEW_TARGET_DRIFTED"
                    | "REVIEW_TARGET_LIMIT_EXCEEDED"
                    | "REVIEW_CANDIDATE_PROVENANCE_INVALID"
                    | "RUNTIME_STATE_INVALID"
                    | "REVIEW_TARGET_UNSAFE_PATH"
                    | "REVIEW_TARGET_UNSAFE_FILE"
                    | "REVIEW_TARGET_GIT_FAILED"
                    | "REVIEW_TARGET_IO_FAILURE"
                    | "REVIEW_TARGET_SECRET_DETECTED"
                    | "INTERNAL_ERROR"
                    | "REVIEW_TARGET_TIMEOUT"
            ) {
                return Err(malformed());
            }
            if error.code.starts_with("REVIEW_")
                && error
                    .details
                    .get("review_id")
                    .is_some_and(|id| !id.is_null() && id != &json!(self.review_id()))
            {
                self.record_unmediated_tool()?;
                return Err(provenance_invalid(self.review_id()));
            }
            return Ok(Err(error));
        }
        let object = body
            .as_object()
            .filter(|fields| fields.len() == 3)
            .ok_or_else(malformed)?;
        let receipt: CandidateReadReceipt =
            serde_json::from_value(object.get("receipt").cloned().ok_or_else(malformed)?)
                .map_err(|_| malformed())?;
        let request: CandidateReadRequest =
            serde_json::from_value(item.get("arguments").cloned().ok_or_else(malformed)?)
                .map_err(|_| malformed())?;
        let encoded = object
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(malformed)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| malformed())?;
        if object.get("encoding").and_then(Value::as_str) != Some("base64")
            || bytes.len() as u64 != request.byte_length
            || base64::engine::general_purpose::STANDARD.encode(&bytes) != encoded
            || receipt.review_id != self.review_id()
            || receipt.target_sha256 != self.target().target_sha256
            || receipt.receipt_id.get_version_num() != 7
            || receipt.side != request.side
            || receipt.path != request.path
            || receipt.byte_offset != request.byte_offset
            || receipt.byte_length != request.byte_length
            || receipt.schema
                != if request.side == "metadata" {
                    "dolgorae-target-metadata-read-receipt/v1"
                } else {
                    "dolgorae-candidate-read-receipt/v1"
                }
        {
            self.record_unmediated_tool()?;
            return Err(provenance_invalid(self.review_id()));
        }
        Ok(Ok(receipt))
    }

    /// An unsuccessful host response does not prove that its storage or source
    /// fault has gone away. Recheck the existing evidence without reissuing the
    /// call or changing its recorded response.
    fn reverify_failed_host_read(&self, original: &MachineError) -> Result<(), MachineError> {
        let fault = matches!(
            original.code.as_str(),
            "RUNTIME_STATE_INVALID"
                | "REVIEW_TARGET_GIT_FAILED"
                | "REVIEW_TARGET_IO_FAILURE"
                | "INTERNAL_ERROR"
                | "REVIEW_TARGET_TIMEOUT"
        );
        let storage = self.verify_failed_read_storage();
        if let Err(error) = storage {
            return Err(
                if matches!(
                    error.code.as_str(),
                    "REVIEW_TARGET_UNAVAILABLE" | "REVIEW_TARGET_DRIFTED"
                ) {
                    error
                } else if fault || error.code == original.code {
                    original.clone()
                } else {
                    control_error(&error.message)
                },
            );
        }
        if fault {
            let workspace = self.workspace()?;
            for entry in &self.binding.target.entries {
                if let Err(error) = self.binding.target.file_bytes(
                    &workspace,
                    &entry.side,
                    &entry.path,
                    self.review_id(),
                ) {
                    return Err(
                        if matches!(
                            error.code.as_str(),
                            "REVIEW_TARGET_UNAVAILABLE" | "REVIEW_TARGET_DRIFTED"
                        ) {
                            error
                        } else {
                            original.clone()
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn verify_failed_read_storage(&self) -> Result<(), MachineError> {
        let _guard = self.lock()?;
        let state = self
            .root
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| control_error("reader root is invalid"))?;
        let restored = Self::open(state, self.review_id(), self.run_id())?;
        if restored.binding.schema != self.binding.schema
            || restored.binding.workspace != self.binding.workspace
            || restored.binding.workspace_mode != self.binding.workspace_mode
            || restored.binding.target != self.binding.target
        {
            return Err(control_error("reader binding changed"));
        }
        let lock: Value = read_control(&self.root.join("reader.lock"))?;
        if lock != json!({}) {
            return Err(control_error("reader lock changed"));
        }
        let metadata = self.binding.target.metadata_bytes()?;
        let mut bounds = BTreeMap::from([(
            ("metadata".to_owned(), "target.json".to_owned()),
            (sha256_hex(&metadata), metadata.len() as u64),
        )]);
        for entry in &self.binding.target.entries {
            bounds.insert(
                (entry.side.clone(), entry.path.clone()),
                (entry.sha256.clone(), entry.byte_length),
            );
        }
        for (count, entry) in fs::read_dir(self.root.join("reads"))
            .map_err(control_error)?
            .enumerate()
        {
            if count >= MAX_RECEIPTS {
                return Err(control_error("receipt directory exceeds its bound"));
            }
            let entry = entry.map_err(control_error)?;
            let read: IssuedRead = read_control(&entry.path())?;
            let receipt = &read.receipt;
            let request = CandidateReadRequest {
                side: receipt.side.clone(),
                path: receipt.path.clone(),
                byte_offset: receipt.byte_offset,
                byte_length: receipt.byte_length,
            };
            let key = value_digest(&json!([read.thread_id, read.turn_id, read.call_id]))?;
            if [
                read.thread_id.as_str(),
                read.turn_id.as_str(),
                read.call_id.as_str(),
            ]
            .iter()
            .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
                || entry.file_name() != std::ffi::OsStr::new(&format!("{key}.json"))
                || read.request_sha256
                    != value_digest(&serde_json::to_value(&request).map_err(control_error)?)?
                || receipt.schema
                    != if request.side == "metadata" {
                        "dolgorae-target-metadata-read-receipt/v1"
                    } else {
                        "dolgorae-candidate-read-receipt/v1"
                    }
                || receipt.receipt_id.get_version_num() != 7
                || receipt.review_id != self.review_id()
                || receipt.target_sha256 != self.target().target_sha256
                || request.byte_length > MAX_READ_BYTES
            {
                return Err(control_error("stored receipt cannot be verified"));
            }
            validate_path(&request.path)?;
            let source = (request.side.clone(), request.path.clone());
            if !bounds.contains_key(&source) && request.side == "diff" {
                let content = self.target().content(
                    &self.workspace()?,
                    &request.side,
                    &request.path,
                    self.review_id(),
                )?;
                bounds.insert(source.clone(), (sha256_hex(&content), content.len() as u64));
            }
            let (digest, length) = bounds
                .get(&source)
                .ok_or_else(|| control_error("stored receipt source is foreign"))?;
            if &receipt.sha256 != digest
                || request
                    .byte_offset
                    .checked_add(request.byte_length)
                    .is_none_or(|end| end > *length)
            {
                return Err(control_error("stored receipt content changed"));
            }
        }
        Ok(())
    }

    pub fn check_acceptance(&self) -> Result<(), MachineError> {
        if !self.pending_histories()?.is_empty() {
            return Err(provenance_invalid(self.review_id()));
        }
        match fs::symlink_metadata(self.root.join("provenance-violation.json")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Ok(_) => return Err(provenance_invalid(self.binding.review_id)),
            Err(error) => return Err(control_error(error)),
        }
        self.binding.target.check_drift(
            &self.binding.workspace.to_path_buf()?,
            self.binding.workspace_mode,
            self.binding.review_id,
        )
    }

    pub fn validate_location(
        &self,
        receipt_id: Uuid,
        path: &str,
        line_start: Option<u64>,
        line_end: Option<u64>,
    ) -> Result<(), MachineError> {
        let fail = || provenance_invalid(self.binding.review_id);
        self.check_acceptance()?;
        let mut found = None;
        for entry in fs::read_dir(self.root.join("reads")).map_err(control_error)? {
            let entry = entry.map_err(control_error)?;
            let issued: IssuedRead = read_control(&entry.path())?;
            if issued.receipt.receipt_id == receipt_id {
                if found.is_some() {
                    return Err(fail());
                }
                found = Some(issued.receipt);
            }
        }
        let receipt = found.ok_or_else(fail)?;
        if receipt.schema != "dolgorae-candidate-read-receipt/v1"
            || receipt.review_id != self.binding.review_id
            || receipt.target_sha256 != self.binding.target.target_sha256
            || receipt.path != path
        {
            return Err(fail());
        }
        if receipt.side == "diff" && (line_start.is_some() || line_end.is_some()) {
            return Err(fail());
        }
        let content = self.binding.target.content(
            &self.binding.workspace.to_path_buf()?,
            &receipt.side,
            path,
            self.binding.review_id,
        )?;
        let end = receipt
            .byte_offset
            .checked_add(receipt.byte_length)
            .ok_or_else(fail)?;
        if sha256_hex(&content) != receipt.sha256 || end > content.len() as u64 {
            return Err(fail());
        }
        match (line_start, line_end) {
            (None, None) => Ok(()),
            (Some(first), Some(last))
                if first > 0
                    && last >= first
                    && !content.contains(&0)
                    && std::str::from_utf8(&content).is_ok() =>
            {
                let mut starts = if content.is_empty() {
                    Vec::new()
                } else {
                    vec![0_u64]
                };
                for (offset, byte) in content.iter().enumerate() {
                    if *byte == b'\n' && offset + 1 < content.len() {
                        starts.push(offset as u64 + 1);
                    }
                }
                let first_offset = starts.get((first - 1) as usize).copied().ok_or_else(fail)?;
                let last_offset = starts
                    .get(last as usize)
                    .copied()
                    .unwrap_or(content.len() as u64);
                if last as usize > starts.len()
                    || first_offset < receipt.byte_offset
                    || last_offset > end
                {
                    return Err(fail());
                }
                Ok(())
            }
            _ => Err(fail()),
        }
    }

    fn lock(&self) -> Result<File, MachineError> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(self.root.join("reader.lock"))
            .map_err(control_error)?;
        let metadata = lock.metadata().map_err(control_error)?;
        if !metadata.is_file()
            || metadata.uid() != current_uid()
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(provenance_invalid(self.binding.review_id));
        }
        lock.lock().map_err(control_error)?;
        Ok(lock)
    }
}

pub fn reading_boundary_unsupported() -> MachineError {
    review_error(
        "REVIEW_READING_BOUNDARY_UNSUPPORTED",
        "the runtime has no qualified candidate-reading boundary",
        "select_supported_runtime",
        "admission",
        "not_dispatched",
        None,
    )
}

pub fn provenance_invalid(review_id: Uuid) -> MachineError {
    review_error(
        "REVIEW_CANDIDATE_PROVENANCE_INVALID",
        "candidate read provenance is violated or unverifiable",
        "inspect_review",
        "acceptance",
        "inconclusive",
        Some(review_id),
    )
}

fn unavailable(review_id: Option<Uuid>) -> MachineError {
    review_error(
        "REVIEW_TARGET_UNAVAILABLE",
        "the admitted candidate source is unavailable",
        "restore_target",
        "acceptance",
        "failed",
        review_id,
    )
}

fn review_error(
    code: &'static str,
    message: &'static str,
    action: &'static str,
    phase: &'static str,
    outcome: &'static str,
    review_id: Option<Uuid>,
) -> MachineError {
    MachineError::new(
        code,
        message,
        false,
        json!({"schema":"dolgorae-workspace-error-details/v1","required_action":action,"phase":phase,"outcome":outcome,"review_id":review_id,"run_id":null}),
    )
}

fn value_digest(value: &Value) -> Result<String, MachineError> {
    Ok(sha256_hex(
        &canonicalize(&parse(&value.to_string()).map_err(control_error)?).map_err(control_error)?,
    ))
}

fn validate_selection(kind: &str, revision: Option<&str>) -> Result<(), MachineError> {
    match (kind, revision) {
        ("workspace" | "staged" | "dirty" | "head", None) => Ok(()),
        ("commit", Some(value)) if !value.is_empty() && value.len() <= 1024 => Ok(()),
        ("range", Some(value)) if value.len() <= 1024 => split_range(value).map(|_| ()),
        _ => Err(MachineError::invalid_argument(
            "target",
            "invalid direct target selection",
        )),
    }
}

fn validate_path(path: &str) -> Result<(), MachineError> {
    if path.len() > MAX_PATH_BYTES
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || is_private_tool_path(path)
        || path.split('/').any(|part| part == ".git")
    {
        return Err(MachineError::invalid_argument(
            "candidate.path",
            "unsafe or excluded candidate path",
        ));
    }
    safe_join(Path::new("/"), path).map(|_| ())
}

fn non_git_entries(root: &Path) -> Result<Vec<Candidate>, MachineError> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut Vec<Candidate>,
        total: &mut usize,
    ) -> Result<(), MachineError> {
        for entry in fs::read_dir(directory).map_err(control_error)? {
            let entry = entry.map_err(control_error)?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(control_error)?
                .to_str()
                .ok_or_else(|| {
                    MachineError::invalid_argument("candidate.path", "opaque paths are unsupported")
                })?
                .to_owned();
            if is_private_tool_path(&relative) || relative.split('/').any(|part| part == ".git") {
                continue;
            }
            validate_path(&relative)?;
            let kind = entry.file_type().map_err(control_error)?;
            if kind.is_dir() {
                visit(root, &path, entries, total)?;
            } else {
                let (metadata, bytes) =
                    read_worktree_file(root, &relative)?.ok_or_else(|| unavailable(None))?;
                *total = total.checked_add(bytes.len()).ok_or_else(limit)?;
                if entries.len() >= MAX_FILES || *total > MAX_SIDE_BYTES {
                    return Err(limit());
                }
                entries.push(Candidate {
                    path: relative,
                    mode: if metadata.mode() & 0o111 == 0 {
                        0o444
                    } else {
                        0o555
                    },
                    bytes,
                    blob_oid: None,
                });
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    visit(root, root, &mut entries, &mut 0)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn replacement_diff(path: &str, before: &[u8], after: &[u8]) -> Result<Vec<u8>, MachineError> {
    let before = std::str::from_utf8(before).map_err(|_| {
        MachineError::invalid_argument("candidate_read", "binary transition requires side reads")
    })?;
    let after = std::str::from_utf8(after).map_err(|_| {
        MachineError::invalid_argument("candidate_read", "binary transition requires side reads")
    })?;
    if before == after {
        return Ok(Vec::new());
    }
    let mut diff = format!(
        "--- before/{path}\n+++ after/{path}\n@@ -1,{} +1,{} @@\n",
        before.lines().count(),
        after.lines().count()
    );
    for (prefix, text) in [("-", before), ("+", after)] {
        for line in text.split_inclusive('\n') {
            diff.push_str(prefix);
            diff.push_str(line);
            if !line.ends_with('\n') {
                diff.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    Ok(diff.into_bytes())
}

fn git_identity(bytes: &[u8]) -> Result<String, MachineError> {
    let value = std::str::from_utf8(bytes).map_err(control_error)?.trim();
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(unavailable(None));
    }
    Ok(value.to_owned())
}

fn current_uid() -> u32 {
    SystemWorkspacePlatform.current_uid()
}

fn create_or_verify(path: &Path) -> Result<(), MachineError> {
    match create_directory(path, 0o700) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(control_error(error)),
    };
    verify_secure_directory(path, current_uid())
}

fn publish(path: &Path, value: &impl Serialize) -> Result<(), MachineError> {
    publish_bounded(path, value, MAX_CONTROL_BYTES)
}

fn publish_bounded(path: &Path, value: &impl Serialize, maximum: u64) -> Result<(), MachineError> {
    let bytes = serde_json::to_vec(value).map_err(control_error)?;
    if bytes.len() as u64 > maximum {
        return Err(limit());
    }
    atomic_create(&SystemWorkspacePlatform, path, &bytes, 0o600).map_err(control_error)
}

fn read_control<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, MachineError> {
    read_control_bounded(path, MAX_CONTROL_BYTES)
}

fn read_control_bounded<T: for<'de> Deserialize<'de>>(
    path: &Path,
    maximum: u64,
) -> Result<T, MachineError> {
    read_control_if_present(path, maximum)?.ok_or_else(|| control_error("missing control file"))
}

fn read_control_if_present<T: for<'de> Deserialize<'de>>(
    path: &Path,
    maximum: u64,
) -> Result<Option<T>, MachineError> {
    let Some(file) = open_secure_file_if_present(path, current_uid())? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(control_error)?;
    if bytes.len() as u64 > maximum {
        return Err(limit());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| control_error("invalid control JSON"))
}

fn control_error(_error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "RUNTIME_STATE_INVALID",
        "direct review control state is unavailable or invalid",
        false,
        json!({"required_action":"inspect_state"}),
    )
}
fn limit() -> MachineError {
    MachineError::new(
        "REVIEW_TARGET_LIMIT_EXCEEDED",
        "direct candidate exceeds its bounded source limits",
        false,
        json!({"required_action":"none"}),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    thread_local! {
        static SOURCE_READ_FAULT: std::cell::RefCell<Option<PathBuf>> = const {std::cell::RefCell::new(None)};
    }
    pub(crate) struct SourceReadFault(Option<PathBuf>);
    impl SourceReadFault {
        pub(crate) fn at(root: &Path) -> Self {
            Self(SOURCE_READ_FAULT.with(|state| state.replace(Some(root.to_owned()))))
        }
    }
    impl Drop for SourceReadFault {
        fn drop(&mut self) {
            SOURCE_READ_FAULT.with(|state| state.replace(self.0.take()));
        }
    }
    pub(super) fn source_read_fault(root: &Path) -> Result<(), MachineError> {
        if SOURCE_READ_FAULT.with(|state| state.borrow().as_deref() == Some(root)) {
            Err(MachineError::new(
                "REVIEW_TARGET_IO_FAILURE",
                "source read failed",
                false,
                json!({}),
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) fn fill_receipt_directory(reader: &DirectTargetReader) {
        let request = CandidateReadRequest {
            side: "metadata".to_owned(),
            path: "target.json".to_owned(),
            byte_offset: 0,
            byte_length: 1,
        };
        reader
            .read("thread-1", "turn-1", "capacity-0", &request)
            .unwrap();
        let first = fs::read_dir(reader.root.join("reads"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let template: IssuedRead = read_control(&first).unwrap();
        for index in 1..MAX_RECEIPTS {
            let mut record = template.clone();
            record.call_id = format!("capacity-{index}");
            record.receipt.receipt_id = new_uuid_v7();
            let key =
                value_digest(&json!([record.thread_id, record.turn_id, record.call_id])).unwrap();
            let path = reader.root.join("reads").join(format!("{key}.json"));
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    pub(crate) fn oversize_control_file(path: &Path) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(MAX_CONTROL_BYTES + 1)
            .unwrap();
    }

    pub(crate) struct Fixture {
        root: PathBuf,
        pub(crate) source: PathBuf,
        pub(crate) state: PathBuf,
        home: PathBuf,
        mode: WorkspaceMode,
    }

    #[test]
    fn recorded_candidate_response_cases_remain_distinct() {
        for case in 0..16 {
            let fixture = Fixture::new(false);
            fs::write(fixture.source.join("file.txt"), b"candidate\n").unwrap();
            let reader = fixture.reader(fixture.admit("workspace", None));
            let request = CandidateReadRequest {
                side: "current".to_owned(),
                path: "file.txt".to_owned(),
                byte_offset: 0,
                byte_length: 10,
            };
            let response = reader.read("thread-1", "turn-1", "read", &request).unwrap();
            let mut item = json!({"type":"dynamicToolCall","id":"read","tool":"dolgorae_candidate_read",
                "status":"completed","success":true,"arguments":request,
                "contentItems":[{"type":"inputText","text":response.to_string()}]});
            let path = fs::read_dir(reader.root.join("reads"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let bytes = fs::read(&path).unwrap();
            if matches!(case, 0 | 1 | 3 | 14) {
                item.as_object_mut().unwrap().remove("success");
                item.as_object_mut().unwrap().remove("contentItems");
            }
            match case {
                1 | 2 => fs::remove_file(&path).unwrap(),
                3 => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
                4 => fs::write(&path, b"broken JSON").unwrap(),
                5 | 14 => {
                    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
                    value["receipt"]["review_id"] = json!(new_uuid_v7());
                    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                6 => {
                    item.as_object_mut().unwrap().remove("contentItems");
                }
                7 => item["status"] = json!("in_progress"),
                8 => item["namespace"] = json!("foreign"),
                9 => item["contentItems"][0]["text"] = json!("not JSON"),
                10 => {
                    item["success"] = json!(false);
                    item["contentItems"][0]["text"]=json!(json!({"error":MachineError::new("UNKNOWN_FAILURE","unknown",false,json!({}))}).to_string());
                }
                11 => {
                    item["contentItems"][0]["text"] = json!("{\"receipt\":null,\"receipt\":null}")
                }
                12 => {
                    let mut value = response;
                    value["receipt"]["review_id"] = json!(new_uuid_v7());
                    item["contentItems"][0]["text"] = json!(value.to_string());
                }
                13 => item["threadId"] = json!("foreign-thread"),
                15 => {
                    oversize_control_file(&path);
                    item["success"] = json!(false);
                    item["contentItems"][0]["text"] = json!(json!({"error":limit()}).to_string());
                }
                _ => {}
            }
            let result = reader.observe_item("thread-1", "turn-1", &item, true);
            if case == 15 {
                assert_eq!(
                    result.as_ref().unwrap_err().code,
                    "REVIEW_TARGET_LIMIT_EXCEEDED"
                );
            }
            let violation = matches!(case, 5 | 12 | 13 | 14);
            assert_eq!(
                reader.root.join("provenance-violation.json").exists(),
                violation,
                "case={case}"
            );
            assert_eq!(
                result.is_ok(),
                case == 0 || matches!(case, 5 | 13 | 14),
                "case={case}: {result:?}"
            );
            if matches!(case, 1 | 2 | 3 | 4 | 15) {
                fs::write(&path, &bytes).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                assert!(
                    reader
                        .observe_item("thread-1", "turn-1", &item, true)
                        .is_ok(),
                    "case={case}"
                );
            }
        }
    }

    impl Fixture {
        pub(crate) fn new(git: bool) -> Self {
            let root =
                std::env::temp_dir().join(format!("dolgorae-direct-review-{}", new_uuid_v7()));
            create_directory(&root, 0o700).unwrap();
            let source = root.join("source");
            let state = root.join("state");
            let home = root.join("home");
            for path in [&source, &state, &home] {
                create_directory(path, 0o700).unwrap();
            }
            let fixture = Self {
                root,
                source,
                state,
                home,
                mode: if git {
                    WorkspaceMode::Git
                } else {
                    WorkspaceMode::NonGit
                },
            };
            if git {
                fixture.git(&["init", "-q"]);
                fixture.git(&["config", "user.name", "Direct Review Fixture"]);
                fixture.git(&["config", "user.email", "fixture@example.invalid"]);
            }
            fixture
        }
        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("/usr/bin/git")
                .arg("-C")
                .arg(&self.source)
                .args(args)
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_CONFIG_COUNT")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "Git fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }
        pub(crate) fn commit(&self, content: &str) -> String {
            fs::write(self.source.join("file.txt"), content).unwrap();
            self.git(&["add", "file.txt"]);
            self.git(&["commit", "-qm", "fixture change"]);
            self.git(&["rev-parse", "HEAD"])
        }
        pub(crate) fn admit(&self, kind: &str, revision: Option<&str>) -> DirectTarget {
            DirectTarget::admit(&self.source, self.mode, kind, revision, new_uuid_v7()).unwrap()
        }
        pub(crate) fn reader(&self, target: DirectTarget) -> DirectTargetReader {
            DirectTargetReader::create(
                &self.state,
                &self.source,
                self.mode,
                new_uuid_v7(),
                new_uuid_v7(),
                target,
            )
            .unwrap()
        }

        pub(crate) fn maximal_inventory(&self) -> DirectTarget {
            // Exercise retained metadata capacity with entries based on a real
            // admitted blob; source selection itself has separate Git fixtures.
            let mut target = self.admit("range", Some("HEAD..HEAD"));
            let entry = target.entries[0].clone();
            target.entries = ["before", "after"]
                .into_iter()
                .flat_map(|side| {
                    (0..MAX_FILES).map({
                        let entry = entry.clone();
                        move |index| {
                            let mut entry = entry.clone();
                            entry.side = side.to_owned();
                            entry.path = format!("{index:05}-{}", "x".repeat(200));
                            entry
                        }
                    })
                })
                .collect();
            target
                .entries
                .sort_by(|a, b| (&a.side, &a.path).cmp(&(&b.side, &b.path)));
            target.admission_fingerprint = value_digest(
                &json!({"base":target.resolved_base,"head":target.resolved_head,"entries":target.entries}),
            ).unwrap();
            target.target_sha256 = target.metadata_digest().unwrap();
            target
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn reusable_reader_keeps_each_task_target_and_receipts_after_source_changes() {
        let fixture = Fixture::new(false);
        fs::write(fixture.source.join("file.txt"), b"first\n").unwrap();
        let run_id = new_uuid_v7();
        let first = new_uuid_v7();
        let target = crate::review::ReviewTargetRequest {
            kind: "workspace".to_owned(),
            revision: None,
        };
        let reader = DirectTargetReader::create_for_task(
            &fixture.state,
            &fixture.source,
            fixture.mode,
            first,
            run_id,
            &target,
        )
        .unwrap();
        let query = request("current", 6);
        let original = reader.read("thread", "first-turn", "call", &query).unwrap();
        let original_sha = reader.target().target_sha256.clone();
        fs::write(fixture.source.join("file.txt"), b"later\n").unwrap();
        let second = new_uuid_v7();
        let later = DirectTargetReader::create_for_task(
            &fixture.state,
            &fixture.source,
            fixture.mode,
            second,
            run_id,
            &target,
        )
        .unwrap();
        assert_ne!(later.target().target_sha256, original_sha);
        let reopened = DirectTargetReader::create_for_task(
            &fixture.state,
            &fixture.source,
            fixture.mode,
            first,
            run_id,
            &target,
        )
        .unwrap();
        assert_eq!(reopened.target().target_sha256, original_sha);
        assert_eq!(
            reopened
                .replay_read("thread", "first-turn", "call", &query)
                .unwrap_err()
                .code,
            "REVIEW_TARGET_DRIFTED"
        );
        assert_eq!(
            reopened.check_acceptance().unwrap_err().code,
            "REVIEW_TARGET_DRIFTED"
        );
        let receipt = original["receipt"]["receipt_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            later
                .validate_location(receipt, "file.txt", Some(1), Some(1))
                .is_err()
        );
        assert!(
            DirectTargetReader::for_run(&fixture.state, run_id)
                .unwrap()
                .is_none()
        );
    }

    fn request(side: &str, length: u64) -> CandidateReadRequest {
        CandidateReadRequest {
            side: side.to_owned(),
            path: "file.txt".to_owned(),
            byte_offset: 0,
            byte_length: length,
        }
    }
    fn bytes(value: &Value) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(value["content"].as_str().unwrap())
            .unwrap()
    }

    #[test]
    fn direct_index_objects_and_workspace_never_substitute_each_other() {
        let fixture = Fixture::new(true);
        let commit = fixture.commit("committed\n");
        fs::write(fixture.source.join("file.txt"), "indexed\n").unwrap();
        fixture.git(&["add", "file.txt"]);
        fs::write(fixture.source.join("file.txt"), "live-worktree\n").unwrap();
        fs::write(fixture.source.join("new.txt"), "untracked\n").unwrap();
        let staged = fixture.reader(fixture.admit("staged", None));
        let historical = fixture.reader(fixture.admit("head", None));
        let staged_read = staged
            .read("thread", "turn", "index-call", &request("after", 8))
            .unwrap();
        assert_eq!(bytes(&staged_read), b"indexed\n");
        assert_eq!(
            bytes(
                &historical
                    .read("thread", "turn", "object-call", &request("current", 10))
                    .unwrap()
            ),
            b"committed\n"
        );
        let dirty = fixture.admit("dirty", None);
        assert_eq!(
            dirty
                .entries
                .iter()
                .find(|entry| entry.side == "after" && entry.path == "file.txt")
                .unwrap()
                .sha256,
            sha256_hex(b"live-worktree\n")
        );
        assert!(
            fixture
                .admit("workspace", None)
                .entries
                .iter()
                .any(|entry| entry.path == "new.txt")
        );
        let root_commit = fixture.admit("commit", Some(&commit));
        assert!(root_commit.resolved_base.is_none());
        assert!(
            root_commit
                .entries
                .iter()
                .all(|entry| entry.side == "after")
        );
        let before = fixture.git(&["status", "--porcelain"]);
        let worktrees = fixture.git(&["worktree", "list", "--porcelain"]);
        fs::write(fixture.source.join("file.txt"), "later-worktree\n").unwrap();
        staged.check_acceptance().unwrap();
        historical.check_acceptance().unwrap();
        assert_eq!(
            bytes(
                &staged
                    .read("thread", "turn", "index-call", &request("after", 8))
                    .unwrap()
            ),
            b"indexed\n"
        );
        assert_eq!(fixture.git(&["worktree", "list", "--porcelain"]), worktrees);
        assert_eq!(fixture.git(&["status", "--porcelain"]), before);
        assert!(!fixture.state.join("source").exists());
    }

    #[test]
    fn direct_range_pins_both_endpoints_and_the_effective_base() {
        let fixture = Fixture::new(true);
        let base = fixture.commit("base\n");
        fixture.git(&["branch", "left"]);
        let left = fixture.commit("left\n");
        fixture.git(&["branch", "-f", "left", &left]);
        fixture.git(&["checkout", "-qb", "right", &base]);
        let right = fixture.commit("right\n");
        let two = fixture.admit("range", Some("left..right"));
        let three = fixture.admit("range", Some("left...right"));
        assert_eq!(two.resolved_base.as_deref(), Some(left.as_str()));
        assert_eq!(three.resolved_base.as_deref(), Some(base.as_str()));
        assert_eq!(three.range.as_ref().unwrap().left_oid, left);
        assert_eq!(three.range.as_ref().unwrap().right_oid, right);
        fixture.git(&["update-ref", "refs/heads/left", &right]);
        fixture.git(&["update-ref", "refs/heads/right", &base]);
        let reader = fixture.reader(three);
        assert_eq!(
            bytes(
                &reader
                    .read("thread", "turn", "pinned", &request("after", 6))
                    .unwrap()
            ),
            b"right\n"
        );
        reader.check_acceptance().unwrap();
        assert_eq!(
            reader.target().resolved_head.as_deref(),
            Some(right.as_str())
        );
    }

    #[test]
    fn direct_mutable_drift_rejects_index_head_deletion_and_untracked_changes() {
        let fixture = Fixture::new(true);
        fixture.commit("original\n");
        let staged = fixture.admit("staged", None);
        let workspace = fixture.admit("workspace", None);
        let dirty = fixture.admit("dirty", None);
        fs::write(fixture.source.join("new.txt"), "new\n").unwrap();
        staged
            .check_drift(&fixture.source, fixture.mode, new_uuid_v7())
            .unwrap();
        for target in [&workspace, &dirty] {
            assert_eq!(
                target
                    .check_drift(&fixture.source, fixture.mode, new_uuid_v7())
                    .unwrap_err()
                    .code,
                "REVIEW_TARGET_DRIFTED"
            );
        }
        fs::remove_file(fixture.source.join("new.txt")).unwrap();
        fs::remove_file(fixture.source.join("file.txt")).unwrap();
        assert_eq!(
            workspace
                .check_drift(&fixture.source, fixture.mode, new_uuid_v7())
                .unwrap_err()
                .code,
            "REVIEW_TARGET_DRIFTED"
        );
        fs::write(fixture.source.join("file.txt"), "changed-index\n").unwrap();
        fixture.git(&["add", "file.txt"]);
        assert_eq!(
            staged
                .check_drift(&fixture.source, fixture.mode, new_uuid_v7())
                .unwrap_err()
                .code,
            "REVIEW_TARGET_DRIFTED"
        );
    }

    #[test]
    fn direct_receipts_survive_restart_and_never_accept_forgery_or_native_reads() {
        let fixture = Fixture::new(true);
        fixture.commit("first\nsecond\n");
        let reader = fixture.reader(fixture.admit("head", None));
        let response = reader
            .read("thread", "turn", "original-call", &request("current", 6))
            .unwrap();
        let receipt: CandidateReadReceipt =
            serde_json::from_value(response["receipt"].clone()).unwrap();
        reader
            .validate_location(receipt.receipt_id, "file.txt", Some(1), Some(1))
            .unwrap();
        assert_eq!(
            reader
                .validate_location(receipt.receipt_id, "file.txt", Some(2), Some(2))
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        assert_eq!(
            reader
                .validate_location(new_uuid_v7(), "file.txt", None, None)
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        let reopened = DirectTargetReader::open(
            &fixture.state,
            reader.binding.review_id,
            reader.binding.run_id,
        )
        .unwrap();
        assert_eq!(
            reopened
                .read("thread", "turn", "original-call", &request("current", 6))
                .unwrap(),
            response
        );
        assert_eq!(
            reopened
                .read("thread", "turn", "original-call", &request("current", 13))
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        reopened.record_unmediated_tool().unwrap();
        let reopened = DirectTargetReader::open(
            &fixture.state,
            reader.binding.review_id,
            reader.binding.run_id,
        )
        .unwrap();
        assert_eq!(
            reopened.check_acceptance().unwrap_err().code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        assert_eq!(
            reopened
                .validate_location(receipt.receipt_id, "file.txt", Some(1), Some(1))
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
    }

    #[test]
    fn escaped_retained_inventory_reopens_above_small_control_record_limits() {
        let fixture = Fixture::new(true);
        fixture.commit("candidate\n");
        let mut target = fixture.maximal_inventory();
        for entry in &mut target.entries {
            entry.path = format!("{}-{}", "\u{1}".repeat(600), entry.path);
        }
        target.admission_fingerprint = value_digest(
            &json!({"base":target.resolved_base,"head":target.resolved_head,"entries":target.entries}),
        ).unwrap();
        target.target_sha256 = target.metadata_digest().unwrap();
        let metadata_length = target.metadata_bytes().unwrap().len() as u64;
        assert!(metadata_length > MAX_CONTROL_BYTES);
        assert!(metadata_length < MAX_BINDING_BYTES);
        let reader = fixture.reader(target);
        let run = reader.run_id();
        let request = CandidateReadRequest {
            side: "metadata".to_owned(),
            path: "target.json".to_owned(),
            byte_offset: metadata_length - 16,
            byte_length: 16,
        };
        let original = reader
            .read("thread", "turn", "last-page", &request)
            .unwrap();
        assert!(
            fs::metadata(reader.root.join("binding.json"))
                .unwrap()
                .len()
                > MAX_CONTROL_BYTES
        );
        drop(reader);
        let reopened = DirectTargetReader::for_run(&fixture.state, run)
            .unwrap()
            .unwrap();
        assert_eq!(reopened.target().entries.len(), 20_000);
        assert_eq!(
            reopened
                .replay_read("thread", "turn", "last-page", &request)
                .unwrap(),
            original
        );
        reopened.check_acceptance().unwrap();
        let bounded = fixture.root.join("bounded.json");
        assert!(publish_bounded(&bounded, &"escaped\n", 4).is_err());
        assert!(!bounded.exists());
    }

    #[test]
    fn direct_non_git_and_missing_objects_fail_without_materialization() {
        let non_git = Fixture::new(false);
        fs::write(non_git.source.join("file.txt"), "caller bytes\n").unwrap();
        let reader = non_git.reader(non_git.admit("workspace", None));
        assert_eq!(
            bytes(
                &reader
                    .read("thread", "turn", "non-git", &request("current", 13))
                    .unwrap()
            ),
            b"caller bytes\n"
        );
        for kind in ["staged", "dirty", "head", "commit", "range"] {
            assert!(
                DirectTarget::admit(
                    &non_git.source,
                    non_git.mode,
                    kind,
                    match kind {
                        "commit" => Some("HEAD"),
                        "range" => Some("HEAD..HEAD"),
                        _ => None,
                    },
                    new_uuid_v7()
                )
                .is_err()
            );
        }
        let fixture = Fixture::new(true);
        fixture.commit("stored\n");
        let target = fixture.admit("head", None);
        let oid = target.entries[0].blob_oid.clone().unwrap();
        let reader = fixture.reader(target);
        fs::remove_file(
            fixture
                .source
                .join(".git/objects")
                .join(&oid[..2])
                .join(&oid[2..]),
        )
        .unwrap();
        assert_eq!(
            reader
                .read("thread", "turn", "missing-object", &request("current", 7))
                .unwrap_err()
                .code,
            "REVIEW_TARGET_UNAVAILABLE"
        );
        assert_eq!(
            fs::read(fixture.source.join("file.txt")).unwrap(),
            b"stored\n"
        );
        assert_eq!(
            reading_boundary_unsupported().details["required_action"],
            "select_supported_runtime"
        );
    }

    #[test]
    fn diff_receipts_cannot_attest_to_numbered_file_lines() {
        let fixture = Fixture::new(true);
        fixture.commit("before\n");
        fs::write(fixture.source.join("file.txt"), "after\n").unwrap();
        fixture.git(&["add", "file.txt"]);
        let reader = fixture.reader(fixture.admit("staged", None));
        let response = reader
            .read("thread", "turn", "diff", &request("diff", 10))
            .unwrap();
        let receipt: CandidateReadReceipt =
            serde_json::from_value(response["receipt"].clone()).unwrap();
        reader
            .validate_location(receipt.receipt_id, "file.txt", None, None)
            .unwrap();
        assert_eq!(
            reader
                .validate_location(receipt.receipt_id, "file.txt", Some(1), Some(1))
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        let response = reader
            .read("thread", "turn", "side", &request("after", 6))
            .unwrap();
        let receipt: CandidateReadReceipt =
            serde_json::from_value(response["receipt"].clone()).unwrap();
        reader
            .validate_location(receipt.receipt_id, "file.txt", Some(1), Some(1))
            .unwrap();
    }

    #[test]
    fn shallow_boundary_preserves_the_actual_commit_parent() {
        let fixture = Fixture::new(true);
        let parent = fixture.commit("parent\n");
        let child = fixture.commit("child\n");
        fs::write(fixture.source.join(".git/shallow"), format!("{child}\n")).unwrap();
        assert_eq!(
            first_parent(&fixture.source, &child).unwrap().as_deref(),
            Some(parent.as_str())
        );
        let target = fixture.admit("commit", Some(&child));
        assert_eq!(target.resolved_base.as_deref(), Some(parent.as_str()));
        assert!(target.entries.iter().any(|entry| entry.side == "before"));
        fs::remove_file(
            fixture
                .source
                .join(".git/objects")
                .join(&parent[..2])
                .join(&parent[2..]),
        )
        .unwrap();
        let review_id = new_uuid_v7();
        let error = DirectTarget::admit(
            &fixture.source,
            fixture.mode,
            "commit",
            Some(&child),
            review_id,
        )
        .unwrap_err();
        assert_eq!(error.code, "REVIEW_TARGET_UNAVAILABLE");
        assert_eq!(error.details["review_id"], review_id.to_string());
        assert_eq!(error.details["phase"], "admission");
        assert_eq!(error.details["outcome"], "not_dispatched");
    }

    #[test]
    fn missing_head_object_is_unavailable_instead_of_an_unborn_repository() {
        let fixture = Fixture::new(true);
        assert!(head(&fixture.source).unwrap().is_none());
        let commit = fixture.commit("committed\n");
        fs::remove_file(
            fixture
                .source
                .join(".git/objects")
                .join(&commit[..2])
                .join(&commit[2..]),
        )
        .unwrap();
        assert_eq!(
            head(&fixture.source).unwrap_err().code,
            "REVIEW_TARGET_UNAVAILABLE"
        );
        for kind in ["head", "staged", "dirty"] {
            let review_id = new_uuid_v7();
            let error = DirectTarget::admit(&fixture.source, fixture.mode, kind, None, review_id)
                .unwrap_err();
            assert_eq!(error.code, "REVIEW_TARGET_UNAVAILABLE");
            assert_eq!(error.details["phase"], "admission");
            assert_eq!(error.details["review_id"], review_id.to_string());
        }
    }

    #[test]
    fn run_binding_reopens_only_the_original_reader_and_keeps_tool_violations() {
        let fixture = Fixture::new(true);
        fixture.commit("candidate\n");
        let reader = fixture.reader(fixture.admit("head", None));
        assert!(
            DirectTargetReader::for_run(&fixture.state, new_uuid_v7())
                .unwrap()
                .is_none()
        );
        let run = reader.run_id();
        let review = reader.review_id();
        let response = reader
            .read("thread", "turn", "call", &request("current", 10))
            .unwrap();
        reader
            .observe_item(
                "thread",
                "turn",
                &json!({"type":"dynamicToolCall","id":"call","tool":"dolgorae_candidate_read"}),
                true,
            )
            .unwrap();
        reader.check_acceptance().unwrap();
        let reopened = DirectTargetReader::for_run(&fixture.state, run)
            .unwrap()
            .unwrap();
        assert_eq!(reopened.review_id(), review);
        assert_eq!(
            reopened
                .read("thread", "turn", "call", &request("current", 10))
                .unwrap(),
            response
        );
        reopened
            .observe_item(
                "thread",
                "turn",
                &json!({"type":"commandExecution","id":"native","command":"cat file.txt"}),
                true,
            )
            .unwrap();
        assert_eq!(
            DirectTargetReader::for_run(&fixture.state, run)
                .unwrap()
                .unwrap()
                .check_acceptance()
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
    }

    #[test]
    fn non_git_mode_does_not_inherit_the_ancestor_repository() {
        let fixture = Fixture::new(true);
        fixture.commit("ancestor\n");
        let child = fixture.source.join("child");
        create_directory(&child, 0o700).unwrap();
        fs::write(child.join("file.txt"), "caller\n").unwrap();
        fs::write(fixture.source.join(".gitignore"), "child/ignored.txt\n").unwrap();
        fs::write(child.join("ignored.txt"), "non-git eligible\n").unwrap();
        let review = new_uuid_v7();
        let target =
            DirectTarget::admit(&child, WorkspaceMode::NonGit, "workspace", None, review).unwrap();
        assert_eq!(target.entries.len(), 2);
        let reader = DirectTargetReader::create(
            &fixture.state,
            &child,
            WorkspaceMode::NonGit,
            review,
            new_uuid_v7(),
            target,
        )
        .unwrap();
        reader.check_acceptance().unwrap();
        fs::write(child.join("ignored.txt"), "drift\n").unwrap();
        assert_eq!(
            reader.check_acceptance().unwrap_err().code,
            "REVIEW_TARGET_DRIFTED"
        );
    }

    #[test]
    fn pending_history_survives_terminal_crash_and_requires_original_readback() {
        let fixture = Fixture::new(true);
        fixture.commit("candidate\n");
        let reader = fixture.reader(fixture.admit("head", None));
        let run = reader.run_id();
        reader.begin_history("thread", "turn").unwrap();
        let reopened = DirectTargetReader::for_run(&fixture.state, run)
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened.pending_histories().unwrap(),
            vec![("thread".to_owned(), "turn".to_owned())]
        );
        assert_eq!(
            reopened.check_acceptance().unwrap_err().code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        assert!(
            reopened
                .finish_history("foreign-thread", "turn", &[])
                .is_err()
        );
        reopened
            .finish_history(
                "thread",
                "turn",
                &[json!({"type":"agentMessage","text":"clean"})],
            )
            .unwrap();
        reopened.check_acceptance().unwrap();
        reopened.begin_history("thread", "next").unwrap();
        reopened
            .finish_history(
                "thread",
                "next",
                &[json!({"type":"mcpToolCall","id":"unmediated"})],
            )
            .unwrap();
        assert_eq!(
            DirectTargetReader::for_run(&fixture.state, run)
                .unwrap()
                .unwrap()
                .check_acceptance()
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
    }

    #[test]
    fn replay_only_reads_never_mint_missing_receipts() {
        let fixture = Fixture::new(true);
        fixture.commit("candidate\n");
        let reader = fixture.reader(fixture.admit("head", None));
        assert_eq!(
            reader
                .replay_read("thread", "turn", "missing", &request("current", 10))
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        assert_eq!(fs::read_dir(reader.root.join("reads")).unwrap().count(), 0);
        let response = reader
            .read("thread", "turn", "issued", &request("current", 10))
            .unwrap();
        assert_eq!(
            reader
                .replay_read("thread", "turn", "issued", &request("current", 10))
                .unwrap(),
            response
        );
    }

    #[test]
    fn direct_admission_rejects_secrets_links_and_oversized_sources() {
        let secret = Fixture::new(false);
        fs::write(
            secret.source.join("file.txt"),
            "-----BEGIN PRIVATE KEY-----\nfixture-only\n",
        )
        .unwrap();
        assert_eq!(
            DirectTarget::admit(
                &secret.source,
                secret.mode,
                "workspace",
                None,
                new_uuid_v7()
            )
            .unwrap_err()
            .code,
            "REVIEW_TARGET_SECRET_DETECTED"
        );
        let linked = Fixture::new(false);
        fs::write(linked.root.join("outside"), "outside\n").unwrap();
        std::os::unix::fs::symlink(linked.root.join("outside"), linked.source.join("file.txt"))
            .unwrap();
        assert_eq!(
            DirectTarget::admit(
                &linked.source,
                linked.mode,
                "workspace",
                None,
                new_uuid_v7()
            )
            .unwrap_err()
            .code,
            "REVIEW_TARGET_UNSAFE_FILE"
        );
        let oversized = Fixture::new(false);
        File::create(oversized.source.join("file.txt"))
            .unwrap()
            .set_len(MAX_FILE_BYTES as u64 + 1)
            .unwrap();
        assert_eq!(
            DirectTarget::admit(
                &oversized.source,
                oversized.mode,
                "workspace",
                None,
                new_uuid_v7()
            )
            .unwrap_err()
            .code,
            "REVIEW_TARGET_LIMIT_EXCEEDED"
        );
        let safe = Fixture::new(false);
        fs::write(safe.source.join("file.txt"), "safe\n").unwrap();
        let reader = safe.reader(safe.admit("workspace", None));
        assert!(
            reader
                .read(
                    "thread",
                    "turn",
                    "escape",
                    &CandidateReadRequest {
                        side: "current".to_owned(),
                        path: "../outside".to_owned(),
                        byte_offset: 0,
                        byte_length: 1
                    }
                )
                .is_err()
        );
        assert_eq!(fs::read_dir(reader.root.join("reads")).unwrap().count(), 0);
    }
}
