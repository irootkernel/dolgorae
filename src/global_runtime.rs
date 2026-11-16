//! Global Profile runtime, membership, and immutable Run binding.

use crate::darwin::DarwinSystem;
use crate::global_profile::GlobalProfileStore;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::profile::{CompatibilityVerdict, ExecutableIdentity, ProfileSnapshot, ServerState};
use crate::workspace::{
    RuntimeProfile, SystemWorkspacePlatform, atomic_create, create_directory,
    open_secure_file_if_present, sync_directory, verify_secure_directory,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const MINIMUM_CREATION_CODEX_VERSION: &str = "0.169.0";

pub(crate) fn creation_version_supported(version: &str) -> bool {
    matches!(
        (crate::profile::parse_version("runtime_admission", version),
         crate::profile::parse_version("runtime_admission", MINIMUM_CREATION_CODEX_VERSION)),
        (Ok(actual), Ok(minimum)) if actual >= minimum
    )
}

#[cfg(test)]
pub(crate) const CURRENT_CODEX_VERSION: &str = MINIMUM_CREATION_CODEX_VERSION;
#[cfg(test)]
pub(crate) const CURRENT_CODEX_SHA256: &str =
    "739cda86e15d30a6e2b40cf8dcbbfb988aaf48b305b27065573bb8e9fd41d002";
#[cfg(test)]
pub(crate) const CURRENT_COMPANION_SHA256: &str =
    "10feae2abe764bcf2273e241ace67aecc360eb8c28a82a2988bf1d8498a9bacf";

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_MEMBERSHIP_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MEMBERSHIP_CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LEGACY_MEMBERSHIP_BYTES: u64 = 64 * 1024 * 1024;

#[path = "global_runtime_observation.rs"]
mod observation;

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
pub struct GlobalRuntimeDiscovery {
    pub schema_version: u32,
    pub selected_profile: String,
    pub server_key: String,
    pub server_epoch: u64,
    pub socket_path: String,
    pub binding_sha256: String,
}

impl ServerState {
    pub fn validate_for_global_restart(&self) -> Result<(), MachineError> {
        if self.schema_version != 2
            || self.server_epoch == 0
            || self.server_key != self.snapshot.server_key
        {
            return Err(binding_invalid(
                "global Profile Server state cannot reconstruct its launch generation",
            ));
        }
        validate_server_key(&self.server_key)
    }

    pub fn discover_global(
        &self,
        binding: &GlobalProfileBinding,
    ) -> Result<GlobalRuntimeDiscovery, MachineError> {
        self.validate_for_global_restart()?;
        binding.validate_for_recovery()?;
        if binding.server_key != self.server_key
            || GlobalLaunchSnapshot::from(&binding.launch_snapshot)
                != GlobalLaunchSnapshot::from(&self.snapshot)
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

    pub fn global_diagnostic(&self, binding: &GlobalProfileBinding) -> Result<Value, MachineError> {
        let discovery = self.discover_global(binding)?;
        Ok(json!({
            "schema_version": 2,
            "profile": discovery.selected_profile,
            "server_key": discovery.server_key,
            "server_epoch": discovery.server_epoch,
            "lifecycle": self.lifecycle,
            "membership_revision": self.membership_revision,
            "launch_snapshot_sha256": canonical_digest(&GlobalLaunchSnapshot::from(&self.snapshot))?,
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

    /// Caller-qualified preparation is deliberately unrecorded. Admission owns
    /// the Current check and the subsequent registry-locked history publication.
    pub(crate) fn prepare_qualified(
        self,
        home: &DolgoraeHome,
    ) -> Result<GlobalProfileBinding, MachineError> {
        let snapshot = crate::profile::qualified_snapshot_for_global(
            home,
            &self.selected_name,
            &self.definition,
        )?;
        self.bind_qualified(snapshot)
    }

    /// A new successor binding, separate from accepted enabled Profile launches.
    pub fn prepare_specialist(
        self,
        home: &DolgoraeHome,
        native_subagents: bool,
    ) -> Result<GlobalProfileBinding, MachineError> {
        let snapshot = crate::profile::specialist_snapshot_for_global(
            home,
            &self.selected_name,
            &self.definition,
            native_subagents,
        )?;
        self.bind_successor(snapshot, native_subagents)
    }

    pub(crate) fn bind_successor(
        self,
        launch_snapshot: ProfileSnapshot,
        native_subagents: bool,
    ) -> Result<GlobalProfileBinding, MachineError> {
        let mut expected = self
            .definition
            .argv
            .iter()
            .skip(1)
            .cloned()
            .collect::<Vec<_>>();
        if !expected.iter().any(|arg| arg == "--strict-config") {
            expected.push("--strict-config".to_owned());
        }
        expected.extend(
            [
                "--enable",
                "multi_agent",
                "--disable",
                "shell_snapshot",
                "-c",
                "project_doc_max_bytes=0",
            ]
            .map(str::to_owned),
        );
        let (enabled, disabled) = if native_subagents {
            (
                vec!["multi_agent".to_owned()],
                vec!["shell_snapshot".to_owned()],
            )
        } else {
            expected.extend(["--disable", "multi_agent"].map(str::to_owned));
            (
                vec![],
                vec!["multi_agent".to_owned(), "shell_snapshot".to_owned()],
            )
        };
        if self.schema_version != 1
            || launch_snapshot.schema_version != 1
            || self
                .definition
                .environment
                .iter()
                .any(|(key, value)| launch_snapshot.sanitized_environment.get(key) != Some(value))
            || launch_snapshot.normalized_argv.get(1..) != Some(expected.as_slice())
            || launch_snapshot.enabled_features != enabled
            || launch_snapshot.disabled_features != disabled
            || launch_snapshot.profile_name != self.selected_name
            || launch_snapshot
                .sanitized_environment
                .get("DOLGORAE_SUCCESSOR_PROFILE")
                != Some(&self.selected_name)
            || self.definition_sha256 != canonical_digest(&self.definition)?
        {
            return Err(binding_invalid("successor launch policy disagrees"));
        }
        Ok(GlobalProfileBinding {
            schema_version: 2,
            selected_name: self.selected_name,
            definition: self.definition,
            definition_sha256: self.definition_sha256,
            server_key: launch_snapshot.server_key.clone(),
            launch_snapshot_sha256: canonical_digest(&launch_snapshot)?,
            launch_snapshot,
        })
    }

    pub(crate) fn bind_qualified(
        self,
        launch_snapshot: ProfileSnapshot,
    ) -> Result<GlobalProfileBinding, MachineError> {
        self.bind_policy(launch_snapshot, true)
    }

    pub fn bind(
        self,
        launch_snapshot: ProfileSnapshot,
    ) -> Result<GlobalProfileBinding, MachineError> {
        self.bind_policy(launch_snapshot, false)
    }

    fn bind_policy(
        self,
        launch_snapshot: ProfileSnapshot,
        qualified: bool,
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
        if qualified {
            expected_arguments.extend(["--disable".to_owned(), "shell_snapshot".to_owned()]);
            if launch_snapshot.enabled_features != ["multi_agent"]
                || launch_snapshot.disabled_features != ["shell_snapshot"]
            {
                return Err(binding_invalid("qualified launch feature policy disagrees"));
            }
        }
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
    /// New admission permits untested newer runtimes after the normal protocol
    /// probes. Actual process, socket and configuration evidence stays Worker-owned.
    pub(crate) fn caller_workspace_supported(&self) -> bool {
        self.current_runtime_eligible() && self.uses_qualified_launch_policy()
    }

    pub(crate) fn uses_qualified_launch_policy(&self) -> bool {
        ResolvedGlobalProfile::from_definition(&self.selected_name, self.definition.clone())
            .and_then(|resolved| {
                resolved
                    .clone()
                    .bind_qualified(self.launch_snapshot.clone())
                    .or_else(|_| {
                        resolved.bind_successor(
                            self.launch_snapshot.clone(),
                            self.launch_snapshot.enabled_features == ["multi_agent"],
                        )
                    })
            })
            .is_ok()
    }

    fn current_runtime_eligible(&self) -> bool {
        self.validate_for_recovery().is_ok()
            && creation_version_supported(&self.launch_snapshot.codex_version)
            && self.launch_snapshot.compatibility_verdict != CompatibilityVerdict::Rejected
    }

    /// Retained execution uses its immutable accepted binding, without resolving
    /// the current registry or transferring qualification between executables.
    pub(crate) fn accepted_caller_workspace_supported(&self) -> bool {
        self.caller_workspace_supported()
            || (self.validate_for_recovery().is_ok()
                && self.launch_snapshot.codex_version == "0.167.2"
                && self.launch_snapshot.executable_identity.sha256
                    == "97bc82fd46f117cf10efb95e6ff0c81dc4784f5797bcdc2cb75093ba09e4def0")
            || (self.validate_for_recovery().is_ok()
                && self.launch_snapshot.codex_version == "0.164.0"
                && self.launch_snapshot.executable_identity.sha256
                    == "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf")
            || (self.validate_for_recovery().is_ok()
                && self.launch_snapshot.codex_version == "0.159.3"
                && self.launch_snapshot.executable_identity.sha256
                    == "4d210f7c5a18fd0386434df23b5bdbb8c0e7257d3e8a2b30b0769c8bbe99a878")
    }

    /// The native reader/history fixture qualifies this bounded tool surface,
    /// independently of caller startup and Profile readiness.
    pub(crate) fn checked_review_supported(&self) -> bool {
        self.caller_workspace_supported() && self.checked_review_configuration_supported()
    }

    pub(crate) fn accepted_checked_review_supported(&self) -> bool {
        self.accepted_caller_workspace_supported() && self.checked_review_configuration_supported()
    }

    fn checked_review_configuration_supported(&self) -> bool {
        let snapshot = &self.launch_snapshot;
        (snapshot.enabled_features == ["multi_agent"]
            || snapshot
                .disabled_features
                .iter()
                .any(|feature| feature == "multi_agent"))
            && [
                "mcp_servers",
                "apps",
                "agents",
                "hooks",
                "notify",
                "plugins",
                "marketplaces",
                "skills",
            ]
            .iter()
            .all(|name| {
                snapshot
                    .process_static_configuration
                    .get(*name)
                    .is_none_or(|value| {
                        value.is_null()
                            || value.as_object().is_some_and(|fields| fields.is_empty())
                            || value.as_array().is_some_and(|items| items.is_empty())
                    })
            })
    }

    /// Call only after new qualified-runtime admission. The registry lock rechecks
    /// the originally resolved definition before publishing the binding record.
    pub(crate) fn record_qualified(&self, home: &DolgoraeHome) -> Result<(), MachineError> {
        if !self.caller_workspace_supported() {
            return Err(binding_invalid(
                "unqualified candidate cannot be recorded by qualified admission",
            ));
        }
        GlobalProfileStore::new(home).record_binding(crate::global_profile::ProfileBindingRecord {
            selected_name: self.selected_name.clone(),
            definition_sha256: self.definition_sha256.clone(),
            server_key: self.server_key.clone(),
            launch_snapshot_sha256: self.launch_snapshot_sha256.clone(),
        })
    }

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
    pub fn for_disposition(disposition: MembershipDisposition) -> Self {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipSegment {
    path: String,
    byte_length: u64,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipCheckpoint {
    schema: String,
    index: GlobalMembershipIndex,
    head_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipManifest {
    schema: String,
    server_key: String,
    generation: u64,
    operation_id: Uuid,
    checkpoint_sha256: String,
    history: Vec<MembershipSegment>,
}

struct PinnedMembership {
    journal: PathBuf,
    history: Vec<MembershipSegment>,
    base: Vec<GlobalMembershipRecord>,
    generation: u64,
    operation_id: Option<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipPending {
    schema: String,
    operation_id: Uuid,
    generation: u64,
    journal_sha256: String,
}

/// Actual durable boundaries of a membership generation switch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipSwitchBarrier {
    PendingSynced,
    CheckpointSynced,
    AppendSynced,
    ManifestSynced,
    DirectorySynced,
    SelectionReplaced,
    SelectionSynced,
    ActivationSynced,
}

pub struct GlobalMembershipStore {
    home_root: PathBuf,
    profile: String,
    server_key: String,
}

/// Holds the server membership lock across a destructive lifecycle commit.
/// Dropping the guard reopens admission.
pub struct GlobalQuiescenceGuard {
    _lock: Option<File>,
    pub index: GlobalMembershipIndex,
}

/// Remove one selected alias only after global membership under its resolved
/// server key is proven quiescent while the registry lock is still held.
pub fn remove_global_profile(home: &DolgoraeHome, selected_name: &str) -> Result<(), MachineError> {
    GlobalProfileStore::new(home)
        .remove_if(selected_name, |_definition| {
            let server_keys = crate::global_profile::bound_server_keys_under_root_lock(
                home.root(),
                selected_name,
            )?;
            let mut guards = Vec::with_capacity(server_keys.len());
            for server_key in server_keys {
                guards.push(
                    GlobalMembershipStore::new(home, selected_name, &server_key)?
                        .acquire_quiescence_under_registry_lock(selected_name, "remove")?,
                );
            }
            Ok(guards)
        })
        .map(|_| ())
}

impl GlobalMembershipStore {
    pub(crate) fn with_maintenance_server_lock<T>(
        &self,
        action: impl FnOnce(&Path, &Self) -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        verify_secure_directory(&self.home_root, DarwinSystem.current_uid())?;
        let profiles = self.home_root.join("profiles");
        verify_secure_directory(&profiles, DarwinSystem.current_uid())?;
        let root = profiles.join(&self.server_key);
        verify_secure_directory(&root, DarwinSystem.current_uid())?;
        let path = root.join("server.lock");
        let lock =
            open_secure_file_if_present(&path, DarwinSystem.current_uid())?.ok_or_else(|| {
                membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership maintenance lock is missing",
                )
            })?;
        DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|error| path_error(&path, error))?;
        action(&root, self)
    }
    pub fn new(home: &DolgoraeHome, profile: &str, server_key: &str) -> Result<Self, MachineError> {
        Self::from_root(home.root(), profile, server_key)
    }

    pub(crate) fn from_root(
        root: &Path,
        profile: &str,
        server_key: &str,
    ) -> Result<Self, MachineError> {
        if profile.is_empty() {
            return Err(MachineError::invalid_argument(
                "profile",
                "a global Profile name is required",
            ));
        }
        validate_server_key(server_key)?;
        Ok(Self {
            home_root: root.to_path_buf(),
            profile: profile.to_owned(),
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
        facts: GlobalMembershipFacts,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        validate_member_identity(workspace_id)?;
        validate_membership_facts(&facts)?;
        self.record_under_server_lock(root, workspace_id, run_id, disposition, facts)
    }

    /// Release every attachment after the exact physical server has been
    /// proven absent. This does not invent a terminal Run result.
    pub(crate) fn release_after_server_absence_under_lifecycle_locks(
        &self,
        root: &Path,
        observed_epoch: u64,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        let mut index = self.load_locked(root)?;
        let members = index
            .members
            .values()
            .filter(|member| member.disposition != MembershipDisposition::Released)
            .cloned()
            .collect::<Vec<_>>();
        for member in members {
            let lifecycle = if member.lifecycle == "operator_interrupt_unknown" {
                "interrupted_unknown"
            } else if member.lifecycle.starts_with("operator_interrupt_") {
                member.lifecycle.as_str()
            } else if member.observed_epoch == Some(observed_epoch) {
                "interrupted_unknown"
            } else {
                "stale_generation_reconciled"
            };
            index = self.record_under_server_lock(
                root,
                &member.workspace_id,
                member.run_id,
                MembershipDisposition::Released,
                GlobalMembershipFacts {
                    controller_id: member.controller_id,
                    worker_generation: member.worker_generation,
                    thread_id: member.thread_id,
                    connection_id: member.connection_id,
                    lifecycle: lifecycle.to_owned(),
                    writer: false,
                    observed_epoch: member.observed_epoch,
                    runtime_locator: member.runtime_locator,
                },
            )?;
        }
        Ok(index)
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
        mut facts: GlobalMembershipFacts,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        if path_present(&root.join("membership-pending.json"))?
            || path_present(&self.home_root.join("profile-maintenance-active.json"))?
        {
            return Err(MachineError::new(
                "PROFILE_SERVER_BUSY",
                "retained membership maintenance requires recovery",
                true,
                json!({"profile":self.profile,"server_key":self.server_key,"reason":"resume the original membership maintenance before appending"}),
            ));
        }
        let pinned = pin_membership(root, &self.profile, &self.server_key)?;
        let mut records = replay_membership(&pinned, &self.profile, &self.server_key)?;
        let previous_index =
            membership_index(&self.profile, &self.server_key, root, &pinned, &records)?;
        let previous_state = self.verify_consistency(root, &previous_index)?;
        if let Some(previous) = records
            .iter()
            .rev()
            .find(|record| record.workspace_id == workspace_id && record.run_id == run_id)
        {
            facts.controller_id = facts.controller_id.or(previous.controller_id);
            facts.worker_generation = facts.worker_generation.or(previous.worker_generation);
            facts.thread_id = facts.thread_id.or_else(|| previous.thread_id.clone());
            facts.connection_id = facts.connection_id.or(previous.connection_id);
            facts.observed_epoch = facts.observed_epoch.or(previous.observed_epoch);
            facts.runtime_locator = facts
                .runtime_locator
                .or_else(|| previous.runtime_locator.clone());
            if previous.disposition == disposition
                && previous.controller_id == facts.controller_id
                && previous.worker_generation == facts.worker_generation
                && previous.thread_id == facts.thread_id
                && previous.connection_id == facts.connection_id
                && previous.lifecycle == facts.lifecycle
                && previous.writer == facts.writer
                && previous.observed_epoch == facts.observed_epoch
                && previous.runtime_locator == facts.runtime_locator
            {
                return Ok(previous_index);
            }
        }
        let revision = previous_index.revision.checked_add(1).ok_or_else(|| {
            membership_incomplete(
                &self.profile,
                &self.server_key,
                "membership revision overflow",
            )
        })?;
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
        retain_membership_activation(root, &pinned, &self.server_key)?;
        append_record(&pinned.journal, &record)?;
        records.push(record);
        let index = membership_index(&self.profile, &self.server_key, root, &pinned, &records)?;
        store_index(&root.join("members.json"), &index)?;
        let state_path = root.join("state.json");
        if let Some(mut state) = previous_state {
            state.membership_revision = index.revision;
            let temporary = root.join(format!(".state-{}.json", Uuid::now_v7()));
            atomic_create(
                &SystemWorkspacePlatform,
                &temporary,
                &serde_json::to_vec_pretty(&state).map_err(internal)?,
                0o600,
            )
            .map_err(|error| path_error(&temporary, error))?;
            fs::rename(&temporary, &state_path).map_err(|error| path_error(&state_path, error))?;
            sync_directory(root).map_err(|error| path_error(root, error))?;
        }
        Ok(index)
    }

    pub(crate) fn maintenance_basis_under_server_lock(
        &self,
        root: &Path,
    ) -> Result<(u64, GlobalMembershipIndex), MachineError> {
        let pinned = pin_membership(root, &self.profile, &self.server_key)?;
        let records = replay_membership_bounded(
            &pinned,
            &self.profile,
            &self.server_key,
            MAX_LEGACY_MEMBERSHIP_BYTES,
        )?;
        let index = membership_index_bounded(
            &self.profile,
            &self.server_key,
            root,
            &pinned,
            &records,
            MAX_LEGACY_MEMBERSHIP_BYTES,
        )?;
        self.verify_consistency(root, &index)?;
        Ok((pinned.generation, index))
    }

    pub(crate) fn verify_maintenance_completion_under_server_lock(
        &self,
        root: &Path,
        operation_id: Uuid,
        generation: u64,
        original_digest: &str,
        verify_selected: bool,
    ) -> Result<(), MachineError> {
        let directory = root
            .join("membership-generations")
            .join(operation_id.to_string());
        let manifest: MembershipManifest = membership_control(
            &directory.join("activation.json"),
            &self.profile,
            &self.server_key,
        )?;
        let bytes = membership_control_bytes(
            &directory.join("checkpoint.json"),
            &self.profile,
            &self.server_key,
        )?;
        let checkpoint: MembershipCheckpoint = serde_json::from_slice(&bytes).map_err(|_| {
            membership_incomplete(
                &self.profile,
                &self.server_key,
                "completed membership checkpoint is invalid",
            )
        })?;
        if manifest.schema != "dolgorae.membership-manifest/v1"
            || manifest.operation_id != operation_id
            || manifest.generation != generation + 1
            || manifest.server_key != self.server_key
            || manifest.checkpoint_sha256 != sha256_hex(&bytes)
            || checkpoint.schema != "dolgorae.membership-checkpoint/v1"
            || checkpoint.index.journal_sha256 != original_digest
        {
            return Err(membership_incomplete(
                &self.profile,
                &self.server_key,
                "membership completion disagrees with its original scope",
            ));
        }
        for segment in &manifest.history {
            membership_segment_bytes(root, segment, &self.profile, &self.server_key)?;
        }
        if verify_selected {
            let pinned = pin_membership(root, &self.profile, &self.server_key)?;
            if pinned.operation_id != Some(operation_id) || pinned.generation != generation + 1 {
                return Err(membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "selected membership disagrees with retained completion",
                ));
            }
            let records = replay_membership(&pinned, &self.profile, &self.server_key)?;
            let index = membership_index(&self.profile, &self.server_key, root, &pinned, &records)?;
            self.verify_consistency(root, &index)?;
        }
        Ok(())
    }

    pub(crate) fn compact_under_server_lock(
        &self,
        root: &Path,
        operation_id: Uuid,
        expected_generation: u64,
        expected_digest: &str,
        barrier: &mut impl FnMut(MembershipSwitchBarrier) -> Result<(), MachineError>,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        let pinned = pin_membership(root, &self.profile, &self.server_key)?;
        let records = replay_membership_bounded(
            &pinned,
            &self.profile,
            &self.server_key,
            MAX_LEGACY_MEMBERSHIP_BYTES,
        )?;
        let index = membership_index_bounded(
            &self.profile,
            &self.server_key,
            root,
            &pinned,
            &records,
            MAX_LEGACY_MEMBERSHIP_BYTES,
        )?;
        self.verify_consistency(root, &index)?;
        let pending = MembershipPending {
            schema: "dolgorae.membership-maintenance/v1".to_owned(),
            operation_id,
            generation: expected_generation,
            journal_sha256: expected_digest.to_owned(),
        };
        let pending_path = root.join("membership-pending.json");
        if path_present(&pending_path)? {
            let retained: MembershipPending =
                membership_control(&pending_path, &self.profile, &self.server_key)?;
            if retained != pending {
                return Err(membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership maintenance has a different retained owner",
                ));
            }
        }
        if pinned.operation_id == Some(operation_id)
            && pinned.generation == expected_generation.saturating_add(1)
        {
            let checkpoint: MembershipCheckpoint = membership_control(
                &root
                    .join("membership-generations")
                    .join(operation_id.to_string())
                    .join("checkpoint.json"),
                &self.profile,
                &self.server_key,
            )?;
            if checkpoint.index.journal_sha256 != expected_digest {
                return Err(membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "original membership maintenance digest changed",
                ));
            }
            retain_membership_activation(root, &pinned, &self.server_key)?;
            if path_present(&pending_path)? {
                fs::remove_file(&pending_path)
                    .and_then(|()| sync_directory(root))
                    .map_err(|error| path_error(&pending_path, error))?;
            }
            return Ok(index);
        }
        if pinned.generation != expected_generation
            || index.journal_sha256 != expected_digest
            || operation_id.get_version_num() != 7
        {
            return Err(membership_incomplete(
                &self.profile,
                &self.server_key,
                "membership maintenance source changed",
            ));
        }
        let generation = expected_generation.checked_add(1).ok_or_else(|| {
            membership_incomplete(
                &self.profile,
                &self.server_key,
                "membership generation overflow",
            )
        })?;
        let checkpoint = MembershipCheckpoint {
            schema: "dolgorae.membership-checkpoint/v1".to_owned(),
            index: index.clone(),
            head_sha256: records.last().map_or_else(
                || ZERO_HASH.to_owned(),
                |record| record.record_sha256.clone(),
            ),
        };
        let checkpoint_bytes = serde_json::to_vec(&checkpoint).map_err(internal)?;
        if checkpoint_bytes.len() as u64 > MAX_MEMBERSHIP_CHECKPOINT_BYTES {
            return Err(membership_capacity(
                &self.profile,
                &self.server_key,
                "membership checkpoint exceeds 64 MiB",
            ));
        }
        if pinned.history.len() >= 4096 {
            return Err(membership_capacity(
                &self.profile,
                &self.server_key,
                "membership retained generation bound reached",
            ));
        }
        if !path_present(&pending_path)? {
            retain_membership_file(
                &pending_path,
                &serde_json::to_vec(&pending).map_err(internal)?,
            )?;
            sync_directory(root).map_err(|error| path_error(root, error))?;
        }
        barrier(MembershipSwitchBarrier::PendingSynced)?;
        let generations = root.join("membership-generations");
        if !path_present(&generations)? {
            create_directory(&generations, 0o700)
                .map_err(|error| path_error(&generations, error))?;
        }
        verify_secure_directory(&generations, DarwinSystem.current_uid())?;
        let directory = generations.join(operation_id.to_string());
        if !path_present(&directory)? {
            create_directory(&directory, 0o700).map_err(|error| path_error(&directory, error))?;
        }
        verify_secure_directory(&directory, DarwinSystem.current_uid())?;
        retain_membership_file(&directory.join("checkpoint.json"), &checkpoint_bytes)?;
        barrier(MembershipSwitchBarrier::CheckpointSynced)?;
        retain_membership_file(&directory.join("append.jsonl"), b"")?;
        barrier(MembershipSwitchBarrier::AppendSynced)?;
        let old_bytes =
            match open_secure_file_if_present(&pinned.journal, DarwinSystem.current_uid())? {
                Some(file) => {
                    let mut bytes = Vec::new();
                    file.take(MAX_LEGACY_MEMBERSHIP_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|error| path_error(&pinned.journal, error))?;
                    bytes
                }
                None => Vec::new(),
            };
        let mut history = pinned.history.clone();
        if !path_present(&pinned.journal)? {
            retain_membership_file(&pinned.journal, b"")?;
            sync_directory(root).map_err(|error| path_error(root, error))?;
        }
        let relative = pinned
            .journal
            .strip_prefix(root)
            .map_err(internal)?
            .to_str()
            .ok_or_else(|| internal("membership journal path is not UTF-8"))?
            .to_owned();
        history.push(MembershipSegment {
            path: relative,
            byte_length: old_bytes.len() as u64,
            sha256: sha256_hex(&old_bytes),
        });
        let manifest = MembershipManifest {
            schema: "dolgorae.membership-manifest/v1".to_owned(),
            server_key: self.server_key.clone(),
            generation,
            operation_id,
            checkpoint_sha256: sha256_hex(&checkpoint_bytes),
            history,
        };
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(internal)?;
        retain_membership_file(&directory.join("manifest.json"), &manifest_bytes)?;
        barrier(MembershipSwitchBarrier::ManifestSynced)?;
        sync_directory(&directory)
            .and_then(|()| sync_directory(&generations))
            .and_then(|()| sync_directory(root))
            .map_err(|error| path_error(&directory, error))?;
        barrier(MembershipSwitchBarrier::DirectorySynced)?;
        let selected = root.join("membership-selection.json");
        let temporary = root.join(format!(".membership-selection-{operation_id}.json"));
        retain_membership_file(&temporary, &manifest_bytes)?;
        fs::rename(&temporary, &selected).map_err(|error| path_error(&selected, error))?;
        barrier(MembershipSwitchBarrier::SelectionReplaced)?;
        sync_directory(root).map_err(|error| path_error(root, error))?;
        barrier(MembershipSwitchBarrier::SelectionSynced)?;
        let current = pin_membership(root, &self.profile, &self.server_key)?;
        retain_membership_activation(root, &current, &self.server_key)?;
        barrier(MembershipSwitchBarrier::ActivationSynced)?;
        fs::remove_file(&pending_path)
            .and_then(|()| sync_directory(root))
            .map_err(|error| path_error(&pending_path, error))?;
        Ok(index)
    }

    pub fn load(&self) -> Result<GlobalMembershipIndex, MachineError> {
        let root = self.ensure_root()?;
        let _lock = lock_file(&root.join("server.lock"))?;
        self.load_locked(&root)
    }

    pub(crate) fn load_existing(&self) -> Result<GlobalMembershipIndex, MachineError> {
        self.with_maintenance_server_lock(|root, store| store.load_locked(root))
    }

    pub(crate) fn load_under_server_lock(
        &self,
        root: &Path,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        self.load_locked(root)
    }

    pub fn tombstone_orphan<F>(
        &self,
        workspace_id: &str,
        run_id: Uuid,
        verify_orphan: F,
    ) -> Result<GlobalMembershipIndex, MachineError>
    where
        F: FnOnce(&GlobalMembershipRecord) -> Result<bool, MachineError>,
    {
        validate_member_identity(workspace_id)?;
        let root = self.ensure_root()?;
        let _lock = lock_file(&root.join("server.lock"))?;
        let index = self.load_locked(&root)?;
        let identity = format!("{workspace_id}:{run_id}");
        let member = index.members.get(&identity).ok_or_else(|| {
            membership_incomplete(
                &self.profile,
                &self.server_key,
                "confirmed orphan is not in membership",
            )
        })?;
        if member.disposition == MembershipDisposition::Released || !verify_orphan(member)? {
            return Err(membership_incomplete(
                &self.profile,
                &self.server_key,
                "confirmed member is not the same live orphan",
            ));
        }
        self.record_under_server_lock(
            &root,
            workspace_id,
            run_id,
            MembershipDisposition::Released,
            GlobalMembershipFacts {
                controller_id: member.controller_id,
                worker_generation: member.worker_generation,
                thread_id: member.thread_id.clone(),
                connection_id: member.connection_id,
                lifecycle: "tombstone_orphan".to_owned(),
                writer: false,
                observed_epoch: member.observed_epoch,
                runtime_locator: member.runtime_locator.clone(),
            },
        )
    }

    /// Verify quiescence when the caller already holds this server key's
    /// `server.lock` as part of the lifecycle lock hierarchy.
    pub fn require_quiescent_under_server_lock(
        &self,
        profile: &str,
        operation: &str,
    ) -> Result<GlobalMembershipIndex, MachineError> {
        let root = self.home_root.join("profiles").join(&self.server_key);
        verify_secure_directory(&root, DarwinSystem.current_uid())?;
        let index = self.load_locked(&root)?;
        let blockers = index
            .members
            .values()
            .filter(|member| member.disposition != MembershipDisposition::Released)
            .collect::<Vec<_>>();
        if !blockers.is_empty() {
            return Err(MachineError::new(
                "PROFILE_SERVER_BUSY",
                "global Profile membership blocks the lifecycle operation",
                true,
                json!({
                    "server_key": self.server_key,
                    "profile": profile,
                    "reason": format!("{operation} would interrupt {} live run member(s); use the explicit operator interrupt flow", blockers.len()),
                }),
            ));
        }
        Ok(index)
    }

    fn load_locked(&self, root: &Path) -> Result<GlobalMembershipIndex, MachineError> {
        let pinned = pin_membership(root, &self.profile, &self.server_key)?;
        let records = replay_membership(&pinned, &self.profile, &self.server_key)?;
        let index = membership_index(&self.profile, &self.server_key, root, &pinned, &records)?;
        self.verify_consistency(root, &index)?;
        Ok(index)
    }

    fn verify_consistency(
        &self,
        root: &Path,
        index: &GlobalMembershipIndex,
    ) -> Result<Option<ServerState>, MachineError> {
        self.verify_persisted_index(root, index)?;
        let path = root.join("state.json");
        let Some(file) = open_secure_file_if_present(&path, DarwinSystem.current_uid())? else {
            return Ok(None);
        };
        let state: ServerState = serde_json::from_reader(file).map_err(|_| {
            membership_incomplete(&self.profile, &self.server_key, "server state is malformed")
        })?;
        if state.schema_version != 2
            || state.server_key != self.server_key
            || state.snapshot.server_key != self.server_key
            || state.membership_revision != index.revision
        {
            return Err(membership_incomplete(
                &self.profile,
                &self.server_key,
                "server state identity or membership revision disagrees with journal",
            ));
        }
        Ok(Some(state))
    }

    fn verify_persisted_index(
        &self,
        root: &Path,
        index: &GlobalMembershipIndex,
    ) -> Result<(), MachineError> {
        let persisted = root.join("members.json");
        if let Some(file) = open_secure_file_if_present(&persisted, DarwinSystem.current_uid())? {
            let actual: GlobalMembershipIndex = serde_json::from_reader(file).map_err(|_| {
                membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership index is invalid",
                )
            })?;
            if actual != *index {
                return Err(membership_incomplete(
                    &self.profile,
                    &self.server_key,
                    "membership index disagrees with journal",
                ));
            }
        }
        Ok(())
    }

    pub fn require_quiescent(&self, profile: &str, operation: &str) -> Result<(), MachineError> {
        self.acquire_quiescence(profile, operation).map(|_| ())
    }

    pub fn acquire_quiescence(
        &self,
        profile: &str,
        operation: &str,
    ) -> Result<GlobalQuiescenceGuard, MachineError> {
        let root = self.ensure_root()?;
        self.acquire_quiescence_locked_root(&root, profile, operation)
    }

    fn acquire_quiescence_under_registry_lock(
        &self,
        profile: &str,
        operation: &str,
    ) -> Result<GlobalQuiescenceGuard, MachineError> {
        let root = self.home_root.join("profiles").join(&self.server_key);
        if !path_present(&root)? {
            return Ok(GlobalQuiescenceGuard {
                _lock: None,
                index: derive_index(&self.server_key, &root.join("membership.jsonl"), &[])?,
            });
        }
        verify_secure_directory(&root, DarwinSystem.current_uid())?;
        self.acquire_quiescence_locked_root(&root, profile, operation)
    }

    fn acquire_quiescence_locked_root(
        &self,
        root: &Path,
        profile: &str,
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
            Ok(GlobalQuiescenceGuard {
                _lock: Some(lock),
                index,
            })
        } else {
            Err(MachineError::new(
                "PROFILE_SERVER_BUSY",
                "global Profile membership blocks the lifecycle operation",
                true,
                json!({
                    "server_key": self.server_key,
                    "profile": profile,
                    "reason": format!("{operation} would interrupt {} live run member(s); use the explicit operator interrupt flow", blockers.len()),
                }),
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
        if !path_present(&profiles)? {
            create_directory(&profiles, 0o700).map_err(|error| path_error(&profiles, error))?;
            sync_directory(&self.home_root).map_err(|error| path_error(&self.home_root, error))?;
        }
        verify_secure_directory(&profiles, DarwinSystem.current_uid())?;
        let root = profiles.join(&self.server_key);
        if !path_present(&root)? {
            create_directory(&root, 0o700).map_err(|error| path_error(&root, error))?;
            sync_directory(&profiles).map_err(|error| path_error(&profiles, error))?;
        }
        verify_secure_directory(&root, DarwinSystem.current_uid())?;
        Ok(root)
    }
}

fn retain_membership_file(path: &Path, bytes: &[u8]) -> Result<(), MachineError> {
    if let Some(mut file) = open_secure_file_if_present(path, DarwinSystem.current_uid())? {
        let mut retained = Vec::new();
        (&mut file)
            .take(bytes.len() as u64 + 1)
            .read_to_end(&mut retained)
            .map_err(|error| path_error(path, error))?;
        if retained != bytes {
            return Err(internal(
                "prepared membership file differs from its retained source",
            ));
        }
        file.sync_all().map_err(|error| path_error(path, error))?;
    } else {
        atomic_create(&SystemWorkspacePlatform, path, bytes, 0o600)
            .map_err(|error| path_error(path, error))?;
    }
    Ok(())
}

fn membership_control_bytes(
    path: &Path,
    profile: &str,
    server_key: &str,
) -> Result<Vec<u8>, MachineError> {
    let file = open_secure_file_if_present(path, DarwinSystem.current_uid())?.ok_or_else(|| {
        membership_incomplete(profile, server_key, "membership control is missing")
    })?;
    let maximum = if path
        .file_name()
        .is_some_and(|name| name == "checkpoint.json")
    {
        MAX_MEMBERSHIP_CHECKPOINT_BYTES
    } else {
        MAX_MEMBERSHIP_BYTES
    };
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| path_error(path, error))?;
    if bytes.len() as u64 > maximum {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership control exceeds its bound",
        ));
    }
    parse(std::str::from_utf8(&bytes).map_err(|_| {
        membership_incomplete(profile, server_key, "membership control is not UTF-8")
    })?)
    .map_err(|_| {
        membership_incomplete(profile, server_key, "membership control JSON is invalid")
    })?;
    Ok(bytes)
}

fn membership_control<T: serde::de::DeserializeOwned>(
    path: &Path,
    profile: &str,
    server_key: &str,
) -> Result<T, MachineError> {
    let bytes = membership_control_bytes(path, profile, server_key)?;
    serde_json::from_slice(&bytes).map_err(|_| {
        membership_incomplete(profile, server_key, "membership control shape is invalid")
    })
}

fn membership_segment_path(
    root: &Path,
    relative: &str,
    profile: &str,
    server_key: &str,
) -> Result<PathBuf, MachineError> {
    if relative == "membership.jsonl" {
        return Ok(root.join(relative));
    }
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[0] != "membership-generations"
        || parts[2] != "append.jsonl"
        || !Uuid::parse_str(parts[1])
            .is_ok_and(|id| id.get_version_num() == 7 && id.to_string() == parts[1])
    {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership history contains an invalid relative path",
        ));
    }
    verify_secure_directory(&root.join(parts[0]), DarwinSystem.current_uid())?;
    verify_secure_directory(
        &root.join(parts[0]).join(parts[1]),
        DarwinSystem.current_uid(),
    )?;
    Ok(root.join(relative))
}

fn membership_segment_bytes(
    root: &Path,
    segment: &MembershipSegment,
    profile: &str,
    server_key: &str,
) -> Result<Vec<u8>, MachineError> {
    let path = membership_segment_path(root, &segment.path, profile, server_key)?;
    let file =
        open_secure_file_if_present(&path, DarwinSystem.current_uid())?.ok_or_else(|| {
            membership_incomplete(
                profile,
                server_key,
                "retained membership segment is missing",
            )
        })?;
    let mut bytes = Vec::new();
    file.take(MAX_LEGACY_MEMBERSHIP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| path_error(&path, error))?;
    if bytes.len() as u64 != segment.byte_length
        || bytes.len() as u64 > MAX_LEGACY_MEMBERSHIP_BYTES
        || sha256_hex(&bytes) != segment.sha256
    {
        return Err(membership_incomplete(
            profile,
            server_key,
            "retained membership segment integrity is invalid",
        ));
    }
    Ok(bytes)
}

fn pin_membership(
    root: &Path,
    profile: &str,
    server_key: &str,
) -> Result<PinnedMembership, MachineError> {
    let selected = root.join("membership-selection.json");
    let generations = root.join("membership-generations");
    let mut floor = 1;
    if path_present(&generations)? {
        verify_secure_directory(&generations, DarwinSystem.current_uid())?;
        for entry in fs::read_dir(&generations).map_err(|error| path_error(&generations, error))? {
            let entry = entry.map_err(|error| path_error(&generations, error))?;
            verify_secure_directory(&entry.path(), DarwinSystem.current_uid())?;
            let activation = entry.path().join("activation.json");
            if path_present(&activation)? {
                let manifest: MembershipManifest =
                    membership_control(&activation, profile, server_key)?;
                if manifest.schema != "dolgorae.membership-manifest/v1"
                    || manifest.server_key != server_key
                    || manifest.generation < 2
                    || entry.file_name().to_str()
                        != Some(manifest.operation_id.to_string().as_str())
                {
                    return Err(membership_incomplete(
                        profile,
                        server_key,
                        "membership activation identity is invalid",
                    ));
                }
                floor = floor.max(manifest.generation);
            }
        }
    }
    if !path_present(&selected)? {
        if floor > 1 {
            return Err(membership_incomplete(
                profile,
                server_key,
                "activated membership selection is missing",
            ));
        }
        return Ok(PinnedMembership {
            journal: root.join("membership.jsonl"),
            history: Vec::new(),
            base: Vec::new(),
            generation: 1,
            operation_id: None,
        });
    }
    let manifest: MembershipManifest = membership_control(&selected, profile, server_key)?;
    if manifest.schema != "dolgorae.membership-manifest/v1"
        || manifest.server_key != server_key
        || manifest.generation < 2
        || manifest.generation < floor
        || manifest.operation_id.get_version_num() != 7
        || manifest.history.is_empty()
        || manifest.history.len() > 4096
    {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership selection identity is invalid",
        ));
    }
    let directory = generations.join(manifest.operation_id.to_string());
    verify_secure_directory(&directory, DarwinSystem.current_uid())?;
    let retained: MembershipManifest =
        membership_control(&directory.join("manifest.json"), profile, server_key)?;
    if serde_json::to_value(&retained).map_err(internal)?
        != serde_json::to_value(&manifest).map_err(internal)?
    {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership selection disagrees with its retained manifest",
        ));
    }
    if path_present(&directory.join("activation.json"))? {
        let activation: MembershipManifest =
            membership_control(&directory.join("activation.json"), profile, server_key)?;
        if serde_json::to_value(activation).map_err(internal)?
            != serde_json::to_value(&manifest).map_err(internal)?
        {
            return Err(membership_incomplete(
                profile,
                server_key,
                "membership activation disagrees with selection",
            ));
        }
    }
    let checkpoint_path = directory.join("checkpoint.json");
    let checkpoint_bytes = membership_control_bytes(&checkpoint_path, profile, server_key)?;
    let checkpoint: MembershipCheckpoint =
        serde_json::from_slice(&checkpoint_bytes).map_err(|_| {
            membership_incomplete(
                profile,
                server_key,
                "membership checkpoint shape is invalid",
            )
        })?;
    if sha256_hex(&checkpoint_bytes) != manifest.checkpoint_sha256
        || checkpoint.schema != "dolgorae.membership-checkpoint/v1"
        || checkpoint.index.schema_version != 2
        || checkpoint.index.server_key != server_key
        || checkpoint
            .index
            .members
            .iter()
            .any(|(key, member)| key != &format!("{}:{}", member.workspace_id, member.run_id))
    {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership checkpoint integrity is invalid",
        ));
    }
    let mut base = checkpoint.index.members.into_values().collect::<Vec<_>>();
    base.sort_by_key(|record| record.revision);
    if base.last().map_or(0, |record| record.revision) != checkpoint.index.revision
        || base
            .last()
            .map_or(ZERO_HASH, |record| record.record_sha256.as_str())
            != checkpoint.head_sha256
    {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership checkpoint watermark is invalid",
        ));
    }
    Ok(PinnedMembership {
        journal: directory.join("append.jsonl"),
        history: manifest.history,
        base,
        generation: manifest.generation,
        operation_id: Some(manifest.operation_id),
    })
}

fn replay_membership(
    pinned: &PinnedMembership,
    profile: &str,
    server_key: &str,
) -> Result<Vec<GlobalMembershipRecord>, MachineError> {
    replay_membership_bounded(pinned, profile, server_key, MAX_MEMBERSHIP_BYTES)
}
fn replay_membership_bounded(
    pinned: &PinnedMembership,
    profile: &str,
    server_key: &str,
    maximum: u64,
) -> Result<Vec<GlobalMembershipRecord>, MachineError> {
    if pinned.operation_id.is_some() && !path_present(&pinned.journal)? {
        return Err(membership_incomplete(
            profile,
            server_key,
            "selected membership append is missing",
        ));
    }
    let mut records = pinned.base.clone();
    let revision = records.last().map_or(0, |record| record.revision);
    let hash = records
        .last()
        .map_or(ZERO_HASH, |record| record.record_sha256.as_str());
    let tail = replay_tail_bounded(
        &pinned.journal,
        profile,
        server_key,
        revision,
        hash,
        maximum,
    )?;
    records.extend(tail);
    Ok(records)
}

fn membership_index(
    profile: &str,
    server_key: &str,
    root: &Path,
    pinned: &PinnedMembership,
    records: &[GlobalMembershipRecord],
) -> Result<GlobalMembershipIndex, MachineError> {
    membership_index_bounded(
        profile,
        server_key,
        root,
        pinned,
        records,
        MAX_MEMBERSHIP_BYTES,
    )
}
fn membership_index_bounded(
    profile: &str,
    server_key: &str,
    root: &Path,
    pinned: &PinnedMembership,
    records: &[GlobalMembershipRecord],
    maximum: u64,
) -> Result<GlobalMembershipIndex, MachineError> {
    let mut index = derive_index(server_key, &pinned.journal, records)?;
    if !pinned.history.is_empty() {
        let mut digest = Sha256::new();
        for segment in &pinned.history {
            digest.update(membership_segment_bytes(
                root, segment, profile, server_key,
            )?);
        }
        let file = open_secure_file_if_present(&pinned.journal, DarwinSystem.current_uid())?
            .ok_or_else(|| internal("selected membership append is missing"))?;
        let mut bytes = Vec::new();
        file.take(maximum + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| path_error(&pinned.journal, error))?;
        if bytes.len() as u64 > maximum {
            return Err(membership_capacity(
                "",
                server_key,
                "selected membership append exceeds 8 MiB",
            ));
        }
        digest.update(bytes);
        index.journal_sha256 = format!("{:x}", digest.finalize());
    }
    Ok(index)
}

fn retain_membership_activation(
    root: &Path,
    pinned: &PinnedMembership,
    server_key: &str,
) -> Result<(), MachineError> {
    let Some(id) = pinned.operation_id else {
        return Ok(());
    };
    let directory = root.join("membership-generations").join(id.to_string());
    let path = directory.join("activation.json");
    let manifest: MembershipManifest =
        membership_control(&directory.join("manifest.json"), "", server_key)?;
    let bytes = serde_json::to_vec(&manifest).map_err(internal)?;
    if !path_present(&path)? {
        atomic_create(&SystemWorkspacePlatform, &path, &bytes, 0o600)
            .map_err(|error| path_error(&path, error))?;
        sync_directory(&directory).map_err(|error| path_error(&directory, error))?;
    }
    Ok(())
}

fn path_present(path: &Path) -> Result<bool, MachineError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(path_error(path, error)),
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
    let mut journal_digest = Sha256::new();
    if let Some(mut file) = open_secure_file_if_present(journal, DarwinSystem.current_uid())? {
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| path_error(journal, error))?;
            if count == 0 {
                break;
            }
            journal_digest.update(&buffer[..count]);
        }
    }
    Ok(GlobalMembershipIndex {
        schema_version: 2,
        server_key: server_key.to_owned(),
        revision: records.last().map_or(0, |record| record.revision),
        journal_sha256: format!("{:x}", journal_digest.finalize()),
        members,
    })
}

#[cfg(test)]
fn replay(
    path: &Path,
    profile: &str,
    server_key: &str,
) -> Result<Vec<GlobalMembershipRecord>, MachineError> {
    replay_tail(path, profile, server_key, 0, ZERO_HASH)
}

fn verify_membership_record(
    record: &GlobalMembershipRecord,
    profile: &str,
    server_key: &str,
    expected_revision: u64,
    previous: &str,
) -> Result<(), MachineError> {
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
        || record.revision != expected_revision
        || record.previous_sha256 != previous
        || record.record_sha256 != canonical_value_digest(&body)?
    {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership hash chain breaks",
        ));
    }
    Ok(())
}

fn validate_membership_prefix(
    bytes: &[u8],
    expected_revision: u64,
    previous: &str,
) -> Result<(), serde_json::Error> {
    struct Prefix<'a> {
        revision: u64,
        previous: &'a str,
    }
    impl<'de> serde::de::Visitor<'de> for Prefix<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a checked membership record prefix")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
            let mut fields = std::collections::BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !fields.insert(key.clone()) {
                    return Err(serde::de::Error::custom("duplicate membership field"));
                }
                match key.as_str() {
                    "schema_version" => {
                        if map.next_value::<u32>()? != 2 {
                            return Err(serde::de::Error::custom("unsupported membership version"));
                        }
                    }
                    "revision" => {
                        if map.next_value::<u64>()? != self.revision {
                            return Err(serde::de::Error::custom("membership revision mismatch"));
                        }
                    }
                    "previous_sha256" => {
                        if map.next_value::<String>()? != self.previous {
                            return Err(serde::de::Error::custom("membership chain mismatch"));
                        }
                    }
                    "record_sha256" => {
                        let value = map.next_value::<String>()?;
                        if value.len() != 64
                            || !value
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        {
                            return Err(serde::de::Error::custom("invalid membership record hash"));
                        }
                    }
                    _ => {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
            }
            Ok(())
        }
    }
    let mut de = serde_json::Deserializer::from_slice(bytes);
    serde::Deserializer::deserialize_map(
        &mut de,
        Prefix {
            revision: expected_revision,
            previous,
        },
    )?;
    de.end()
}

#[cfg(test)]
fn replay_tail(
    path: &Path,
    profile: &str,
    server_key: &str,
    base_revision: u64,
    base_hash: &str,
) -> Result<Vec<GlobalMembershipRecord>, MachineError> {
    replay_tail_bounded(
        path,
        profile,
        server_key,
        base_revision,
        base_hash,
        MAX_MEMBERSHIP_BYTES,
    )
}
fn replay_tail_bounded(
    path: &Path,
    profile: &str,
    server_key: &str,
    base_revision: u64,
    base_hash: &str,
    maximum: u64,
) -> Result<Vec<GlobalMembershipRecord>, MachineError> {
    let Some(file) = open_secure_file_if_present(path, DarwinSystem.current_uid())? else {
        return Ok(Vec::new());
    };
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| path_error(path, error))?;
    let over_capacity = bytes.len() as u64 > maximum;
    if over_capacity {
        let boundary = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |n| n + 1);
        let partial = &bytes[boundary..];
        if !partial.is_empty() {
            // Header checks use the last complete record in this bounded prefix.
            let complete = &bytes[..boundary];
            let mut revision = base_revision;
            let mut previous = base_hash.to_owned();
            for line in complete
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                let record: GlobalMembershipRecord =
                    serde_json::from_slice(line).map_err(|_| {
                        membership_incomplete(
                            profile,
                            server_key,
                            "oversized membership journal has malformed input",
                        )
                    })?;
                revision = record.revision;
                previous = record.record_sha256;
            }
            let expected = revision.checked_add(1).ok_or_else(|| {
                membership_incomplete(profile, server_key, "membership revision overflow")
            })?;
            let parsed = serde_json::from_slice::<GlobalMembershipRecord>(partial);
            if let Ok(record) = &parsed {
                verify_membership_record(record, profile, server_key, expected, &previous)?;
            }
            let headers = validate_membership_prefix(partial, expected, &previous);
            let acceptable = |result: &Result<(), serde_json::Error>| {
                result.is_ok() || result.as_ref().is_err_and(|error| error.is_eof())
            };
            if !(parsed.is_ok() || parsed.as_ref().is_err_and(|error| error.is_eof()))
                || !acceptable(&headers)
                || partial
                    .iter()
                    .copied()
                    .find(|byte| !byte.is_ascii_whitespace())
                    != Some(b'{')
            {
                return Err(membership_incomplete(
                    profile,
                    server_key,
                    "oversized membership journal has malformed input",
                ));
            }
        }
        bytes.truncate(boundary);
    } else if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(membership_incomplete(
            profile,
            server_key,
            "membership journal has an unterminated final record",
        ));
    }
    let mut records = Vec::new();
    let mut previous = base_hash.to_owned();
    for (offset, line) in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let record: GlobalMembershipRecord = serde_json::from_slice(line).map_err(|_| {
            membership_incomplete(
                profile,
                server_key,
                format!("membership line {} is invalid", offset + 1),
            )
        })?;
        let expected = base_revision
            .checked_add(u64::try_from(offset + 1).expect("bounded"))
            .ok_or_else(|| {
                membership_incomplete(profile, server_key, "membership revision overflow")
            })?;
        verify_membership_record(&record, profile, server_key, expected, &previous)?;
        previous = record.record_sha256.clone();
        records.push(record);
    }
    if over_capacity {
        return Err(membership_capacity(
            profile,
            server_key,
            "membership journal exceeds 8 MiB",
        ));
    }
    Ok(records)
}

fn append_record(path: &Path, record: &GlobalMembershipRecord) -> Result<(), MachineError> {
    let mut line = serde_json::to_vec(record).map_err(internal)?;
    line.push(b'\n');
    let current_bytes = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(path_error(path, error)),
    };
    if current_bytes.saturating_add(line.len() as u64) > MAX_MEMBERSHIP_BYTES {
        return Err(membership_capacity(
            "",
            "",
            "membership append exceeds 8 MiB",
        ));
    }
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
    verify_lock_file(&file, path, DarwinSystem.current_uid())?;
    DarwinSystem
        .lock_exclusive(&file)
        .map_err(|error| path_error(path, error))?;
    Ok(file)
}

fn verify_lock_file(file: &File, path: &Path, uid: u32) -> Result<(), MachineError> {
    let metadata = file.metadata().map_err(|error| path_error(path, error))?;
    if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o600 {
        return Err(MachineError::runtime_path_invalid(
            path,
            "lock file must be current-uid-owned regular file with mode 0600",
        ));
    }
    Ok(())
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

fn membership_capacity(profile: &str, server_key: &str, reason: &str) -> MachineError {
    MachineError::new(
        "STORAGE_CAPACITY_EXCEEDED",
        "Profile journal capacity is exhausted",
        false,
        json!({"profile":profile,"server_key":server_key,"reason":reason,"required_action":"observe_original_maintenance"}),
    )
}

fn membership_incomplete(
    profile: &str,
    server_key: &str,
    reason: impl Into<String>,
) -> MachineError {
    MachineError::new(
        "PROFILE_MEMBERSHIP_INCOMPLETE",
        "global Profile membership cannot be proven",
        false,
        json!({"profile": profile, "server_key": server_key, "reason": reason.into()}),
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

    #[test]
    fn cleanup_display_is_noncreating_nonblocking_and_rechecks_sources() {
        let (parent, home) = home();
        let key = "8".repeat(64);
        let workspace = "7".repeat(64);
        let run_id = Uuid::now_v7();
        let store = GlobalMembershipStore::new(&home, "default", &key).unwrap();
        let root = home.root().join("profiles").join(&key);
        assert!(store.released_for_display(&workspace, run_id).is_err());
        assert!(!root.exists());
        store
            .record(&workspace, run_id, MembershipDisposition::Released)
            .unwrap();
        assert!(store.released_for_display(&workspace, run_id).unwrap());
        let home_lock = File::open(home.root()).unwrap();
        DarwinSystem.lock_exclusive(&home_lock).unwrap();
        assert!(store.released_for_display(&workspace, run_id).unwrap());
        let server_lock = lock_file(&root.join("server.lock")).unwrap();
        let started = std::time::Instant::now();
        assert!(store.released_for_display(&workspace, run_id).is_err());
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        drop(server_lock);
        drop(home_lock);
        let original = store.load().unwrap();
        let _server = lock_file(&root.join("server.lock")).unwrap();
        store
            .compact_under_server_lock(
                &root,
                Uuid::now_v7(),
                1,
                &original.journal_sha256,
                &mut |_| Ok(()),
            )
            .unwrap();
        drop(_server);
        assert!(store.released_for_display(&workspace, run_id).unwrap());
        let index = root.join("members.json");
        let bytes = fs::read(&index).unwrap();
        fs::write(&index, b"{invalid}").unwrap();
        assert!(store.released_for_display(&workspace, run_id).is_err());
        fs::write(index, bytes).unwrap();
        assert!(store.released_for_display(&workspace, run_id).unwrap());
        fs::remove_dir_all(parent).unwrap();
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
            codex_version: "0.157.0".to_owned(),
            schema_bundle_sha256: "2".repeat(64),
            compatibility_manifest_sha256: "3".repeat(64),
            launch_contract_sha256: "4".repeat(64),
            compatibility_verdict: CompatibilityVerdict::Tested,
            server_key: key.to_owned(),
        }
    }

    fn server_state(snapshot: ProfileSnapshot) -> ServerState {
        ServerState {
            schema_version: 2,
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
    fn new_admission_uses_a_minimum_while_retained_execution_preserves_old_bindings() {
        let (parent, _) = home();
        let definition = definition(&parent);
        let old = "4d210f7c5a18fd0386434df23b5bdbb8c0e7257d3e8a2b30b0769c8bbe99a878";
        let previous = "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf";
        let current = CURRENT_CODEX_SHA256;
        for version in [
            "0.158.0",
            "0.159.3",
            "0.164.0",
            "0.167.2",
            "0.168.9",
            CURRENT_CODEX_VERSION,
            "0.170.0",
            "1.0.0",
            "not-a-version",
        ] {
            for sha in [
                old,
                previous,
                current,
                "97bc82fd46f117cf10efb95e6ff0c81dc4784f5797bcdc2cb75093ba09e4def0",
                &"b".repeat(64),
            ] {
                let mut snapshot = snapshot("reviewer", &definition, &"a".repeat(64));
                snapshot.codex_version = version.to_owned();
                snapshot.executable_identity.sha256 = sha.to_owned();
                snapshot.compatibility_verdict = CompatibilityVerdict::Unverified;
                let binding =
                    ResolvedGlobalProfile::from_definition("reviewer", definition.clone())
                        .unwrap()
                        .bind(snapshot)
                        .unwrap();
                let review: crate::direct_review::ReviewRequestV4 = serde_json::from_value(json!({
                    "schema":"dolgorae-specialist-review-request/v4","operation":"review_target",
                    "target":{"kind":"head","revision":null},"purpose":"completion","brief":"Check the accepted candidate",
                    "contexts":[{"id":"context","content":"Accepted requirement","provenance":"caller"}],
                    "criteria":[{"id":"criterion","statement":"Candidate meets the requirement","source_context_ids":["context"]}],
                    "expected_output":"structured_review_v4","deadline_seconds":60,"role":null
                })).unwrap();
                let admission = false; // Generic construction is never qualified admission.
                let eligible = creation_version_supported(version);
                let accepted = (version == "0.167.2"
                    && sha == "97bc82fd46f117cf10efb95e6ff0c81dc4784f5797bcdc2cb75093ba09e4def0")
                    || (version == "0.164.0" && sha == previous)
                    || (version == "0.159.3" && sha == old);
                assert_eq!(
                    binding.caller_workspace_supported(),
                    admission,
                    "{version}/{sha}"
                );
                assert!(!binding.uses_qualified_launch_policy());
                assert_eq!(review.admit_profile(&binding).is_ok(), admission);
                if !admission {
                    assert_eq!(
                        review.admit_profile(&binding).unwrap_err().code,
                        "REVIEW_READING_BOUNDARY_UNSUPPORTED"
                    );
                }
                assert_eq!(
                    binding.checked_review_supported(),
                    admission,
                    "{version}/{sha}"
                );
                assert_eq!(
                    binding.accepted_caller_workspace_supported(),
                    accepted,
                    "{version}/{sha}"
                );
                assert_eq!(
                    binding.accepted_checked_review_supported(),
                    accepted,
                    "{version}/{sha}"
                );
                assert_eq!(
                    crate::instructions::require_discovery_control(&binding).is_ok(),
                    admission
                );
                assert_eq!(
                    crate::instructions::require_accepted_discovery_control(&binding).is_ok(),
                    accepted
                );
                if accepted && !admission {
                    assert_eq!(
                        crate::instructions::require_discovery_control(&binding)
                            .unwrap_err()
                            .code,
                        "INSTRUCTION_DISCOVERY_UNSUPPORTED"
                    );
                }
                let qualified = crate::profile::qualified_test_binding(&binding);
                assert!(qualified.uses_qualified_launch_policy());
                assert_eq!(
                    qualified.caller_workspace_supported(),
                    eligible,
                    "{version}/{sha}"
                );
                assert_eq!(review.admit_profile(&qualified).is_ok(), eligible);
                assert_eq!(
                    qualified.accepted_caller_workspace_supported(),
                    eligible || accepted
                );
                assert_eq!(
                    qualified.launch_snapshot.compatibility_verdict,
                    CompatibilityVerdict::Unverified
                );
                let mut changed_identity = qualified.clone();
                changed_identity.launch_snapshot.executable_identity.sha256 = "c".repeat(64);
                assert!(!changed_identity.caller_workspace_supported());
                let mut rejected = qualified.clone();
                rejected.launch_snapshot.compatibility_verdict = CompatibilityVerdict::Rejected;
                rejected.launch_snapshot_sha256 =
                    canonical_digest(&rejected.launch_snapshot).unwrap();
                assert!(!rejected.caller_workspace_supported());
                assert_ne!(qualified.server_key, binding.server_key);
                assert_eq!(
                    binding.launch_snapshot.disabled_features,
                    Vec::<String>::new()
                );
                let mut missing = qualified.clone();
                missing.launch_snapshot.disabled_features.clear();
                missing.launch_snapshot_sha256 =
                    canonical_digest(&missing.launch_snapshot).unwrap();
                assert!(!missing.caller_workspace_supported());
                let mut overridden = qualified;
                overridden
                    .launch_snapshot
                    .normalized_argv
                    .extend(["--enable".into(), "shell_snapshot".into()]);
                overridden.launch_snapshot_sha256 =
                    canonical_digest(&overridden.launch_snapshot).unwrap();
                assert!(!overridden.caller_workspace_supported());
                let mut damaged = binding;
                damaged.launch_snapshot_sha256 = "0".repeat(64);
                assert!(!damaged.accepted_caller_workspace_supported());
            }
        }
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn direct_review_qualification_rejects_unexercised_tool_surfaces_and_pin_drift() {
        let (parent, _) = home();
        let definition = definition(&parent);
        let mut snapshot = snapshot("reviewer", &definition, &"a".repeat(64));
        snapshot.codex_version = "0.159.3".to_owned();
        snapshot.executable_identity.sha256 =
            "4d210f7c5a18fd0386434df23b5bdbb8c0e7257d3e8a2b30b0769c8bbe99a878".to_owned();
        let bind = |snapshot| {
            ResolvedGlobalProfile::from_definition("reviewer", definition.clone())
                .unwrap()
                .bind(snapshot)
                .unwrap()
        };
        let binding = bind(snapshot.clone());
        assert!(binding.accepted_checked_review_supported());
        for name in [
            "mcp_servers",
            "apps",
            "agents",
            "hooks",
            "notify",
            "plugins",
            "marketplaces",
            "skills",
        ] {
            let mut changed = snapshot.clone();
            changed
                .process_static_configuration
                .insert(name.to_owned(), json!({"enabled":true}));
            assert!(!bind(changed).accepted_checked_review_supported(), "{name}");
        }
        let mut changed = snapshot.clone();
        changed.enabled_features.push("apps".to_owned());
        assert!(!bind(changed).accepted_checked_review_supported());
        let mut changed = snapshot;
        changed.executable_identity.sha256 = "b".repeat(64);
        assert!(!bind(changed).accepted_checked_review_supported());
        let mut changed = binding;
        changed.launch_snapshot.codex_version = "0.159.4".to_owned();
        assert!(!changed.accepted_checked_review_supported());
        fs::remove_dir_all(parent).unwrap();
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
        let state = server_state(first.launch_snapshot.clone());
        let first_discovery = state.discover_global(&first).unwrap();
        let second_discovery = state.discover_global(&second).unwrap();
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
        let state = server_state(binding.launch_snapshot.clone());
        let diagnostic = state.global_diagnostic(&binding).unwrap();
        store.remove_if("selected", |_| Ok(())).unwrap();
        binding.validate_for_recovery().unwrap();
        state.validate_for_global_restart().unwrap();
        assert_eq!(state.global_diagnostic(&binding).unwrap(), diagnostic);
        assert_eq!(diagnostic["profile"], "selected");
        assert_eq!(diagnostic["server_key"], binding.server_key);

        let mut other_generation = state.clone();
        other_generation.server_key = "c".repeat(64);
        other_generation.snapshot.server_key = other_generation.server_key.clone();
        other_generation.validate_for_global_restart().unwrap();
        assert_eq!(
            other_generation.discover_global(&binding).unwrap_err().code,
            "RUN_MANIFEST_INVALID"
        );
        let mut other_snapshot = state;
        other_snapshot
            .snapshot
            .normalized_argv
            .push("--strict-config".to_owned());
        assert_eq!(
            other_snapshot.discover_global(&binding).unwrap_err().code,
            "RUN_MANIFEST_INVALID"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn runtime_trust_observation_does_not_change_compatible_generation() {
        let (parent, home) = home();
        let profile = definition(&parent);
        GlobalProfileStore::new(&home)
            .add("selected".to_owned(), profile.clone())
            .unwrap();
        let resolved = ResolvedGlobalProfile::resolve(&home, "selected").unwrap();
        let original = snapshot("selected", &profile, &"b".repeat(64));
        let mut observed = original.clone();
        observed.initial_configuration_observation.insert(
            "projects".to_owned(),
            json!({"/synthetic/project": {"trust_level": "trusted"}}),
        );
        let original_binding = resolved.clone().bind(original.clone()).unwrap();
        let observed_binding = resolved.bind(observed.clone()).unwrap();
        assert_ne!(
            original_binding.launch_snapshot_sha256,
            observed_binding.launch_snapshot_sha256
        );
        for (state, binding) in [
            (server_state(original), &observed_binding),
            (server_state(observed), &original_binding),
        ] {
            assert_eq!(state.discover_global(binding).unwrap().server_epoch, 7);
            let mut incompatible = state;
            incompatible.snapshot.process_static_configuration.insert(
                "openai_base_url".to_owned(),
                json!("https://incompatible.invalid"),
            );
            assert_eq!(
                incompatible.discover_global(binding).unwrap_err().code,
                "RUN_MANIFEST_INVALID"
            );
        }
        let mut corrupted = observed_binding;
        corrupted
            .launch_snapshot
            .initial_configuration_observation
            .clear();
        assert_eq!(
            corrupted.validate_for_recovery().unwrap_err().code,
            "RUN_MANIFEST_INVALID"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn removal_uses_persisted_binding_without_reopening_the_executable() {
        let (parent, home) = home();
        let profile = definition(&parent);
        let executable = PathBuf::from(&profile.argv[0]);
        let store = GlobalProfileStore::new(&home);
        store.add("offline".to_owned(), profile).unwrap();
        let resolved = ResolvedGlobalProfile::resolve(&home, "offline").unwrap();
        store
            .record_binding(crate::global_profile::ProfileBindingRecord {
                selected_name: "offline".to_owned(),
                definition_sha256: resolved.definition_sha256,
                server_key: "b".repeat(64),
                launch_snapshot_sha256: "c".repeat(64),
            })
            .unwrap();
        let server_root = home.root().join("profiles").join("b".repeat(64));
        assert!(!server_root.exists());
        fs::remove_file(executable).unwrap();
        remove_global_profile(&home, "offline").unwrap();
        assert!(store.load().unwrap().profiles.is_empty());
        assert!(!server_root.exists());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn dangling_membership_authorities_reject_load_append_and_quiescence() {
        for name in ["membership.jsonl", "members.json", "state.json"] {
            let (parent, home) = home();
            let server_key = "c".repeat(64);
            let store = GlobalMembershipStore::new(&home, "default", &server_key).unwrap();
            store.load().unwrap();
            let root = home.root().join("profiles").join(&server_key);
            let path = root.join(name);
            let target = root.join("absent");
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert_eq!(
                store.load().unwrap_err().code,
                "RUNTIME_PATH_INVALID",
                "{name}"
            );
            assert_eq!(
                store
                    .record(
                        &"1".repeat(64),
                        Uuid::now_v7(),
                        MembershipDisposition::Active
                    )
                    .unwrap_err()
                    .code,
                "RUNTIME_PATH_INVALID",
                "{name}"
            );
            assert_eq!(
                store
                    .acquire_quiescence("default", "profile.remove")
                    .err()
                    .unwrap()
                    .code,
                "RUNTIME_PATH_INVALID",
                "{name}"
            );
            assert!(
                fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(!target.exists());
            for other in ["membership.jsonl", "members.json", "state.json"] {
                if other != name {
                    assert!(!root.join(other).exists());
                }
            }
            fs::remove_dir_all(parent).unwrap();
        }
    }

    #[test]
    fn membership_read_limit_preserves_replay_hashes_and_rejects_before_append() {
        let (parent, home) = home();
        let server_key = "c".repeat(64);
        let workspace_id = "1".repeat(64);
        let run_id = Uuid::now_v7();
        let store = GlobalMembershipStore::new(&home, "default", &server_key).unwrap();
        store
            .record(&workspace_id, run_id, MembershipDisposition::Active)
            .unwrap();
        let root = home.root().join("profiles").join(&server_key);
        let journal = root.join("membership.jsonl");
        let original = fs::read(&journal).unwrap();
        let index_before = fs::read(root.join("members.json")).unwrap();
        for length in [MAX_MEMBERSHIP_BYTES - 1, MAX_MEMBERSHIP_BYTES] {
            let mut bytes = original.clone();
            bytes.resize(usize::try_from(length).unwrap(), b'\n');
            fs::write(&journal, &bytes).unwrap();
            let records = replay(&journal, "default", &server_key).unwrap();
            assert_eq!(records.len(), 1);
            let index = derive_index(&server_key, &journal, &records).unwrap();
            assert_eq!(index.journal_sha256, sha256_hex(&bytes));
            assert_eq!(index.revision, 1);
            assert_eq!(index.members.len(), 1);
            assert_eq!(fs::read(&journal).unwrap(), bytes);
        }
        let mut oversized = original;
        oversized.resize(usize::try_from(MAX_MEMBERSHIP_BYTES).unwrap() + 1, b'\n');
        fs::write(&journal, &oversized).unwrap();
        let error = store
            .record(&workspace_id, run_id, MembershipDisposition::Released)
            .unwrap_err();
        assert_eq!(error.code, "STORAGE_CAPACITY_EXCEEDED");
        assert_eq!(error.details["reason"], "membership journal exceeds 8 MiB");
        assert_eq!(fs::read(&journal).unwrap(), oversized);
        assert_eq!(fs::read(root.join("members.json")).unwrap(), index_before);
        // Index derivation must hash every byte, including an append crossing
        // the replay ceiling; it must never publish a truncated-prefix hash.
        assert_eq!(
            derive_index(&server_key, &journal, &[])
                .unwrap()
                .journal_sha256,
            sha256_hex(&oversized)
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn unterminated_membership_rejects_reads_and_appends_without_mutation() {
        for partial_tail in [false, true] {
            for missing_index in [false, true] {
                let (parent, home) = home();
                let key = "c".repeat(64);
                let store = GlobalMembershipStore::new(&home, "selected", &key).unwrap();
                assert_eq!(store.load().unwrap().revision, 0);
                let root = home.root().join("profiles").join(&key);
                let state = server_state(snapshot("selected", &definition(&parent), &key));
                atomic_create(
                    &SystemWorkspacePlatform,
                    &root.join("state.json"),
                    &serde_json::to_vec(&state).unwrap(),
                    0o600,
                )
                .unwrap();
                let run_id = Uuid::now_v7();
                store
                    .record(&"1".repeat(64), run_id, MembershipDisposition::Active)
                    .unwrap();
                assert_eq!(store.load().unwrap().revision, 1);
                let paths = [
                    root.join("membership.jsonl"),
                    root.join("members.json"),
                    root.join("state.json"),
                ];
                let mut bytes = fs::read(&paths[0]).unwrap();
                assert_eq!(bytes.last(), Some(&b'\n'));
                if partial_tail {
                    bytes.extend_from_slice(b"{\"schema_version\":");
                } else {
                    bytes.pop();
                }
                fs::write(&paths[0], bytes).unwrap();
                if missing_index {
                    fs::remove_file(&paths[1]).unwrap();
                }
                let before = paths.each_ref().map(|path| fs::read(path).ok());
                let error = store.load().unwrap_err();
                assert_eq!(error.code, "PROFILE_MEMBERSHIP_INCOMPLETE");
                assert_eq!(
                    error.details["reason"],
                    "membership journal has an unterminated final record"
                );
                assert_eq!(paths.each_ref().map(|path| fs::read(path).ok()), before);
                let error = store
                    .record(&"1".repeat(64), run_id, MembershipDisposition::Released)
                    .unwrap_err();
                assert_eq!(error.code, "PROFILE_MEMBERSHIP_INCOMPLETE");
                assert_eq!(
                    error.details["reason"],
                    "membership journal has an unterminated final record"
                );
                assert_eq!(paths.each_ref().map(|path| fs::read(path).ok()), before);
                fs::remove_dir_all(parent).unwrap();
            }
        }
    }

    #[test]
    fn membership_spans_workspaces_profiles_and_blocks_unknown_outcomes() {
        let (parent, home) = home();
        let first = GlobalMembershipStore::new(&home, "default", &"c".repeat(64)).unwrap();
        let second = GlobalMembershipStore::new(&home, "default", &"d".repeat(64)).unwrap();
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
            let error = first.require_quiescent("one", operation).unwrap_err();
            assert_eq!(error.code, "PROFILE_SERVER_BUSY");
            assert!(error.retryable);
            assert_eq!(error.details["profile"], "one");
            assert_eq!(error.details["server_key"], "c".repeat(64));
            assert!(
                error.details["reason"]
                    .as_str()
                    .unwrap()
                    .contains("2 live run member(s)")
            );
        }
        first
            .record(&"1".repeat(64), run_a, MembershipDisposition::Released)
            .unwrap();
        first
            .record(&"2".repeat(64), run_b, MembershipDisposition::Released)
            .unwrap();
        first.require_quiescent("one", "generation_change").unwrap();
        assert!(second.require_quiescent("two", "stop").is_err());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn proven_server_absence_releases_old_epoch_without_inventing_a_run_result() {
        let (parent, home) = home();
        let store = GlobalMembershipStore::new(&home, "default", &"7".repeat(64)).unwrap();
        let run_id = Uuid::now_v7();
        let stale_run_id = Uuid::now_v7();
        store
            .record_observed(
                &"1".repeat(64),
                run_id,
                MembershipDisposition::Active,
                GlobalMembershipFacts {
                    controller_id: Some(Uuid::now_v7()),
                    worker_generation: Some(3),
                    thread_id: Some("thread".to_owned()),
                    connection_id: Some(Uuid::now_v7()),
                    lifecycle: "running".to_owned(),
                    writer: true,
                    observed_epoch: Some(7),
                    runtime_locator: Some("/tmp/run".to_owned()),
                },
            )
            .unwrap();
        store
            .record_observed(
                &"2".repeat(64),
                stale_run_id,
                MembershipDisposition::Active,
                GlobalMembershipFacts {
                    controller_id: None,
                    worker_generation: Some(2),
                    thread_id: None,
                    connection_id: None,
                    lifecycle: "idle".to_owned(),
                    writer: false,
                    observed_epoch: Some(6),
                    runtime_locator: Some("/tmp/stale-run".to_owned()),
                },
            )
            .unwrap();
        let root = home.root().join("profiles").join("7".repeat(64));
        let released = store
            .release_after_server_absence_under_lifecycle_locks(&root, 7)
            .unwrap();
        let member = released
            .members
            .get(&format!("{}:{run_id}", "1".repeat(64)))
            .unwrap();
        assert_eq!(member.disposition, MembershipDisposition::Released);
        assert_eq!(member.lifecycle, "interrupted_unknown");
        assert_eq!(member.observed_epoch, Some(7));
        let stale = released
            .members
            .get(&format!("{}:{stale_run_id}", "2".repeat(64)))
            .unwrap();
        assert_eq!(stale.disposition, MembershipDisposition::Released);
        assert_eq!(stale.lifecycle, "stale_generation_reconciled");
        assert_eq!(stale.observed_epoch, Some(6));
        store.require_quiescent("default", "restart").unwrap();
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn reattachment_replaces_the_observed_epoch_before_a_later_absence() {
        let (parent, home) = home();
        let store = GlobalMembershipStore::new(&home, "default", &"6".repeat(64)).unwrap();
        let workspace_id = "1".repeat(64);
        let run_id = Uuid::now_v7();
        store
            .record_observed(
                &workspace_id,
                run_id,
                MembershipDisposition::Active,
                GlobalMembershipFacts {
                    controller_id: None,
                    worker_generation: Some(1),
                    thread_id: Some("thread".to_owned()),
                    connection_id: None,
                    lifecycle: "running".to_owned(),
                    writer: false,
                    observed_epoch: Some(6),
                    runtime_locator: Some("/tmp/run".to_owned()),
                },
            )
            .unwrap();
        let root = home.root().join("profiles").join("6".repeat(64));
        store
            .release_after_server_absence_under_lifecycle_locks(&root, 6)
            .unwrap();
        store
            .record_observed(
                &workspace_id,
                run_id,
                MembershipDisposition::Active,
                GlobalMembershipFacts {
                    controller_id: None,
                    worker_generation: Some(2),
                    thread_id: None,
                    connection_id: None,
                    lifecycle: "run_reattached".to_owned(),
                    writer: false,
                    observed_epoch: Some(7),
                    runtime_locator: None,
                },
            )
            .unwrap();
        let released = store
            .release_after_server_absence_under_lifecycle_locks(&root, 7)
            .unwrap();
        let member = released
            .members
            .get(&format!("{workspace_id}:{run_id}"))
            .unwrap();
        assert_eq!(member.observed_epoch, Some(7));
        assert_eq!(member.lifecycle, "interrupted_unknown");
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn concurrent_membership_updates_form_one_complete_chain() {
        let (parent, home) = home();
        let store =
            Arc::new(GlobalMembershipStore::new(&home, "default", &"e".repeat(64)).unwrap());
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
    fn insecure_server_locks_block_reads_removal_and_append_without_mutation() {
        for corruption in ["permissions", "directory", "symlink"] {
            let (parent, home) = home();
            let key = "d".repeat(64);
            let profile = definition(&parent);
            let registry = GlobalProfileStore::new(&home);
            registry
                .add("selected".to_owned(), profile.clone())
                .unwrap();
            let resolved = ResolvedGlobalProfile::resolve(&home, "selected").unwrap();
            registry
                .record_binding(crate::global_profile::ProfileBindingRecord {
                    selected_name: "selected".to_owned(),
                    definition_sha256: resolved.definition_sha256,
                    server_key: key.clone(),
                    launch_snapshot_sha256: "b".repeat(64),
                })
                .unwrap();
            let store = GlobalMembershipStore::new(&home, "selected", &key).unwrap();
            let run_id = Uuid::now_v7();
            store
                .record(&"1".repeat(64), run_id, MembershipDisposition::Released)
                .unwrap();
            let root = home.root().join("profiles").join(&key);
            let path = root.join("server.lock");
            match corruption {
                "permissions" => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap()
                }
                "directory" => {
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                }
                "symlink" => {
                    fs::remove_file(&path).unwrap();
                    std::os::unix::fs::symlink(root.join("members.json"), &path).unwrap();
                }
                _ => unreachable!(),
            }
            let paths = [
                root.join("membership.jsonl"),
                root.join("members.json"),
                home.root().join("profiles.yaml"),
                home.root().join("profile-bindings.json"),
            ];
            let before = paths.each_ref().map(|path| fs::read(path).unwrap());
            let metadata = fs::symlink_metadata(&path).unwrap();
            for error in [
                store.load().unwrap_err(),
                store.require_quiescent("selected", "stop").unwrap_err(),
                remove_global_profile(&home, "selected").unwrap_err(),
                store
                    .record(&"1".repeat(64), run_id, MembershipDisposition::Active)
                    .unwrap_err(),
            ] {
                assert_eq!(error.code, "RUNTIME_PATH_INVALID", "{corruption}");
            }
            assert_eq!(paths.each_ref().map(|path| fs::read(path).unwrap()), before);
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!(
                (after.ino(), after.mode(), after.uid()),
                (metadata.ino(), metadata.mode(), metadata.uid())
            );
            fs::remove_dir_all(parent).unwrap();
        }
    }

    #[test]
    fn lock_descriptor_validation_rejects_foreign_ownership_and_non_files() {
        let (parent, home) = home();
        let path = home.root().join("test.lock");
        let file = lock_file(&path).unwrap();
        let uid = DarwinSystem.current_uid();
        assert_eq!(
            verify_lock_file(&file, &path, uid.wrapping_add(1))
                .unwrap_err()
                .code,
            "RUNTIME_PATH_INVALID"
        );
        let directory = File::open(home.root()).unwrap();
        assert_eq!(
            verify_lock_file(&directory, home.root(), uid)
                .unwrap_err()
                .code,
            "RUNTIME_PATH_INVALID"
        );
        drop(file);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn inconsistent_server_evidence_blocks_reads_removal_and_append_without_mutation() {
        for corruption in [
            "missing_history",
            "ahead",
            "behind",
            "wrong_key",
            "wrong_snapshot",
            "legacy",
            "malformed",
            "index",
        ] {
            let (parent, home) = home();
            let key = "d".repeat(64);
            let profile = definition(&parent);
            let registry = GlobalProfileStore::new(&home);
            registry
                .add("selected".to_owned(), profile.clone())
                .unwrap();
            let resolved = ResolvedGlobalProfile::resolve(&home, "selected").unwrap();
            registry
                .record_binding(crate::global_profile::ProfileBindingRecord {
                    selected_name: "selected".to_owned(),
                    definition_sha256: resolved.definition_sha256,
                    server_key: key.clone(),
                    launch_snapshot_sha256: "b".repeat(64),
                })
                .unwrap();
            let store = GlobalMembershipStore::new(&home, "selected", &key).unwrap();
            let run_id = Uuid::now_v7();
            store
                .record(&"1".repeat(64), run_id, MembershipDisposition::Released)
                .unwrap();
            let root = home.root().join("profiles").join(&key);
            let mut state = server_state(snapshot("selected", &profile, &key));
            state.membership_revision = 1;
            match corruption {
                "missing_history" => {
                    fs::remove_file(root.join("membership.jsonl")).unwrap();
                    fs::remove_file(root.join("members.json")).unwrap();
                }
                "ahead" => state.membership_revision = 2,
                "behind" => state.membership_revision = 0,
                "wrong_key" => state.server_key = "e".repeat(64),
                "wrong_snapshot" => state.snapshot.server_key = "e".repeat(64),
                "legacy" => state.schema_version = 1,
                "index" => fs::write(root.join("members.json"), b"{}").unwrap(),
                "malformed" => {}
                _ => unreachable!(),
            }
            let state_bytes = if corruption == "malformed" {
                b"{}".to_vec()
            } else {
                serde_json::to_vec_pretty(&state).unwrap()
            };
            atomic_create(
                &SystemWorkspacePlatform,
                &root.join("state.json"),
                &state_bytes,
                0o600,
            )
            .unwrap();
            let paths = [
                root.join("membership.jsonl"),
                root.join("members.json"),
                root.join("state.json"),
                home.root().join("profiles.yaml"),
                home.root().join("profile-bindings.json"),
            ];
            let before = paths.each_ref().map(|path| fs::read(path).ok());
            for error in [
                store.load().unwrap_err(),
                store.require_quiescent("selected", "stop").unwrap_err(),
                store
                    .require_quiescent_under_server_lock("selected", "restart")
                    .unwrap_err(),
                remove_global_profile(&home, "selected").unwrap_err(),
                store
                    .record(&"1".repeat(64), run_id, MembershipDisposition::Active)
                    .unwrap_err(),
            ] {
                assert_eq!(error.code, "PROFILE_MEMBERSHIP_INCOMPLETE", "{corruption}");
            }
            assert_eq!(
                paths.each_ref().map(|path| fs::read(path).ok()),
                before,
                "{corruption}"
            );
            fs::remove_dir_all(parent).unwrap();
        }
    }

    #[test]
    fn pristine_membership_and_a_missing_derived_index_preserve_journal_authority() {
        let (parent, home) = home();
        let key = "c".repeat(64);
        let store = GlobalMembershipStore::new(&home, "selected", &key).unwrap();
        assert_eq!(store.load().unwrap().revision, 0);
        let root = home.root().join("profiles").join(&key);
        let state = server_state(snapshot("selected", &definition(&parent), &key));
        atomic_create(
            &SystemWorkspacePlatform,
            &root.join("state.json"),
            &serde_json::to_vec(&state).unwrap(),
            0o600,
        )
        .unwrap();
        let run_id = Uuid::now_v7();
        store
            .record(&"1".repeat(64), run_id, MembershipDisposition::Active)
            .unwrap();
        fs::remove_file(root.join("members.json")).unwrap();
        assert_eq!(store.load().unwrap().revision, 1);
        assert_eq!(
            store
                .require_quiescent("selected", "stop")
                .unwrap_err()
                .code,
            "PROFILE_SERVER_BUSY"
        );
        let index = store
            .record(&"1".repeat(64), run_id, MembershipDisposition::Released)
            .unwrap();
        assert_eq!(index.revision, 2);
        let state: ServerState =
            serde_json::from_slice(&fs::read(root.join("state.json")).unwrap()).unwrap();
        assert_eq!(state.membership_revision, 2);
        store.require_quiescent("selected", "stop").unwrap();
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn corrupt_or_legacy_membership_never_becomes_quiescent() {
        let (parent, home) = home();
        let store = GlobalMembershipStore::new(&home, "default", &"f".repeat(64)).unwrap();
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
        let error = store.require_quiescent("default", "restart").unwrap_err();
        assert_eq!(error.code, "PROFILE_MEMBERSHIP_INCOMPLETE");
        assert_eq!(error.details["profile"], "default");
        assert_eq!(error.details["server_key"], "f".repeat(64));
        assert!(error.details["reason"].is_string());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn quiescence_rejects_a_persisted_index_that_disagrees_with_its_journal() {
        let (parent, home) = home();
        let store = GlobalMembershipStore::new(&home, "default", &"9".repeat(64)).unwrap();
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
        assert_eq!(error.details["profile"], "default");
        assert_eq!(error.details["server_key"], "9".repeat(64));
        assert!(error.details["reason"].is_string());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn quiescence_guard_serializes_admission_with_generation_change() {
        let (parent, home) = home();
        let store =
            Arc::new(GlobalMembershipStore::new(&home, "default", &"8".repeat(64)).unwrap());
        let guard = store
            .acquire_quiescence("default", "generation_change")
            .unwrap();
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

    #[test]
    fn membership_compaction_recovers_every_durable_boundary() {
        for fault in [
            MembershipSwitchBarrier::PendingSynced,
            MembershipSwitchBarrier::CheckpointSynced,
            MembershipSwitchBarrier::AppendSynced,
            MembershipSwitchBarrier::ManifestSynced,
            MembershipSwitchBarrier::DirectorySynced,
            MembershipSwitchBarrier::SelectionReplaced,
            MembershipSwitchBarrier::SelectionSynced,
            MembershipSwitchBarrier::ActivationSynced,
        ] {
            let (parent, home) = home();
            let server_key = "8".repeat(64);
            let workspace = "7".repeat(64);
            let store = GlobalMembershipStore::new(&home, "default", &server_key).unwrap();
            let active = Uuid::now_v7();
            let unknown = Uuid::now_v7();
            store
                .record(&workspace, active, MembershipDisposition::Active)
                .unwrap();
            let before = store
                .record(&workspace, unknown, MembershipDisposition::Unknown)
                .unwrap();
            let root = store.ensure_root().unwrap();
            let original = fs::read(root.join("membership.jsonl")).unwrap();
            let operation = Uuid::now_v7();
            {
                let _lock = lock_file(&root.join("server.lock")).unwrap();
                let result = store.compact_under_server_lock(
                    &root,
                    operation,
                    1,
                    &before.journal_sha256,
                    &mut |point| {
                        if point == fault {
                            Err(internal("injected membership crash"))
                        } else {
                            Ok(())
                        }
                    },
                );
                assert!(result.is_err());
            }
            assert_eq!(store.load().unwrap(), before);
            assert_eq!(fs::read(root.join("membership.jsonl")).unwrap(), original);
            assert_eq!(
                store
                    .record(&workspace, active, MembershipDisposition::Released)
                    .unwrap_err()
                    .code,
                "PROFILE_SERVER_BUSY"
            );
            {
                let _lock = lock_file(&root.join("server.lock")).unwrap();
                assert_eq!(
                    store
                        .compact_under_server_lock(
                            &root,
                            operation,
                            1,
                            &before.journal_sha256,
                            &mut |_| Ok(())
                        )
                        .unwrap(),
                    before
                );
                assert_eq!(
                    store
                        .compact_under_server_lock(
                            &root,
                            operation,
                            1,
                            &before.journal_sha256,
                            &mut |_| Ok(())
                        )
                        .unwrap(),
                    before
                );
            }
            let after = store
                .record(&workspace, active, MembershipDisposition::Released)
                .unwrap();
            assert_eq!(after.revision, before.revision + 1);
            assert_eq!(
                after.members[&format!("{workspace}:{unknown}")],
                before.members[&format!("{workspace}:{unknown}")]
            );
            assert_eq!(fs::read(root.join("membership.jsonl")).unwrap(), original);
            fs::remove_dir_all(parent).unwrap();
        }
    }

    #[test]
    fn distinct_members_and_legacy_over_cap_source_remain_compactable() {
        let (parent, home) = home();
        let server = "8".repeat(64);
        let workspace = "7".repeat(64);
        let store = GlobalMembershipStore::new(&home, "default", &server).unwrap();
        let root = store.ensure_root().unwrap();
        let journal = root.join("membership.jsonl");
        let mut records = Vec::new();
        let mut bytes = Vec::new();
        let mut previous = ZERO_HASH.to_owned();
        for revision in 1..=10_300 {
            let mut record = GlobalMembershipRecord {
                schema_version: 2,
                revision,
                workspace_id: workspace.clone(),
                run_id: Uuid::now_v7(),
                disposition: MembershipDisposition::Released,
                controller_id: Some(Uuid::now_v7()),
                worker_generation: Some(1),
                thread_id: Some(format!("thread-{}", "a".repeat(256))),
                connection_id: Some(Uuid::now_v7()),
                lifecycle: "closed".into(),
                writer: false,
                observed_epoch: Some(1),
                runtime_locator: Some("/tmp/dolgorae-501/s/retained-worker.sock".into()),
                previous_sha256: previous.clone(),
                record_sha256: String::new(),
            };
            let mut body = serde_json::to_value(&record).unwrap();
            body.as_object_mut().unwrap().remove("record_sha256");
            record.record_sha256 = canonical_value_digest(&body).unwrap();
            previous = record.record_sha256.clone();
            bytes.extend(serde_json::to_vec(&record).unwrap());
            bytes.push(b'\n');
            records.push(record);
        }
        assert!(bytes.len() as u64 > MAX_MEMBERSHIP_BYTES);
        fs::write(&journal, &bytes).unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
        let original = derive_index(&server, &journal, &records).unwrap();
        store_index(&root.join("members.json"), &original).unwrap();
        assert_eq!(store.load().unwrap_err().code, "STORAGE_CAPACITY_EXCEEDED");
        let lock = lock_file(&root.join("server.lock")).unwrap();
        assert_eq!(
            store.maintenance_basis_under_server_lock(&root).unwrap().1,
            original
        );
        let operation = Uuid::now_v7();
        let failed = store.compact_under_server_lock(
            &root,
            operation,
            1,
            &original.journal_sha256,
            &mut |point| {
                if point == MembershipSwitchBarrier::CheckpointSynced {
                    Err(internal("injected checkpoint interruption"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(failed.is_err());
        let checkpoint = root
            .join("membership-generations")
            .join(operation.to_string())
            .join("checkpoint.json");
        let checkpoint_bytes = fs::metadata(&checkpoint).unwrap().len();
        assert!(checkpoint_bytes > MAX_MEMBERSHIP_BYTES);
        assert!(checkpoint_bytes <= MAX_MEMBERSHIP_CHECKPOINT_BYTES);
        let started = std::time::Instant::now();
        assert_eq!(
            store
                .compact_under_server_lock(
                    &root,
                    operation,
                    1,
                    &original.journal_sha256,
                    &mut |_| Ok(())
                )
                .unwrap(),
            original
        );
        drop(lock);
        assert_eq!(store.load().unwrap(), original);
        assert_eq!(fs::read(&journal).unwrap(), bytes);
        let member = Uuid::now_v7();
        store
            .record(&workspace, member, MembershipDisposition::Active)
            .unwrap();
        let after = store
            .record(&workspace, member, MembershipDisposition::Released)
            .unwrap();
        for (key, record) in original.members {
            assert_eq!(after.members[&key], record);
        }
        println!(
            "distinct_members=10300 legacy_source_bytes={} checkpoint_bytes={} resume_ms={}",
            bytes.len(),
            checkpoint_bytes,
            started.elapsed().as_millis()
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn ten_thousand_synthetic_membership_cycles_keep_selected_journal_bounded() {
        let (parent, home) = home();
        let key = "8".repeat(64);
        let workspace = "7".repeat(64);
        let store = GlobalMembershipStore::new(&home, "default", &key).unwrap();
        let active = Uuid::now_v7();
        let unknown = Uuid::now_v7();
        let cycling = Uuid::now_v7();
        store
            .record(&workspace, active, MembershipDisposition::Active)
            .unwrap();
        let protected = store
            .record(&workspace, unknown, MembershipDisposition::Unknown)
            .unwrap();
        let root = store.ensure_root().unwrap();
        let original = fs::read(root.join("membership.jsonl")).unwrap();
        let mut maximum_append_bytes = 0;
        for cycle in 1..=10_000 {
            store
                .record_observed(
                    &workspace,
                    cycling,
                    MembershipDisposition::Active,
                    GlobalMembershipFacts {
                        worker_generation: Some(cycle),
                        ..GlobalMembershipFacts::for_disposition(MembershipDisposition::Active)
                    },
                )
                .unwrap();
            store
                .record(&workspace, cycling, MembershipDisposition::Released)
                .unwrap();
            let pinned = pin_membership(&root, "default", &key).unwrap();
            maximum_append_bytes =
                maximum_append_bytes.max(fs::metadata(&pinned.journal).unwrap().len());
            assert!(maximum_append_bytes <= MAX_MEMBERSHIP_BYTES);
            if cycle % 500 == 0 {
                let _lock = lock_file(&root.join("server.lock")).unwrap();
                let (generation, basis) = store.maintenance_basis_under_server_lock(&root).unwrap();
                store
                    .compact_under_server_lock(
                        &root,
                        Uuid::now_v7(),
                        generation,
                        &basis.journal_sha256,
                        &mut |_| Ok(()),
                    )
                    .unwrap();
            }
        }
        let index = store.load().unwrap();
        assert_eq!(index.revision, 20_002);
        assert_eq!(
            index.members[&format!("{workspace}:{active}")],
            protected.members[&format!("{workspace}:{active}")]
        );
        assert_eq!(
            index.members[&format!("{workspace}:{unknown}")],
            protected.members[&format!("{workspace}:{unknown}")]
        );
        assert_eq!(
            index.members[&format!("{workspace}:{cycling}")].worker_generation,
            Some(10_000)
        );
        assert_eq!(
            fs::read(root.join("membership.jsonl")).unwrap()[..original.len()],
            original
        );
        let pinned = pin_membership(&root, "default", &key).unwrap();
        let retained_bytes = pinned
            .history
            .iter()
            .map(|segment| segment.byte_length)
            .sum::<u64>();
        let retained_records = pinned
            .history
            .iter()
            .map(|segment| {
                membership_segment_bytes(&root, segment, "default", &key)
                    .unwrap()
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count()
            })
            .sum::<usize>();
        assert_eq!(retained_records, 20_002);
        assert_eq!(pinned.generation, 21);
        println!(
            "synthetic_membership_cycles=10000 accepted_records={} selected_append_max_bytes={} retained_history_bytes={} selected_generation={}",
            index.revision, maximum_append_bytes, retained_bytes, pinned.generation
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn membership_generation_refuses_missing_selection_and_corrupt_history() {
        let (parent, home) = home();
        let key = "8".repeat(64);
        let workspace = "7".repeat(64);
        let store = GlobalMembershipStore::new(&home, "default", &key).unwrap();
        let member = Uuid::now_v7();
        let original = store
            .record(&workspace, member, MembershipDisposition::Unknown)
            .unwrap();
        let root = store.ensure_root().unwrap();
        let id = Uuid::now_v7();
        {
            let _lock = lock_file(&root.join("server.lock")).unwrap();
            store
                .compact_under_server_lock(&root, id, 1, &original.journal_sha256, &mut |_| Ok(()))
                .unwrap();
        }
        let selected = root.join("membership-selection.json");
        let selection = fs::read(&selected).unwrap();
        fs::remove_file(&selected).unwrap();
        assert_eq!(
            store.load().unwrap_err().code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        assert_eq!(
            store
                .record(&workspace, member, MembershipDisposition::Released)
                .unwrap_err()
                .code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        fs::write(&selected, &selection).unwrap();
        fs::set_permissions(&selected, fs::Permissions::from_mode(0o600)).unwrap();
        let historical = root.join("membership.jsonl");
        let history = fs::read(&historical).unwrap();
        fs::write(&historical, b"corrupt history\n").unwrap();
        assert_eq!(
            store.load().unwrap_err().code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        fs::write(&historical, history).unwrap();
        assert_eq!(store.load().unwrap(), original);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn maintenance_and_existing_reads_reject_unsafe_profile_parents() {
        let (parent, home) = home();
        let store = GlobalMembershipStore::new(&home, "default", &"8".repeat(64)).unwrap();
        let root = store.ensure_root().unwrap();
        let original = store.load().unwrap();
        let profiles = home.root().join("profiles");
        let backup = parent.join("safe-profiles");
        for corruption in ["mode", "symlink"] {
            if corruption == "mode" {
                fs::set_permissions(&profiles, fs::Permissions::from_mode(0o755)).unwrap();
            } else {
                fs::rename(&profiles, &backup).unwrap();
                std::os::unix::fs::symlink(&backup, &profiles).unwrap();
            }
            assert_eq!(
                store.load_existing().unwrap_err().code,
                "RUNTIME_PATH_INVALID"
            );
            assert!(
                store
                    .with_maintenance_server_lock(|_, _| -> Result<(), MachineError> {
                        panic!("unsafe parent reached effect")
                    })
                    .is_err()
            );
            if corruption == "mode" {
                fs::set_permissions(&profiles, fs::Permissions::from_mode(0o700)).unwrap();
            } else {
                fs::remove_file(&profiles).unwrap();
                fs::rename(&backup, &profiles).unwrap();
            }
            assert_eq!(store.load().unwrap(), original);
            assert!(root.is_dir());
        }
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn membership_capacity_is_distinct_from_malformed_input() {
        let (parent, home) = home();
        let key = "8".repeat(64);
        let store = GlobalMembershipStore::new(&home, "default", &key).unwrap();
        let root = store.ensure_root().unwrap();
        let journal = root.join("membership.jsonl");
        fs::write(&journal, b"not JSON\n").unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            store.load().unwrap_err().code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        fs::write(&journal, vec![b'x'; MAX_MEMBERSHIP_BYTES as usize + 1]).unwrap();
        assert_eq!(
            store.load().unwrap_err().code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        let mut oversized = b"not JSON\n".to_vec();
        oversized.resize(MAX_MEMBERSHIP_BYTES as usize + 1, b'\n');
        fs::write(&journal, oversized).unwrap();
        assert_eq!(
            store.load().unwrap_err().code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        for prefix in [
            "{\"schema_version\":1,",
            "{\"schema_version\":2,\"schema_version\":2,",
            "{\"revision\":2,",
            "{\"previous_sha256\":\"wrong\",",
            "{\"record_sha256\":\"wrong\",",
            "{\"writer\":\"wrong\",",
        ] {
            let mut bytes = prefix.as_bytes().to_vec();
            bytes.resize(MAX_MEMBERSHIP_BYTES as usize + 1, b' ');
            fs::write(&journal, &bytes).unwrap();
            assert_eq!(
                store.load().unwrap_err().code,
                "PROFILE_MEMBERSHIP_INCOMPLETE",
                "{prefix}"
            );
            assert_eq!(fs::read(&journal).unwrap(), bytes);
        }
        let mut bad_hash = serde_json::to_vec(&json!({
            "schema_version":2,"revision":1,"workspace_id":"7".repeat(64),"run_id":Uuid::now_v7(),
            "disposition":"active","lifecycle":"active","writer":false,
            "previous_sha256":ZERO_HASH,"record_sha256":ZERO_HASH,
        }))
        .unwrap();
        bad_hash.resize(MAX_MEMBERSHIP_BYTES as usize + 1, b' ');
        fs::write(&journal, bad_hash).unwrap();
        assert_eq!(
            store.load().unwrap_err().code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        let mut valid = b" {\"schema_version\":2,\"revision\":1,".to_vec();
        valid.resize(MAX_MEMBERSHIP_BYTES as usize + 1, b' ');
        fs::write(&journal, valid).unwrap();
        assert_eq!(store.load().unwrap_err().code, "STORAGE_CAPACITY_EXCEEDED");
        fs::write(&journal, vec![b'\n'; MAX_MEMBERSHIP_BYTES as usize + 1]).unwrap();
        assert_eq!(store.load().unwrap_err().code, "STORAGE_CAPACITY_EXCEEDED");
        let before = fs::read(&journal).unwrap();
        assert_eq!(
            store
                .record(
                    &"7".repeat(64),
                    Uuid::now_v7(),
                    MembershipDisposition::Active
                )
                .unwrap_err()
                .code,
            "STORAGE_CAPACITY_EXCEEDED"
        );
        assert_eq!(fs::read(&journal).unwrap(), before);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn identical_membership_facts_reuse_verified_durable_state() {
        let (parent, home) = home();
        let key = "8".repeat(64);
        let workspace = "7".repeat(64);
        let store = GlobalMembershipStore::new(&home, "default", &key).unwrap();
        let id = Uuid::now_v7();
        let first = store
            .record(&workspace, id, MembershipDisposition::Active)
            .unwrap();
        let root = store.ensure_root().unwrap();
        let journal = fs::read(root.join("membership.jsonl")).unwrap();
        let index = fs::read(root.join("members.json")).unwrap();
        assert_eq!(
            store
                .record(&workspace, id, MembershipDisposition::Active)
                .unwrap(),
            first
        );
        assert_eq!(fs::read(root.join("membership.jsonl")).unwrap(), journal);
        assert_eq!(fs::read(root.join("members.json")).unwrap(), index);
        fs::write(root.join("members.json"), b"{}").unwrap();
        assert_eq!(
            store
                .record(&workspace, id, MembershipDisposition::Active)
                .unwrap_err()
                .code,
            "PROFILE_MEMBERSHIP_INCOMPLETE"
        );
        assert_eq!(fs::read(root.join("membership.jsonl")).unwrap(), journal);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn membership_compaction_serializes_concurrent_reader_and_writer() {
        let (parent, home) = home();
        let key = "8".repeat(64);
        let workspace = "7".repeat(64);
        let store = Arc::new(GlobalMembershipStore::new(&home, "default", &key).unwrap());
        let original = store
            .record(&workspace, Uuid::now_v7(), MembershipDisposition::Unknown)
            .unwrap();
        let root = store.ensure_root().unwrap();
        let lock = lock_file(&root.join("server.lock")).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let reader_store = Arc::clone(&store);
        let reader_barrier = Arc::clone(&barrier);
        let reader = thread::spawn(move || {
            reader_barrier.wait();
            reader_store.load().unwrap()
        });
        let writer_store = Arc::clone(&store);
        let writer_barrier = Arc::clone(&barrier);
        let writer_workspace = workspace.clone();
        let writer = thread::spawn(move || {
            writer_barrier.wait();
            writer_store
                .record(
                    &writer_workspace,
                    Uuid::now_v7(),
                    MembershipDisposition::Active,
                )
                .unwrap()
        });
        barrier.wait();
        thread::sleep(std::time::Duration::from_millis(20));
        assert!(!reader.is_finished() && !writer.is_finished());
        store
            .compact_under_server_lock(
                &root,
                Uuid::now_v7(),
                1,
                &original.journal_sha256,
                &mut |_| Ok(()),
            )
            .unwrap();
        drop(lock);
        let read = reader.join().unwrap();
        let written = writer.join().unwrap();
        assert!(read == original || read == written);
        assert_eq!(written.revision, original.revision + 1);
        for (id, member) in original.members {
            assert_eq!(written.members[&id], member);
        }
        assert_eq!(store.load().unwrap(), written);
        fs::remove_dir_all(parent).unwrap();
    }
}
