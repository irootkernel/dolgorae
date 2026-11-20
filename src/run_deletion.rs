//! Workspace-owned deletion records. The UUID path never becomes available
//! for publication again, even to a build that cannot read the tombstone.

use crate::darwin::DarwinSystem;
use crate::delegation_identity::OperationReceipt;
use crate::domain::RunLifecycle;
use crate::machine::MachineError;
use crate::run::{ControllerBinding, run_root};
use crate::workspace::{
    SystemWorkspacePlatform, WorkspacePlatform as _, atomic_create, create_directory,
    open_secure_file_if_present, sync_directory, verify_secure_directory,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MARKER: &str = "run-deletion.json";
const MAX_MARKER_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunDeletion {
    schema_version: u32,
    pub(crate) workspace_id: String,
    run_id: Uuid,
    pub(crate) lifecycle: RunLifecycle,
    pub(crate) controller: ControllerBinding,
    pub(crate) start_receipt: Option<OperationReceipt>,
    source: DirectoryIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) force: Option<ForceRetirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForceRetirement {
    pub(crate) operator_id: Uuid,
    pub(crate) operator_generation: u64,
    pub(crate) profile: String,
    pub(crate) server_key: String,
    pub(crate) worker_generation: u64,
    pub(crate) writer_generation: Option<u64>,
    pub(crate) start_outcome: StartOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) deferred_runtime: Option<DeferredRuntime>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeferredRuntime {
    pub(crate) record_sha256: String,
    pub(crate) worker_identity_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StartOutcome {
    Completed,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

impl DirectoryIdentity {
    fn read(path: &Path, uid: u32) -> Result<Self, MachineError> {
        verify_secure_directory(path, uid)?;
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| MachineError::runtime_path_invalid(path, error.to_string()))?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl RunDeletion {
    pub(crate) fn capture(
        state_root: &Path,
        workspace_id: &str,
        run_id: Uuid,
        lifecycle: RunLifecycle,
        controller: ControllerBinding,
        start_receipt: Option<OperationReceipt>,
    ) -> Result<Self, MachineError> {
        let deletion = Self {
            schema_version: 1,
            workspace_id: workspace_id.to_owned(),
            run_id,
            lifecycle,
            controller,
            start_receipt,
            source: DirectoryIdentity::read(&run_root(state_root, run_id), current_uid())?,
            force: None,
        };
        deletion.validate(run_id)?;
        Ok(deletion)
    }

    pub(crate) fn capture_force(
        state_root: &Path,
        manifest: &crate::run::RunManifest,
        lifecycle: RunLifecycle,
        force: ForceRetirement,
    ) -> Result<Self, MachineError> {
        let run_id = manifest.run_id;
        let deletion = Self {
            schema_version: 2,
            workspace_id: manifest.workspace_id.clone(),
            run_id,
            lifecycle,
            controller: manifest.controller.clone(),
            start_receipt: manifest.start_operation_receipt.clone().flatten(),
            source: DirectoryIdentity::read(&run_root(state_root, run_id), current_uid())?,
            force: Some(force),
        };
        deletion.validate(run_id)?;
        Ok(deletion)
    }

    pub(crate) fn start_outcome(&self) -> &'static str {
        match self.force.as_ref().map(|force| force.start_outcome) {
            Some(StartOutcome::Completed) => "completed",
            Some(StartOutcome::Failed) => "failed",
            Some(StartOutcome::Unknown) => "unknown",
            None if self.lifecycle == RunLifecycle::Closed => "completed",
            None => "failed",
        }
    }

    fn validate(&self, run_id: Uuid) -> Result<(), MachineError> {
        let valid_mode = match (self.schema_version, &self.force) {
            (1, None) => matches!(
                self.lifecycle,
                RunLifecycle::Closed | RunLifecycle::StartFailed
            ),
            (2, Some(force)) => {
                force.operator_id.get_version_num() == 7
                    && force.operator_generation > 0
                    && !force.profile.is_empty()
                    && is_digest(&force.server_key)
                    && force
                        .writer_generation
                        .is_none_or(|generation| generation > 0 && force.worker_generation > 0)
                    && (self.start_receipt.is_some()
                        || force.start_outcome == StartOutcome::Unknown)
                    && force.deferred_runtime.as_ref().is_none_or(|deferred| {
                        force.worker_generation > 0
                            && is_digest(&deferred.record_sha256)
                            && is_digest(&deferred.worker_identity_sha256)
                    })
            }
            _ => false,
        };
        if !valid_mode
            || self.run_id != run_id
            || run_id.get_version_num() != 7
            || !is_digest(&self.workspace_id)
            || self.source.device == 0
            || self.source.inode == 0
            || self.controller.identity.generation == 0
            || !is_digest(&self.controller.capability_sha256)
        {
            return Err(refusal(run_id, "invalid_deletion_tombstone"));
        }
        if let Some(receipt) = &self.start_receipt {
            receipt
                .validate()
                .map_err(|_| refusal(run_id, "invalid_deletion_tombstone"))?;
            if receipt.namespace.workspace_ref != self.workspace_id
                || receipt.namespace.operation_kind != "start_run"
                || receipt.accepted_subjects.run_ref.as_deref() != Some(&run_id.to_string())
            {
                return Err(refusal(run_id, "invalid_deletion_tombstone"));
            }
        }
        Ok(())
    }

    /// The caller holds the lifecycle and startup serializers and has proved
    /// the required process identities and any deferred cleanup. The prepared directory contains only
    /// the marker until exchange, and only the detached payload afterwards.
    pub(crate) fn publish(&self, state_root: &Path) -> Result<(), MachineError> {
        let uid = current_uid();
        let root = run_root(state_root, self.run_id);
        let parent = root.parent().expect("Run path has a parent");
        let state_identity = DirectoryIdentity::read(state_root, uid)?;
        let runs_identity = DirectoryIdentity::read(parent, uid)?;
        let staging_parent = state_root.join("run-deletions");
        ensure_directory(&staging_parent)?;
        let staging_identity = DirectoryIdentity::read(&staging_parent, uid)?;
        let detached = self.detached_path(state_root);
        ensure_directory(&detached)?;
        if DirectoryIdentity::read(&root, uid)? != self.source {
            return Err(refusal(self.run_id, "deletion_directory_identity_changed"));
        }
        self.prepare(&detached, uid)?;
        if DirectoryIdentity::read(state_root, uid)? != state_identity
            || DirectoryIdentity::read(parent, uid)? != runs_identity
            || DirectoryIdentity::read(&staging_parent, uid)? != staging_identity
            || DirectoryIdentity::read(&root, uid)? != self.source
            || read_at(&detached, self.run_id, uid)?.as_ref() != Some(self)
        {
            return Err(refusal(self.run_id, "deletion_directory_identity_changed"));
        }
        DarwinSystem
            .exchange_paths(&root, &detached)
            .map_err(|_| self.exchange_error())
    }

    fn prepare(&self, directory: &Path, uid: u32) -> Result<(), MachineError> {
        let marker = directory.join(MARKER);
        match read_at(directory, self.run_id, uid)? {
            Some(mut prepared) => {
                if prepared == *self {
                    return Ok(());
                }
                // Preparation has no effect until exchange. A fresh confirmed
                // request can rebind the same proved payload after Controller reset.
                prepared.controller = self.controller.clone();
                let same_force_scope = self.force.as_ref().is_some_and(|force| {
                    prepared
                        .force
                        .as_ref()
                        .is_none_or(|previous| previous.operator_id == force.operator_id)
                        && prepared.source == self.source
                        && prepared.workspace_id == self.workspace_id
                        && prepared.start_receipt == self.start_receipt
                });
                if prepared != *self && !same_force_scope {
                    return Err(refusal(self.run_id, "prepared_deletion_changed"));
                }
                fs::remove_file(&marker).map_err(|_| self.exchange_error())?;
            }
            None if fs::read_dir(directory)
                .map_err(|_| self.exchange_error())?
                .next()
                .is_none() => {}
            None => return Err(refusal(self.run_id, "prepared_deletion_changed")),
        }
        let bytes = serde_json::to_vec(self).map_err(|_| self.exchange_error())?;
        atomic_create(&SystemWorkspacePlatform, &marker, &bytes, 0o600)
            .map_err(|_| self.exchange_error())
    }

    pub(crate) fn sync_published(&self, state_root: &Path) -> Result<(), MachineError> {
        let uid = current_uid();
        let root = run_root(state_root, self.run_id);
        let parent = root.parent().expect("Run path has a parent");
        let detached = self.detached_path(state_root);
        let staging_parent = detached.parent().expect("detached path has a parent");
        verify_secure_directory(staging_parent, uid)?;
        if read_at(&root, self.run_id, uid)?.as_ref() != Some(self) {
            return Err(refusal(self.run_id, "deletion_tombstone_changed"));
        }
        // Persist the exchange before allowing any destructive cleanup.
        sync_directory(parent).map_err(|_| self.exchange_error())?;
        sync_directory(staging_parent).map_err(|_| self.exchange_error())?;
        Ok(())
    }

    pub(crate) fn finish(&self, state_root: &Path) -> Result<(), MachineError> {
        self.sync_published(state_root)?;
        let uid = current_uid();
        let detached = self.detached_path(state_root);
        let staging_parent = detached.parent().expect("detached path has a parent");
        match fs::symlink_metadata(&detached) {
            Ok(_) if DirectoryIdentity::read(&detached, uid)? == self.source => {
                fs::remove_dir_all(&detached).map_err(|_| self.exchange_error())?;
            }
            Ok(_) => return Err(refusal(self.run_id, "detached_run_identity_changed")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(self.exchange_error()),
        }
        sync_directory(staging_parent).map_err(|_| self.exchange_error())
    }

    pub(crate) fn payload_pending(&self, state_root: &Path) -> Result<bool, MachineError> {
        let detached = self.detached_path(state_root);
        match fs::symlink_metadata(&detached) {
            Ok(_) if DirectoryIdentity::read(&detached, current_uid())? == self.source => Ok(true),
            Ok(_) => Err(refusal(self.run_id, "detached_run_identity_changed")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(self.exchange_error()),
        }
    }

    fn detached_path(&self, state_root: &Path) -> PathBuf {
        state_root
            .join("run-deletions")
            .join(self.run_id.to_string())
    }

    fn exchange_error(&self) -> MachineError {
        refusal(self.run_id, "run_deletion_pending")
    }
}

pub(crate) fn read_at(
    root: &Path,
    run_id: Uuid,
    uid: u32,
) -> Result<Option<RunDeletion>, MachineError> {
    let path = root.join(MARKER);
    let Some(file) = open_secure_file_if_present(&path, uid)? else {
        return Ok(None);
    };
    verify_secure_directory(root, uid)?;
    let metadata = file
        .metadata()
        .map_err(|_| refusal(run_id, "invalid_deletion_tombstone"))?;
    if metadata.nlink() != 1 || metadata.len() > MAX_MARKER_BYTES {
        return Err(refusal(run_id, "invalid_deletion_tombstone"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refusal(run_id, "invalid_deletion_tombstone"))?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err(refusal(run_id, "invalid_deletion_tombstone"));
    }
    let deletion: RunDeletion = serde_json::from_slice(&bytes)
        .map_err(|_| refusal(run_id, "invalid_deletion_tombstone"))?;
    deletion.validate(run_id)?;
    if DirectoryIdentity::read(root, uid)? == deletion.source
        || fs::read_dir(root)
            .map_err(|_| refusal(run_id, "invalid_deletion_tombstone"))?
            .take(2)
            .count()
            != 1
    {
        return Err(refusal(run_id, "invalid_deletion_tombstone"));
    }
    Ok(Some(deletion))
}

pub(crate) fn require_live_at(root: &Path, run_id: Uuid) -> Result<(), MachineError> {
    if read_at(root, run_id, current_uid())?.is_some() {
        return Err(crate::run::run_not_found(run_id));
    }
    Ok(())
}

pub(crate) fn replay_refusal(run_id: Uuid) -> MachineError {
    MachineError::new(
        "RUN_NOT_FOUND",
        "the original allocated Run was deleted",
        false,
        json!({"run_id":run_id,"reason":"run_deleted"}),
    )
}

fn ensure_directory(path: &Path) -> Result<(), MachineError> {
    match create_directory(path, 0o700) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_secure_directory(path, current_uid())
        }
        Err(error) => Err(MachineError::runtime_path_invalid(path, error.to_string())),
    }
}

fn current_uid() -> u32 {
    SystemWorkspacePlatform.current_uid()
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn refusal(run_id: Uuid, reason: &'static str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "confirmed Run removal could not verify its retained state",
        false,
        json!({"run_id":run_id,"reason":reason}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ControllerIdentity, ControllerKind};

    struct TestDirectory(PathBuf);

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exchanged_deletion_resumes_only_the_original_detached_directory() {
        let root =
            TestDirectory(Path::new("/tmp").join(format!("dolgorae-deletion-{}", Uuid::now_v7())));
        create_directory(&root.0, 0o700).unwrap();
        let state = &root.0;
        let run_id = Uuid::now_v7();
        let run = run_root(state, run_id);
        create_directory(&state.join("runs"), 0o700).unwrap();
        create_directory(&run, 0o700).unwrap();
        fs::write(run.join("original"), b"Run payload").unwrap();
        let controller = ControllerBinding {
            identity: ControllerIdentity {
                controller_id: Uuid::now_v7(),
                kind: ControllerKind::Automation,
                instance_id: "deletion-test".into(),
                subject_id: None,
                generation: 1,
            },
            capability_sha256: "a".repeat(64),
        };
        let mut deletion = RunDeletion::capture(
            state,
            &"b".repeat(64),
            run_id,
            RunLifecycle::StartFailed,
            controller,
            None,
        )
        .unwrap();
        let prepared = deletion.detached_path(state);
        ensure_directory(prepared.parent().unwrap()).unwrap();
        ensure_directory(&prepared).unwrap();
        deletion.prepare(&prepared, current_uid()).unwrap();
        // A reset before exchange does not strand the inert prepared entry.
        deletion.controller.identity.generation += 1;
        deletion.controller.capability_sha256 = "c".repeat(64);
        deletion.publish(state).unwrap();

        // Start a new invocation after the exchange and before payload removal.
        let retained = read_at(&run, run_id, current_uid()).unwrap().unwrap();
        assert_eq!(retained, deletion);
        assert_eq!(
            require_live_at(&run, run_id).unwrap_err().code,
            "RUN_NOT_FOUND"
        );
        let detached = retained.detached_path(state);
        assert_eq!(fs::read(detached.join("original")).unwrap(), b"Run payload");
        let original = detached.with_extension("original");
        fs::rename(&detached, &original).unwrap();
        create_directory(&detached, 0o700).unwrap();
        fs::write(detached.join("unrelated"), b"must survive").unwrap();
        assert_eq!(
            retained.finish(state).unwrap_err().details["reason"],
            "detached_run_identity_changed"
        );
        assert_eq!(
            fs::read(detached.join("unrelated")).unwrap(),
            b"must survive"
        );
        fs::remove_file(detached.join("unrelated")).unwrap();
        fs::remove_dir(&detached).unwrap();
        fs::rename(original, &detached).unwrap();
        retained.finish(state).unwrap();
        retained.finish(state).unwrap();
        assert!(!detached.exists());
        assert_eq!(
            read_at(&run, run_id, current_uid()).unwrap(),
            Some(retained)
        );
    }
}
