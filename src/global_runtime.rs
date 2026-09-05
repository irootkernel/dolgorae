//! Global Profile runtime, membership, and immutable Run binding.

use crate::darwin::DarwinSystem;
use crate::global_profile::GlobalProfileStore;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::profile::{
    CompatibilityVerdict, ExecutableIdentity, ProfileCapabilityState, ProfileSnapshot, ServerState,
};
use crate::workspace::{
    RuntimeProfile, SystemWorkspacePlatform, atomic_create, create_directory, sync_directory,
    verify_secure_directory, verify_secure_file,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_MEMBERSHIP_BYTES: u64 = 8 * 1024 * 1024;

/// One name resolution from the global registry. The selected name is kept
/// separately because two names may intentionally describe one launch
/// contract and therefore share a server key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedGlobalProfile {
    pub schema_version: u32,
    pub selected_name: String,
    pub definition: RuntimeProfile,
    pub definition_sha256: String,
}

/// The complete immutable Profile authority stored with a Run generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalProfileBinding {
    pub schema_version: u32,
    pub selected_name: String,
    pub definition: RuntimeProfile,
    pub definition_sha256: String,
    pub launch_snapshot: ProfileSnapshot,
    pub launch_snapshot_sha256: String,
    pub server_key: String,
}

/// Name-neutral launch authority owned by one physical Profile Server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalLaunchSnapshot {
    pub schema_version: u32,
    pub canonical_codex_home: String,
    pub normalized_argv: Vec<String>,
    pub launch_cwd_policy: String,
    pub derived_launch_cwd: String,
    pub sanitized_environment: BTreeMap<String, String>,
    pub enabled_features: Vec<String>,
    pub disabled_features: Vec<String>,
    pub process_static_configuration: BTreeMap<String, Value>,
    pub initial_configuration_observation: BTreeMap<String, Value>,
    pub executable_identity: ExecutableIdentity,
    pub codex_version: String,
    pub schema_bundle_sha256: String,
    pub compatibility_manifest_sha256: String,
    pub launch_contract_sha256: String,
    pub compatibility_verdict: CompatibilityVerdict,
    pub server_key: String,
}

impl From<&ProfileSnapshot> for GlobalLaunchSnapshot {
    fn from(snapshot: &ProfileSnapshot) -> Self {
        Self {
            schema_version: snapshot.schema_version,
            canonical_codex_home: snapshot.canonical_codex_home.clone(),
            normalized_argv: snapshot.normalized_argv.clone(),
            launch_cwd_policy: snapshot.launch_cwd_policy.clone(),
            derived_launch_cwd: snapshot.derived_launch_cwd.clone(),
            sanitized_environment: snapshot.sanitized_environment.clone(),
            enabled_features: snapshot.enabled_features.clone(),
            disabled_features: snapshot.disabled_features.clone(),
            process_static_configuration: snapshot.process_static_configuration.clone(),
            initial_configuration_observation: snapshot.initial_configuration_observation.clone(),
            executable_identity: snapshot.executable_identity.clone(),
            codex_version: snapshot.codex_version.clone(),
            schema_bundle_sha256: snapshot.schema_bundle_sha256.clone(),
            compatibility_manifest_sha256: snapshot.compatibility_manifest_sha256.clone(),
            launch_contract_sha256: snapshot.launch_contract_sha256.clone(),
            compatibility_verdict: snapshot.compatibility_verdict,
            server_key: snapshot.server_key.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalProfileServerState {
    pub schema_version: u32,
    pub server_key: String,
    pub lifecycle: String,
    pub server_epoch: u64,
    pub epoch_id: Uuid,
    pub boot_session_uuid: Uuid,
    pub pid: u32,
    pub pgid: u32,
    pub uid: u32,
    pub process_fingerprint: String,
    pub drainer_pid: u32,
    pub drainer_pgid: u32,
    pub drainer_uid: u32,
    pub drainer_fingerprint: String,
    pub socket_path: String,
    pub socket_device: u64,
    pub socket_inode: u64,
    pub membership_revision: u64,
    pub default_model: String,
    pub models: Vec<String>,
    pub capabilities: BTreeMap<String, ProfileCapabilityState>,
    pub launch_snapshot_sha256: String,
    pub snapshot: GlobalLaunchSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalRuntimeDiscovery {
    pub schema_version: u32,
    pub selected_profile: String,
    pub server_key: String,
    pub server_epoch: u64,
    pub socket_path: String,
    pub binding_sha256: String,
}

impl GlobalProfileServerState {
    pub fn from_prepared(state: &ServerState) -> Result<Self, MachineError> {
        let snapshot = GlobalLaunchSnapshot::from(&state.snapshot);
        Ok(Self {
            schema_version: 2,
            server_key: state.server_key.clone(),
            lifecycle: state.lifecycle.clone(),
            server_epoch: state.server_epoch,
            epoch_id: state.epoch_id,
            boot_session_uuid: state.boot_session_uuid,
            pid: state.pid,
            pgid: state.pgid,
            uid: state.uid,
            process_fingerprint: state.process_fingerprint.clone(),
            drainer_pid: state.drainer_pid,
            drainer_pgid: state.drainer_pgid,
            drainer_uid: state.drainer_uid,
            drainer_fingerprint: state.drainer_fingerprint.clone(),
            socket_path: state.socket_path.clone(),
            socket_device: state.socket_device,
            socket_inode: state.socket_inode,
            membership_revision: state.membership_revision,
            default_model: state.default_model.clone(),
            models: state.models.clone(),
            capabilities: state.capabilities.clone(),
            launch_snapshot_sha256: canonical_digest(&snapshot)?,
            snapshot,
        })
    }

    pub fn validate_for_restart(&self) -> Result<(), MachineError> {
        if self.schema_version != 2
            || self.server_epoch == 0
            || self.server_key != self.snapshot.server_key
            || self.launch_snapshot_sha256 != canonical_digest(&self.snapshot)?
        {
            return Err(binding_invalid(
                "global Profile Server state cannot reconstruct its launch generation",
            ));
        }
        validate_server_key(&self.server_key)
    }

    pub fn discover(
        &self,
        binding: &GlobalProfileBinding,
    ) -> Result<GlobalRuntimeDiscovery, MachineError> {
        self.validate_for_restart()?;
        binding.validate_for_recovery()?;
        if binding.server_key != self.server_key
            || GlobalLaunchSnapshot::from(&binding.launch_snapshot) != self.snapshot
        {
            return Err(binding_invalid(
                "Run binding does not name this Profile Server generation",
            ));
        }
        Ok(GlobalRuntimeDiscovery {
            schema_version: 2,
            selected_profile: binding.selected_name.clone(),
            server_key: self.server_key.clone(),
            server_epoch: self.server_epoch,
            socket_path: self.socket_path.clone(),
            binding_sha256: canonical_digest(binding)?,
        })
    }

    pub fn diagnostic(&self, binding: &GlobalProfileBinding) -> Result<Value, MachineError> {
        let discovery = self.discover(binding)?;
        Ok(json!({
            "schema_version": 2,
            "profile": discovery.selected_profile,
            "server_key": discovery.server_key,
            "server_epoch": discovery.server_epoch,
            "lifecycle": self.lifecycle,
            "membership_revision": self.membership_revision,
            "launch_snapshot_sha256": self.launch_snapshot_sha256,
        }))
    }
}

impl ResolvedGlobalProfile {
    pub fn from_definition(
        selected_name: &str,
        definition: RuntimeProfile,
    ) -> Result<Self, MachineError> {
        if selected_name.is_empty() {
            return Err(MachineError::invalid_argument(
                "profile_name",
                "a global Profile name is required",
            ));
        }
        let definition_sha256 = canonical_digest(&definition)?;
        Ok(Self {
            schema_version: 1,
            selected_name: selected_name.to_owned(),
            definition,
            definition_sha256,
        })
    }

    pub fn resolve(home: &DolgoraeHome, selected_name: &str) -> Result<Self, MachineError> {
        let definition = GlobalProfileStore::new(home).resolve(selected_name)?;
        Self::from_definition(selected_name, definition)
    }

    /// Perform the expensive executable and App Server inspection from the
    /// already resolved definition, without consulting caller environment or
    /// reopening the registry.
    pub fn prepare(self, home: &DolgoraeHome) -> Result<GlobalProfileBinding, MachineError> {
        let snapshot =
            crate::profile::snapshot_for_global(home, &self.selected_name, &self.definition)?;
        self.bind(snapshot)
    }

    pub fn bind(
        self,
        launch_snapshot: ProfileSnapshot,
    ) -> Result<GlobalProfileBinding, MachineError> {
        let mut expected_arguments = self
            .definition
            .argv
            .iter()
            .skip(1)
            .cloned()
            .collect::<Vec<_>>();
        if !expected_arguments
            .iter()
            .any(|argument| argument == "--strict-config")
        {
            expected_arguments.push("--strict-config".to_owned());
        }
        expected_arguments.extend(["--enable".to_owned(), "multi_agent".to_owned()]);
        if self.schema_version != 1
            || self.definition_sha256 != canonical_digest(&self.definition)?
            || launch_snapshot.schema_version != 1
            || launch_snapshot.profile_name != self.selected_name
            || launch_snapshot.normalized_argv.get(1..) != Some(expected_arguments.as_slice())
            || self
                .definition
                .environment
                .iter()
                .any(|(name, value)| launch_snapshot.sanitized_environment.get(name) != Some(value))
            || !launch_snapshot
                .enabled_features
                .iter()
                .any(|feature| feature == "multi_agent")
            || launch_snapshot.server_key.len() != 64
            || !launch_snapshot
                .server_key
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(binding_invalid(
                "resolved definition and launch snapshot disagree",
            ));
        }
        let launch_snapshot_sha256 = canonical_digest(&launch_snapshot)?;
        Ok(GlobalProfileBinding {
            schema_version: 2,
            selected_name: self.selected_name,
            definition: self.definition,
            definition_sha256: self.definition_sha256,
            server_key: launch_snapshot.server_key.clone(),
            launch_snapshot,
            launch_snapshot_sha256,
        })
    }
}

impl GlobalProfileBinding {
    pub fn digest(&self) -> Result<String, MachineError> {
        canonical_digest(self)
    }

    /// Validate only persisted bytes. Recovery deliberately has no registry or
    /// caller-environment parameter and therefore cannot drift after admission.
    pub fn validate_for_recovery(&self) -> Result<(), MachineError> {
        if self.schema_version != 2
            || self.selected_name.is_empty()
            || self.definition_sha256 != canonical_digest(&self.definition)?
            || self.launch_snapshot_sha256 != canonical_digest(&self.launch_snapshot)?
            || self.launch_snapshot.profile_name != self.selected_name
            || self.launch_snapshot.server_key != self.server_key
        {
            return Err(binding_invalid(
                "persisted global Profile binding is inconsistent",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipDisposition {
    Active,
    Released,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalMembershipRecord {
    pub schema_version: u32,
    pub revision: u64,
    pub workspace_id: String,
    pub run_id: Uuid,
    pub disposition: MembershipDisposition,
    pub controller_id: Option<Uuid>,
    pub worker_generation: Option<u64>,
    pub thread_id: Option<String>,
    pub connection_id: Option<Uuid>,
    pub lifecycle: String,
    pub writer: bool,
    pub observed_epoch: Option<u64>,
    pub runtime_locator: Option<String>,
    pub previous_sha256: String,
    pub record_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalMembershipFacts {
    pub controller_id: Option<Uuid>,
    pub worker_generation: Option<u64>,
    pub thread_id: Option<String>,
    pub connection_id: Option<Uuid>,
    pub lifecycle: String,
    pub writer: bool,
    pub observed_epoch: Option<u64>,
    pub runtime_locator: Option<String>,
}

impl GlobalMembershipFacts {
    fn for_disposition(disposition: MembershipDisposition) -> Self {
        Self {
            controller_id: None,
            worker_generation: None,
            thread_id: None,
            connection_id: None,
            lifecycle: match disposition {
                MembershipDisposition::Active => "running",
                MembershipDisposition::Released => "closed",
                MembershipDisposition::Unknown => "outcome_unknown",
            }
            .to_owned(),
            writer: false,
            observed_epoch: None,
            runtime_locator: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalMembershipIndex {
    pub schema_version: u32,
    pub server_key: String,
    pub revision: u64,
    pub journal_sha256: String,
    pub members: BTreeMap<String, GlobalMembershipRecord>,
}

pub struct GlobalMembershipStore {
    home_root: PathBuf,
    server_key: String,
}

/// Holds the server membership lock across a destructive lifecycle commit.
/// Dropping the guard reopens admission.
pub struct GlobalQuiescenceGuard {
    _lock: File,
    pub index: GlobalMembershipIndex,
}

pub struct GlobalServerStateStore {
    membership: GlobalMembershipStore,
}

impl GlobalServerStateStore {
    pub fn new(home: &DolgoraeHome, server_key: &str) -> Result<Self, MachineError> {
        Self::from_root(home.root(), server_key)
    }

    pub(crate) fn from_root(root: &Path, server_key: &str) -> Result<Self, MachineError> {
        Ok(Self {
            membership: GlobalMembershipStore::from_root(root, server_key)?,
        })
    }

    pub fn publish(&self, state: &GlobalProfileServerState) -> Result<PathBuf, MachineError> {
        state.validate_for_restart()?;
        if state.server_key != self.membership.server_key {
            return Err(binding_invalid(
                "server state store key disagrees with state",
            ));
        }
        let root = self.membership.ensure_root()?;
        let _lock = lock_file(&root.join("server.lock"))?;
        let path = root.join("state.json");
        let bytes = serde_json::to_vec_pretty(state).map_err(internal)?;
        let temporary = root.join(format!(".state-{}.json", Uuid::now_v7()));
        atomic_create(&SystemWorkspacePlatform, &temporary, &bytes, 0o600)
            .map_err(|error| path_error(&temporary, error))?;
        fs::rename(&temporary, &path).map_err(|error| path_error(&path, error))?;
        sync_directory(&root).map_err(|error| path_error(&root, error))?;
        Ok(path)
    }

    pub fn load_for_recovery(&self) -> Result<GlobalProfileServerState, MachineError> {
        let root = self.membership.ensure_root()?;
        let _lock = lock_file(&root.join("server.lock"))?;
        let path = root.join("state.json");
        verify_secure_file(&path, DarwinSystem.current_uid())?;
        let state: GlobalProfileServerState =
            serde_json::from_slice(&fs::read(&path).map_err(|error| path_error(&path, error))?)
                .map_err(|_| binding_invalid("global Profile Server state is malformed"))?;
        state.validate_for_restart()?;
        if state.server_key != self.membership.server_key {
            return Err(binding_invalid(
                "persisted server state is stored under the wrong key",
            ));
        }
        Ok(state)
    }
}

/// Remove one selected alias only after global membership under its resolved
/// server key is proven quiescent while the registry lock is still held.
pub fn remove_global_profile(home: &DolgoraeHome, selected_name: &str) -> Result<(), MachineError> {
    GlobalProfileStore::new(home)
        .remove_if(selected_name, |definition| {
            let binding =
                ResolvedGlobalProfile::from_definition(selected_name, definition.clone())?
                    .prepare(home)?;
            let _guard = GlobalMembershipStore::new(home, &binding.server_key)?
                .acquire_quiescence_under_registry_lock("remove")?;
            Ok(())
        })
        .map(|_| ())
}

impl GlobalMembershipStore {
    pub fn new(home: &DolgoraeHome, server_key: &str) -> Result<Self, MachineError> {
        Self::from_root(home.root(), server_key)
    }

    pub(crate) fn from_root(root: &Path, server_key: &str) -> Result<Self, MachineError> {
        validate_server_key(server_key)?;
        Ok(Self {
            home_root: root.to_path_buf(),
            server_key: server_key.to_owned(),
        })
    }

    pub fn record(
        &self,
        workspace_id: &str,
        run_id: Uuid,
        disposition: MembershipDisposition,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        self.record_observed(
            workspace_id,
            run_id,
            disposition,
            GlobalMembershipFacts::for_disposition(disposition),
        )
    }

    pub fn record_observed(
        &self,
        workspace_id: &str,
        run_id: Uuid,
        disposition: MembershipDisposition,
        facts: GlobalMembershipFacts,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        validate_member_identity(workspace_id)?;
        validate_membership_facts(&facts)?;
        let root = self.ensure_root()?;
        self.record_locked_root(&root, workspace_id, run_id, disposition, facts)
    }

    pub(crate) fn record_under_lifecycle_locks(
        &self,
        root: &Path,
        workspace_id: &str,
        run_id: Uuid,
        disposition: MembershipDisposition,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        validate_member_identity(workspace_id)?;
        self.record_under_server_lock(
            root,
            workspace_id,
            run_id,
            disposition,
            GlobalMembershipFacts::for_disposition(disposition),
        )
    }

    fn record_locked_root(
        &self,
        root: &Path,
        workspace_id: &str,
        run_id: Uuid,
        disposition: MembershipDisposition,
        facts: GlobalMembershipFacts,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        let _lock = lock_file(&root.join("server.lock"))?;
        self.record_under_server_lock(root, workspace_id, run_id, disposition, facts)
    }

    fn record_under_server_lock(
        &self,
        root: &Path,
        workspace_id: &str,
        run_id: Uuid,
        disposition: MembershipDisposition,
        facts: GlobalMembershipFacts,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        let journal = root.join("membership.jsonl");
        let mut records = replay(&journal, &self.server_key)?;
        let revision = u64::try_from(records.len())
            .map_err(|_| membership_incomplete(&self.server_key, "membership revision overflow"))?
            + 1;
        let previous_sha256 = records.last().map_or_else(
            || ZERO_HASH.to_owned(),
            |record| record.record_sha256.clone(),
        );
        let body = json!({
            "schema_version": 2,
            "revision": revision,
            "workspace_id": workspace_id,
            "run_id": run_id,
            "disposition": disposition,
            "controller_id": facts.controller_id,
            "worker_generation": facts.worker_generation,
            "thread_id": facts.thread_id,
            "connection_id": facts.connection_id,
            "lifecycle": facts.lifecycle,
            "writer": facts.writer,
            "observed_epoch": facts.observed_epoch,
            "runtime_locator": facts.runtime_locator,
            "previous_sha256": previous_sha256,
        });
        let record = GlobalMembershipRecord {
            schema_version: 2,
            revision,
            workspace_id: workspace_id.to_owned(),
            run_id,
            disposition,
            controller_id: facts.controller_id,
            worker_generation: facts.worker_generation,
            thread_id: facts.thread_id,
            connection_id: facts.connection_id,
            lifecycle: facts.lifecycle,
            writer: facts.writer,
            observed_epoch: facts.observed_epoch,
            runtime_locator: facts.runtime_locator,
            previous_sha256: body["previous_sha256"].as_str().expect("string").to_owned(),
            record_sha256: canonical_value_digest(&body)?,
        };
        append_record(&journal, &record)?;
        records.push(record);
        let index = derive_index(&self.server_key, &journal, &records)?;
        store_index(&root.join("members.json"), &index)?;
        Ok(index)
    }

    pub fn load(&self) -> Result<GlobalMembershipIndex, MachineError> {
        let root = self.ensure_root()?;
        let _lock = lock_file(&root.join("server.lock"))?;
        self.load_locked(&root)
    }

    /// Verify quiescence when the caller already holds this server key's
    /// `server.lock` as part of the lifecycle lock hierarchy.
    pub fn require_quiescent_under_server_lock(
        &self,
        profile: &str,
        _operation: &str,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        let root = self.home_root.join("profiles").join(&self.server_key);
        verify_secure_directory(&root, DarwinSystem.current_uid())?;
        let journal = root.join("membership.jsonl");
        let records = replay(&journal, &self.server_key)?;
        let index = derive_index(&self.server_key, &journal, &records)?;
        self.verify_persisted_index(&root, &index)?;
        let blockers = index
            .members
            .values()
            .filter(|member| member.disposition != MembershipDisposition::Released)
            .collect::<Vec<_>>();
        if !blockers.is_empty() {
            return Err(MachineError::new(
                "PROFILE_MEMBERSHIP_INCOMPLETE",
                "global Profile membership blocks the lifecycle operation",
                false,
                json!({
                    "server_key": self.server_key,
                    "profile": profile,
                    "reason": format!("stopping this server would interrupt {} live run member(s)", blockers.len()),
                }),
            ));
        }
        Ok(index)
    }

    fn load_locked(&self, root: &Path) -> Result<GlobalMembershipIndex, MachineError> {
        let journal = root.join("membership.jsonl");
        let records = replay(&journal, &self.server_key)?;
        let index = derive_index(&self.server_key, &journal, &records)?;
        self.verify_persisted_index(root, &index)?;
        Ok(index)
    }

    fn verify_persisted_index(
        &self,
        root: &Path,
        index: &GlobalMembershipIndex,
    ) -> Result<(), MachineError> {
        let persisted = root.join("members.json");
        if persisted.exists() {
            verify_secure_file(&persisted, DarwinSystem.current_uid())?;
            let actual: GlobalMembershipIndex = serde_json::from_slice(
                &fs::read(&persisted).map_err(|error| path_error(&persisted, error))?,
            )
            .map_err(|_| membership_incomplete(&self.server_key, "membership index is invalid"))?;
            if actual != *index {
                return Err(membership_incomplete(
                    &self.server_key,
                    "membership index disagrees with journal",
                ));
            }
        }
        Ok(())
    }

    pub fn require_quiescent(&self, operation: &str) -> Result<(), MachineError> {
        self.acquire_quiescence(operation).map(|_| ())
    }

    pub fn acquire_quiescence(
        &self,
        operation: &str,
    ) -> Result<GlobalQuiescenceGuard, MachineError> {
        let root = self.ensure_root()?;
        self.acquire_quiescence_locked_root(&root, operation)
    }

    fn acquire_quiescence_under_registry_lock(
        &self,
        operation: &str,
    ) -> Result<GlobalQuiescenceGuard, MachineError> {
        let root = self.ensure_root_under_home_lock()?;
        self.acquire_quiescence_locked_root(&root, operation)
    }

    fn acquire_quiescence_locked_root(
        &self,
        root: &Path,
        operation: &str,
    ) -> Result<GlobalQuiescenceGuard, MachineError> {
        let lock = lock_file(&root.join("server.lock"))?;
        let index = self.load_locked(root)?;
        let blockers: Vec<_> = index
            .members
            .values()
            .filter(|member| member.disposition != MembershipDisposition::Released)
            .map(|member| {
                json!({
                    "workspace_id": member.workspace_id,
                    "run_id": member.run_id,
                    "disposition": member.disposition,
                })
            })
            .collect();
        if blockers.is_empty() {
            Ok(GlobalQuiescenceGuard { _lock: lock, index })
        } else {
            Err(MachineError::new(
                "PROFILE_SERVER_BUSY",
                "global Profile membership blocks the lifecycle operation",
                false,
                json!({"server_key": self.server_key, "operation": operation, "blockers": blockers}),
            ))
        }
    }

    fn ensure_root(&self) -> Result<PathBuf, MachineError> {
        verify_secure_directory(&self.home_root, DarwinSystem.current_uid())?;
        let home_lock =
            File::open(&self.home_root).map_err(|error| path_error(&self.home_root, error))?;
        DarwinSystem
            .lock_exclusive(&home_lock)
            .map_err(|error| path_error(&self.home_root, error))?;
        self.ensure_root_under_home_lock()
    }

    fn ensure_root_under_home_lock(&self) -> Result<PathBuf, MachineError> {
        let profiles = self.home_root.join("profiles");
        if !profiles.exists() {
            create_directory(&profiles, 0o700).map_err(|error| path_error(&profiles, error))?;
            sync_directory(&self.home_root).map_err(|error| path_error(&self.home_root, error))?;
        }
        verify_secure_directory(&profiles, DarwinSystem.current_uid())?;
        let root = profiles.join(&self.server_key);
        if !root.exists() {
            create_directory(&root, 0o700).map_err(|error| path_error(&root, error))?;
            sync_directory(&profiles).map_err(|error| path_error(&profiles, error))?;
        }
        verify_secure_directory(&root, DarwinSystem.current_uid())?;
        Ok(root)
    }
}

fn derive_index(
    server_key: &str,
    journal: &Path,
    records: &[GlobalMembershipRecord],
) -> Result<GlobalMembershipIndex, MachineError> {
    let mut members = BTreeMap::new();
    for record in records {
        members.insert(
            format!("{}:{}", record.workspace_id, record.run_id),
            record.clone(),
        );
    }
    let bytes = if journal.exists() {
        fs::read(journal).map_err(|error| path_error(journal, error))?
    } else {
        Vec::new()
    };
    Ok(GlobalMembershipIndex {
        schema_version: 2,
        server_key: server_key.to_owned(),
        revision: records.last().map_or(0, |record| record.revision),
        journal_sha256: sha256_hex(&bytes),
        members,
    })
}

fn replay(path: &Path, server_key: &str) -> Result<Vec<GlobalMembershipRecord>, MachineError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    verify_secure_file(path, DarwinSystem.current_uid())?;
    let bytes = fs::read(path).map_err(|error| path_error(path, error))?;
    if bytes.len() as u64 > MAX_MEMBERSHIP_BYTES {
        return Err(membership_incomplete(
            server_key,
            "membership journal exceeds 8 MiB",
        ));
    }
    let mut records = Vec::new();
    let mut previous = ZERO_HASH.to_owned();
    for (offset, line) in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let record: GlobalMembershipRecord = serde_json::from_slice(line).map_err(|_| {
            membership_incomplete(
                server_key,
                format!("membership line {} is invalid", offset + 1),
            )
        })?;
        let body = json!({
            "schema_version": record.schema_version,
            "revision": record.revision,
            "workspace_id": record.workspace_id,
            "run_id": record.run_id,
            "disposition": record.disposition,
            "controller_id": record.controller_id,
            "worker_generation": record.worker_generation,
            "thread_id": record.thread_id,
            "connection_id": record.connection_id,
            "lifecycle": record.lifecycle,
            "writer": record.writer,
            "observed_epoch": record.observed_epoch,
            "runtime_locator": record.runtime_locator,
            "previous_sha256": record.previous_sha256,
        });
        if record.schema_version != 2
            || record.revision != u64::try_from(offset + 1).expect("bounded")
            || record.previous_sha256 != previous
            || record.record_sha256 != canonical_value_digest(&body)?
        {
            return Err(membership_incomplete(
                server_key,
                format!("membership hash chain breaks at line {}", offset + 1),
            ));
        }
        previous = record.record_sha256.clone();
        records.push(record);
    }
    Ok(records)
}

fn append_record(path: &Path, record: &GlobalMembershipRecord) -> Result<(), MachineError> {
    let mut line = serde_json::to_vec(record).map_err(internal)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| path_error(path, error))?;
    file.write_all(&line)
        .and_then(|()| file.sync_all())
        .map_err(|error| path_error(path, error))?;
    sync_directory(path.parent().expect("journal parent")).map_err(|error| path_error(path, error))
}

fn store_index(path: &Path, index: &GlobalMembershipIndex) -> Result<(), MachineError> {
    let bytes = serde_json::to_vec_pretty(index).map_err(internal)?;
    let temporary = path.with_file_name(format!(".members-{}.json", Uuid::now_v7()));
    atomic_create(&SystemWorkspacePlatform, &temporary, &bytes, 0o600)
        .map_err(|error| path_error(&temporary, error))?;
    fs::rename(&temporary, path).map_err(|error| path_error(path, error))?;
    sync_directory(path.parent().expect("index parent")).map_err(|error| path_error(path, error))
}

fn lock_file(path: &Path) -> Result<File, MachineError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| path_error(path, error))?;
    DarwinSystem
        .lock_exclusive(&file)
        .map_err(|error| path_error(path, error))?;
    Ok(file)
}

fn validate_member_identity(workspace_id: &str) -> Result<(), MachineError> {
    if workspace_id.len() != 64
        || !workspace_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(MachineError::invalid_argument(
            "workspace_id",
            "workspace membership requires a lowercase SHA-256 identity",
        ));
    }
    Ok(())
}

fn validate_server_key(server_key: &str) -> Result<(), MachineError> {
    if server_key.len() != 64
        || !server_key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(MachineError::invalid_argument(
            "server_key",
            "global membership requires a lowercase SHA-256 server key",
        ));
    }
    Ok(())
}

fn validate_membership_facts(facts: &GlobalMembershipFacts) -> Result<(), MachineError> {
    if facts.lifecycle.is_empty()
        || facts.lifecycle.len() > 64
        || facts.worker_generation == Some(0)
        || facts.observed_epoch == Some(0)
        || facts.thread_id.as_ref().is_some_and(String::is_empty)
        || facts
            .runtime_locator
            .as_ref()
            .is_some_and(|value| value.is_empty() || !Path::new(value).is_absolute())
    {
        return Err(MachineError::invalid_argument(
            "membership",
            "global membership facts are incomplete or invalid",
        ));
    }
    Ok(())
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<String, MachineError> {
    canonical_value_digest(&serde_json::to_value(value).map_err(internal)?)
}

fn canonical_value_digest(value: &Value) -> Result<String, MachineError> {
    let text = serde_json::to_string(value).map_err(internal)?;
    let parsed = parse(&text).map_err(|error| internal(error.to_string()))?;
    let bytes = canonicalize(&parsed).map_err(|error| internal(error.to_string()))?;
    Ok(sha256_hex(&bytes))
}

fn membership_incomplete(server_key: &str, reason: impl Into<String>) -> MachineError {
    MachineError::new(
        "PROFILE_MEMBERSHIP_INCOMPLETE",
        "global Profile membership cannot be proven",
        false,
        json!({"server_key": server_key, "reason": reason.into()}),
    )
}

fn binding_invalid(reason: &str) -> MachineError {
    MachineError::new(
        "RUN_MANIFEST_INVALID",
        "global Profile Run binding is invalid",
        false,
        json!({"reason": reason}),
    )
}

fn path_error(path: &Path, error: std::io::Error) -> MachineError {
    MachineError::runtime_path_invalid(path, error.to_string())
}

fn internal(error: impl ToString) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "global Profile contract processing failed",
        false,
        json!({"reason": error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_profile::initialize_generation;
    use crate::profile::{CompatibilityVerdict, ExecutableIdentity};
    use crate::workspace::NativeSubagents;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;
    use std::thread;

    fn home() -> (PathBuf, DolgoraeHome) {
        let parent =
            std::env::temp_dir().join(format!("dolgorae-global-runtime-{}", Uuid::now_v7()));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let home = DolgoraeHome::from_canonical_home(parent.clone()).unwrap();
        initialize_generation(&home).unwrap();
        (parent, home)
    }

    fn definition(parent: &Path) -> RuntimeProfile {
        let executable = parent.join("codex");
        fs::write(&executable, [0xcf, 0xfa, 0xed, 0xfe]).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let codex_home = parent.join("codex-home");
        fs::create_dir(&codex_home).unwrap();
        RuntimeProfile {
            argv: vec![executable.to_string_lossy().into_owned()],
            codex_home: codex_home.to_string_lossy().into_owned(),
            environment: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("LANG".to_owned(), "C".to_owned()),
                ("LC_ALL".to_owned(), "C".to_owned()),
            ]),
            native_subagents: NativeSubagents::Enabled,
        }
    }

    fn snapshot(name: &str, definition: &RuntimeProfile, key: &str) -> ProfileSnapshot {
        ProfileSnapshot {
            schema_version: 1,
            profile_name: name.to_owned(),
            canonical_codex_home: definition.codex_home.clone(),
            normalized_argv: definition
                .argv
                .iter()
                .cloned()
                .chain([
                    "--strict-config".to_owned(),
                    "--enable".to_owned(),
                    "multi_agent".to_owned(),
                ])
                .collect(),
            launch_cwd_policy: "profile_state_directory_v1".to_owned(),
            derived_launch_cwd: format!("/tmp/{key}"),
            sanitized_environment: definition.environment.clone(),
            enabled_features: vec!["multi_agent".to_owned()],
            disabled_features: Vec::new(),
            process_static_configuration: BTreeMap::new(),
            initial_configuration_observation: BTreeMap::new(),
            executable_identity: ExecutableIdentity {
                resolved_path: definition.argv[0].clone(),
                device: 1,
                inode: 1,
                sha256: "1".repeat(64),
            },
            codex_version: "0.149.0".to_owned(),
            schema_bundle_sha256: "2".repeat(64),
            compatibility_manifest_sha256: "3".repeat(64),
            launch_contract_sha256: "4".repeat(64),
            compatibility_verdict: CompatibilityVerdict::Tested,
            server_key: key.to_owned(),
        }
    }

    fn server_state(snapshot: ProfileSnapshot) -> ServerState {
        ServerState {
            schema_version: 1,
            server_key: snapshot.server_key.clone(),
            lifecycle: "ready".to_owned(),
            server_epoch: 7,
            epoch_id: Uuid::now_v7(),
            boot_session_uuid: Uuid::now_v7(),
            pid: 10,
            pgid: 10,
            uid: DarwinSystem.current_uid(),
            process_fingerprint: "process".to_owned(),
            drainer_pid: 11,
            drainer_pgid: 11,
            drainer_uid: DarwinSystem.current_uid(),
            drainer_fingerprint: "drainer".to_owned(),
            socket_path: "/tmp/dolgorae-test.sock".to_owned(),
            socket_device: 1,
            socket_inode: 2,
            membership_revision: 0,
            default_model: "gpt-5.6".to_owned(),
            models: vec!["gpt-5.6".to_owned()],
            capabilities: BTreeMap::new(),
            snapshot,
        }
    }

    #[test]
    fn aliases_keep_selected_name_but_share_launch_identity() {
        let (parent, home) = home();
        let profile = definition(&parent);
        let store = GlobalProfileStore::new(&home);
        store.add("codex".to_owned(), profile.clone()).unwrap();
        store.add("codex-hsy".to_owned(), profile.clone()).unwrap();
        let first = ResolvedGlobalProfile::resolve(&home, "codex").unwrap();
        let second = ResolvedGlobalProfile::resolve(&home, "codex-hsy").unwrap();
        assert_eq!(first.definition_sha256, second.definition_sha256);
        let key = "a".repeat(64);
        let first = first.bind(snapshot("codex", &profile, &key)).unwrap();
        let second = second.bind(snapshot("codex-hsy", &profile, &key)).unwrap();
        assert_eq!(first.server_key, second.server_key);
        assert_ne!(first.selected_name, second.selected_name);
        let state =
            GlobalProfileServerState::from_prepared(&server_state(first.launch_snapshot.clone()))
                .unwrap();
        let first_discovery = state.discover(&first).unwrap();
        let second_discovery = state.discover(&second).unwrap();
        assert_eq!(first_discovery.server_key, second_discovery.server_key);
        assert_ne!(
            first_discovery.selected_profile,
            second_discovery.selected_profile
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn recovery_uses_persisted_binding_after_registry_change() {
        let (parent, home) = home();
        let profile = definition(&parent);
        let store = GlobalProfileStore::new(&home);
        store.add("selected".to_owned(), profile.clone()).unwrap();
        let binding = ResolvedGlobalProfile::resolve(&home, "selected")
            .unwrap()
            .bind(snapshot("selected", &profile, &"b".repeat(64)))
            .unwrap();
        store.remove_if("selected", |_| Ok(())).unwrap();
        binding.validate_for_recovery().unwrap();
        let state =
            GlobalProfileServerState::from_prepared(&server_state(binding.launch_snapshot.clone()))
                .unwrap();
        let state_store = GlobalServerStateStore::new(&home, &binding.server_key).unwrap();
        state_store.publish(&state).unwrap();
        let recovered = state_store.load_for_recovery().unwrap();
        assert_eq!(recovered, state);
        assert_eq!(
            recovered.diagnostic(&binding).unwrap(),
            recovered.diagnostic(&binding).unwrap()
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn membership_spans_workspaces_profiles_and_blocks_unknown_outcomes() {
        let (parent, home) = home();
        let first = GlobalMembershipStore::new(&home, &"c".repeat(64)).unwrap();
        let second = GlobalMembershipStore::new(&home, &"d".repeat(64)).unwrap();
        let run_a = Uuid::now_v7();
        let run_b = Uuid::now_v7();
        first
            .record(&"1".repeat(64), run_a, MembershipDisposition::Active)
            .unwrap();
        first
            .record(&"2".repeat(64), run_b, MembershipDisposition::Unknown)
            .unwrap();
        second
            .record(
                &"1".repeat(64),
                Uuid::now_v7(),
                MembershipDisposition::Active,
            )
            .unwrap();
        for operation in [
            "replace",
            "remove",
            "migrate",
            "stop",
            "restart",
            "generation_change",
        ] {
            let error = first.require_quiescent(operation).unwrap_err();
            assert_eq!(error.code, "PROFILE_SERVER_BUSY");
            assert_eq!(error.details["blockers"].as_array().unwrap().len(), 2);
        }
        first
            .record(&"1".repeat(64), run_a, MembershipDisposition::Released)
            .unwrap();
        first
            .record(&"2".repeat(64), run_b, MembershipDisposition::Released)
            .unwrap();
        first.require_quiescent("generation_change").unwrap();
        assert!(second.require_quiescent("stop").is_err());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn concurrent_membership_updates_form_one_complete_chain() {
        let (parent, home) = home();
        let store = Arc::new(GlobalMembershipStore::new(&home, &"e".repeat(64)).unwrap());
        let workers: Vec<_> = (0..12)
            .map(|index| {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    store
                        .record(
                            &format!("{index:064x}"),
                            Uuid::now_v7(),
                            MembershipDisposition::Active,
                        )
                        .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let index = store.load().unwrap();
        assert_eq!(index.revision, 12);
        assert_eq!(index.members.len(), 12);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn corrupt_or_legacy_membership_never_becomes_quiescent() {
        let (parent, home) = home();
        let store = GlobalMembershipStore::new(&home, &"f".repeat(64)).unwrap();
        store
            .record(
                &"1".repeat(64),
                Uuid::now_v7(),
                MembershipDisposition::Active,
            )
            .unwrap();
        let journal = home
            .root()
            .join("profiles")
            .join("f".repeat(64))
            .join("membership.jsonl");
        fs::write(&journal, b"{\"schema_version\":1}\n").unwrap();
        assert_eq!(
            store.require_quiescent("restart").unwrap_err().code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn quiescence_rejects_a_persisted_index_that_disagrees_with_its_journal() {
        let (parent, home) = home();
        let store = GlobalMembershipStore::new(&home, &"9".repeat(64)).unwrap();
        let run_id = Uuid::now_v7();
        store
            .record(&"1".repeat(64), run_id, MembershipDisposition::Released)
            .unwrap();
        let persisted = home
            .root()
            .join("profiles")
            .join("9".repeat(64))
            .join("members.json");
        let mut index: GlobalMembershipIndex =
            serde_json::from_slice(&fs::read(&persisted).unwrap()).unwrap();
        index.revision += 1;
        fs::write(&persisted, serde_json::to_vec_pretty(&index).unwrap()).unwrap();
        let error = store
            .require_quiescent_under_server_lock("selected", "stop")
            .unwrap_err();
        assert_eq!(error.code, "PROFILE_MEMBERSHIP_INCOMPLETE");
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn quiescence_guard_serializes_admission_with_generation_change() {
        let (parent, home) = home();
        let store = Arc::new(GlobalMembershipStore::new(&home, &"8".repeat(64)).unwrap());
        let guard = store.acquire_quiescence("generation_change").unwrap();
        let contender = Arc::clone(&store);
        let handle = thread::spawn(move || {
            contender
                .record(
                    &"7".repeat(64),
                    Uuid::now_v7(),
                    MembershipDisposition::Active,
                )
                .unwrap()
        });
        thread::sleep(std::time::Duration::from_millis(20));
        assert!(!handle.is_finished());
        drop(guard);
        assert_eq!(handle.join().unwrap().revision, 1);
        fs::remove_dir_all(parent).unwrap();
    }
}
