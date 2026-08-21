use crate::audit::{GENESIS_PREVIOUS_HASH, HASH_SCHEME, is_microsecond_utc_timestamp};
use crate::domain::{
    Access, AggregateKind, Assurance, ControlMode, ControllerIdentity, ControllerKind,
    ExecutionLane, Purpose,
};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::workspace::{
    GitBaseline, LosslessPath, WorkspaceMode, WorkspacePlatform, atomic_create, create_directory,
    sync_directory, verify_secure_directory, verify_secure_file,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const CONTROLLER_CAPABILITY_DOMAIN: &[u8] = b"dolgorae.controller-capability.v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParentReference {
    pub namespace: String,
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerBinding {
    pub identity: ControllerIdentity,
    pub capability_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Supported,
    RecognizedUnsupported,
    Unavailable,
    Unverified,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCapabilitySnapshot {
    pub schema_version: u32,
    pub profile_name: String,
    pub server_key: String,
    pub server_epoch: u64,
    pub app_server_version: String,
    pub schema_sha256: String,
    pub capabilities: BTreeMap<String, CapabilityState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSnapshot {
    pub schema_version: u32,
    pub profile_name: String,
    pub canonical_codex_home: String,
    pub normalized_argv: Vec<String>,
    pub launch_cwd_policy: String,
    pub derived_launch_cwd: String,
    pub sanitized_environment: BTreeMap<String, String>,
    pub enabled_features: Vec<String>,
    pub disabled_features: Vec<String>,
    pub process_static_configuration: BTreeMap<String, serde_json::Value>,
    pub initial_configuration_observation: BTreeMap<String, serde_json::Value>,
    pub executable_identity: ExecutableIdentity,
    pub codex_version: String,
    pub app_server_schema_sha256: String,
    pub compatibility_manifest_sha256: String,
    pub launch_contract_sha256: String,
    pub initial_server_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableIdentity {
    pub resolved_path: LosslessPath,
    pub device: u64,
    pub inode: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigurationSnapshot {
    pub schema_version: u32,
    pub runtime_profile: String,
    pub runtime_profile_snapshot_sha256: String,
    pub model: String,
    pub default_effort: String,
    pub purpose: Purpose,
    pub required_capabilities: Vec<String>,
    pub role_reference: Option<String>,
    pub normalized_instructions: String,
    pub instructions: InstructionSnapshot,
    pub execution_lane: ExecutionLane,
    pub required_assurance: Assurance,
    pub native_subagent_policy: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppServerFacts {
    pub version: Option<String>,
    pub schema_status: Option<String>,
    pub actual_codex_home: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DolgoraeBuild {
    pub version: String,
    pub binary_sha256: String,
    pub ipc_protocol_version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionSnapshot {
    pub schema: String,
    pub common_prefix_version: u32,
    pub mode_prefix_version: u32,
    pub purpose_prefix_version: u32,
    pub normalized_byte_length: u64,
    pub normalized_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateMemberKind {
    Primary,
    Specialist,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateBinding {
    pub aggregate_kind: AggregateKind,
    pub aggregate_id: Uuid,
    pub operation_id: Uuid,
    pub member_kind: AggregateMemberKind,
    pub policy_sha256: Option<String>,
    pub role_reference: Option<String>,
    pub role_snapshot_sha256: Option<String>,
    pub agent_configuration_sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkProvenance {
    pub source_run_id: Uuid,
    pub source_turn_id: String,
    pub source_thread_id: String,
    pub last_confirmed_boundary: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityVerdict {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditPolicy {
    pub hash_scheme: String,
    pub genesis_previous_hash: String,
    pub raw_payload_limit: u64,
    pub represented_payload_limit: u64,
}

impl Default for AuditPolicy {
    fn default() -> Self {
        Self {
            hash_scheme: HASH_SCHEME.to_owned(),
            genesis_previous_hash: GENESIS_PREVIOUS_HASH.to_owned(),
            raw_payload_limit: crate::jcs::RAW_PAYLOAD_LIMIT as u64,
            represented_payload_limit: crate::jcs::REPRESENTED_PAYLOAD_LIMIT as u64,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunManifest {
    pub schema_version: u32,
    pub run_id: Uuid,
    pub workspace_id: String,
    pub canonical_workspace: LosslessPath,
    pub workspace_mode: WorkspaceMode,
    pub start_baseline: GitBaseline,
    pub created_at: String,
    pub initial_access: Access,
    pub control_mode: ControlMode,
    pub execution_lane: ExecutionLane,
    pub requested_assurance: Assurance,
    pub achieved_assurance: Assurance,
    pub profile: ProfileSnapshot,
    pub agent_configuration: AgentConfigurationSnapshot,
    pub profile_capability_snapshot: ProfileCapabilitySnapshot,
    pub app_server: AppServerFacts,
    pub dolgorae: DolgoraeBuild,
    pub model: String,
    pub initial_reasoning_effort: String,
    pub default_reasoning_effort: String,
    pub instructions: InstructionSnapshot,
    pub controller: ControllerBinding,
    pub purpose: Purpose,
    pub parent_ref: Option<ParentReference>,
    pub required_capabilities: Vec<String>,
    pub thread_id: Option<String>,
    pub fork_provenance: Option<ForkProvenance>,
    pub aggregate_binding: Option<AggregateBinding>,
    pub audit: AuditPolicy,
    pub compatibility: CompatibilityVerdict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunDirectory {
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub audit: PathBuf,
    pub recovery: PathBuf,
}

pub struct RunStore<P> {
    platform: P,
    state_root: PathBuf,
}

impl<P: WorkspacePlatform> RunStore<P> {
    #[must_use]
    pub fn new(platform: P, state_root: impl Into<PathBuf>) -> Self {
        Self {
            platform,
            state_root: state_root.into(),
        }
    }

    pub fn publish(&self, manifest: &RunManifest) -> Result<RunDirectory, MachineError> {
        validate_manifest(manifest)?;
        verify_secure_directory(&self.state_root, self.platform.current_uid())?;
        let runs = self.state_root.join("runs");
        verify_secure_directory(&runs, self.platform.current_uid())?;
        let root = runs.join(manifest.run_id.to_string());
        if fs::symlink_metadata(&root).is_ok() {
            return Err(MachineError::new(
                "RUN_STATE_CONFLICT",
                "run identity already has durable state",
                false,
                serde_json::json!({"run_id": manifest.run_id}),
            ));
        }

        let staging = runs.join(format!(
            ".dolgorae-run-{}-{}",
            manifest.run_id,
            Uuid::now_v7()
        ));
        create_directory(&staging, 0o700)
            .map_err(|error| run_path_error(&staging, error.to_string()))?;
        let result = self.populate_and_publish(&staging, &root, manifest);
        if result.is_err() {
            cleanup_staging(&staging);
        }
        result
    }

    fn populate_and_publish(
        &self,
        staging: &Path,
        root: &Path,
        manifest: &RunManifest,
    ) -> Result<RunDirectory, MachineError> {
        let recovery = staging.join("recovery");
        create_directory(&recovery, 0o700)
            .map_err(|error| run_path_error(&recovery, error.to_string()))?;

        let serialized = serde_json::to_string(manifest).map_err(|error| {
            MachineError::new(
                "RUN_STATE_INVARIANT_VIOLATION",
                "run manifest serialization failed",
                false,
                serde_json::json!({"reason": error.to_string()}),
            )
        })?;
        let mut manifest_bytes = canonicalize(&parse(&serialized).map_err(|error| {
            MachineError::new(
                "RUN_STATE_INVARIANT_VIOLATION",
                "run manifest is not canonicalizable",
                false,
                serde_json::json!({"reason": error.to_string()}),
            )
        })?)
        .map_err(|error| {
            MachineError::new(
                "RUN_STATE_INVARIANT_VIOLATION",
                "run manifest is not canonicalizable",
                false,
                serde_json::json!({"reason": error.to_string()}),
            )
        })?;
        manifest_bytes.push(b'\n');
        atomic_create(
            &self.platform,
            &staging.join("manifest.json"),
            &manifest_bytes,
            0o600,
        )
        .map_err(|error| run_path_error(staging.join("manifest.json"), error.to_string()))?;
        atomic_create(&self.platform, &staging.join("audit.jsonl"), b"", 0o600)
            .map_err(|error| run_path_error(staging.join("audit.jsonl"), error.to_string()))?;
        sync_directory(staging).map_err(|error| run_path_error(staging, error.to_string()))?;

        self.platform
            .rename_exclusive(staging, root)
            .map_err(|error| run_path_error(root, error.to_string()))?;
        sync_directory(root.parent().expect("run root has parent"))
            .map_err(|error| run_path_error(root, error.to_string()))?;

        let directory = RunDirectory {
            root: root.to_owned(),
            manifest: root.join("manifest.json"),
            audit: root.join("audit.jsonl"),
            recovery: root.join("recovery"),
        };
        verify_secure_directory(&directory.root, self.platform.current_uid())?;
        verify_secure_directory(&directory.recovery, self.platform.current_uid())?;
        verify_secure_file(&directory.manifest, self.platform.current_uid())?;
        verify_secure_file(&directory.audit, self.platform.current_uid())?;
        Ok(directory)
    }
}

#[must_use]
pub fn controller_capability_digest(capability: &[u8; 32]) -> String {
    let mut preimage = Vec::with_capacity(CONTROLLER_CAPABILITY_DOMAIN.len() + capability.len());
    preimage.extend_from_slice(CONTROLLER_CAPABILITY_DOMAIN);
    preimage.extend_from_slice(capability);
    sha256_hex(&preimage)
}

fn validate_manifest(manifest: &RunManifest) -> Result<(), MachineError> {
    let invalid = |reason: &str| {
        MachineError::new(
            "RUN_STATE_INVARIANT_VIOLATION",
            "run manifest violates fixed semantics",
            false,
            serde_json::json!({"run_id": manifest.run_id, "reason": reason}),
        )
    };
    if manifest.schema_version != 1 || manifest.run_id.get_version_num() != 7 {
        return Err(invalid(
            "schema_version and UUIDv7 run identity are required",
        ));
    }
    if !is_microsecond_utc_timestamp(&manifest.created_at) {
        return Err(invalid(
            "created_at must be UTC RFC 3339 with exactly six fractional digits",
        ));
    }
    if !is_sha256(&manifest.workspace_id)
        || !is_sha256(&manifest.dolgorae.binary_sha256)
        || !is_sha256(&manifest.controller.capability_sha256)
        || !is_sha256(&manifest.instructions.normalized_sha256)
        || !is_sha256(&manifest.profile_capability_snapshot.schema_sha256)
    {
        return Err(invalid("all fixed digests must be lowercase SHA-256"));
    }
    if manifest.controller.identity.generation == 0 {
        return Err(invalid("controller generation must be positive"));
    }
    if manifest.controller.identity.controller_id.get_version_num() != 7
        || !bounded_identity(&manifest.controller.identity.instance_id, 128)
        || manifest
            .controller
            .identity
            .subject_id
            .as_deref()
            .is_some_and(|value| !bounded_identity(value, 256))
    {
        return Err(invalid("controller identity is incomplete or invalid"));
    }
    if manifest.control_mode == ControlMode::DirectInteractive && manifest.parent_ref.is_some() {
        return Err(invalid("direct_interactive runs cannot carry parent_ref"));
    }
    if manifest.execution_lane == ExecutionLane::SharedReadonly
        && manifest.initial_access != Access::Read
    {
        return Err(invalid("shared_readonly runs must begin with read access"));
    }
    if assurance_rank(manifest.achieved_assurance) < assurance_rank(manifest.requested_assurance) {
        return Err(invalid(
            "achieved assurance cannot be below requested assurance",
        ));
    }
    validate_profile_snapshot(&manifest.profile).map_err(invalid)?;
    validate_agent_configuration(manifest).map_err(invalid)?;
    if manifest.profile.profile_name != manifest.profile_capability_snapshot.profile_name {
        return Err(invalid("profile snapshot names disagree"));
    }
    if manifest.profile_capability_snapshot.schema_version != 1
        || !is_sha256(&manifest.profile_capability_snapshot.server_key)
        || manifest.profile_capability_snapshot.server_key != manifest.profile.initial_server_key
        || manifest.profile_capability_snapshot.server_epoch == 0
        || manifest
            .profile_capability_snapshot
            .app_server_version
            .is_empty()
        || manifest.profile_capability_snapshot.app_server_version != manifest.profile.codex_version
    {
        return Err(invalid("profile capability snapshot identity is invalid"));
    }
    let verdict_consistent = match manifest.compatibility {
        CompatibilityVerdict::Pending => {
            manifest.app_server.version.is_none()
                && manifest.app_server.schema_status.is_none()
                && manifest.app_server.actual_codex_home.is_none()
        }
        CompatibilityVerdict::Accepted => {
            manifest.app_server.version.as_deref()
                == Some(
                    manifest
                        .profile_capability_snapshot
                        .app_server_version
                        .as_str(),
                )
                && manifest.app_server.schema_status.as_deref() == Some("accepted")
                && manifest.app_server.actual_codex_home.as_deref()
                    == Some(manifest.profile.canonical_codex_home.as_str())
        }
        CompatibilityVerdict::Rejected => {
            manifest.app_server.schema_status.as_deref() == Some("rejected")
        }
    };
    if !verdict_consistent {
        return Err(invalid(
            "compatibility verdict and app-server completion facts disagree",
        ));
    }
    if manifest
        .app_server
        .version
        .as_deref()
        .is_some_and(|version| {
            version.is_empty() || version != manifest.profile_capability_snapshot.app_server_version
        })
        || manifest
            .app_server
            .schema_status
            .as_deref()
            .is_some_and(|status| !matches!(status, "accepted" | "rejected" | "pending"))
        || manifest
            .app_server
            .actual_codex_home
            .as_deref()
            .is_some_and(|home| {
                !Path::new(home).is_absolute() || home != manifest.profile.canonical_codex_home
            })
    {
        return Err(invalid(
            "app-server facts disagree with the accepted profile snapshot",
        ));
    }
    if manifest.dolgorae.version.is_empty()
        || manifest.dolgorae.ipc_protocol_version == 0
        || manifest.model.is_empty()
        || manifest.initial_reasoning_effort.is_empty()
        || manifest.default_reasoning_effort.is_empty()
        || manifest.instructions.schema != "dolgorae.instructions/v1"
        || manifest.instructions.common_prefix_version != 1
        || manifest.instructions.mode_prefix_version != 1
        || manifest.instructions.purpose_prefix_version != 1
    {
        return Err(invalid(
            "fixed build, model, or instruction metadata is invalid",
        ));
    }
    if manifest
        .thread_id
        .as_deref()
        .is_some_and(|value| !bounded_identity(value, 256))
    {
        return Err(invalid("thread identity is invalid"));
    }
    if manifest.required_capabilities.iter().any(String::is_empty) {
        return Err(invalid("required capability names cannot be empty"));
    }
    if manifest
        .required_capabilities
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
        || manifest.required_capabilities.iter().any(|required| {
            manifest
                .profile_capability_snapshot
                .capabilities
                .get(required)
                != Some(&CapabilityState::Supported)
        })
    {
        return Err(invalid(
            "required capabilities must be sorted, unique, and supported by the snapshot",
        ));
    }
    if manifest.audit != AuditPolicy::default() {
        return Err(invalid(
            "audit hash scheme, genesis, or payload bounds differ",
        ));
    }
    validate_controller_and_aggregate(manifest).map_err(invalid)?;
    validate_fork_provenance(manifest.fork_provenance.as_ref()).map_err(invalid)?;
    validate_bounded_metadata(manifest).map_err(invalid)
}

fn validate_profile_snapshot(profile: &ProfileSnapshot) -> Result<(), &'static str> {
    if profile.schema_version != 1
        || !bounded_identity(&profile.profile_name, 128)
        || !Path::new(&profile.canonical_codex_home).is_absolute()
        || profile.normalized_argv.is_empty()
        || profile.normalized_argv.iter().any(String::is_empty)
        || profile.launch_cwd_policy != "profile_state_directory_v1"
        || !Path::new(&profile.derived_launch_cwd).is_absolute()
        || profile.sanitized_environment.values().any(String::is_empty)
        || !sorted_unique(&profile.enabled_features)
        || !sorted_unique(&profile.disabled_features)
        || profile
            .enabled_features
            .iter()
            .any(|value| profile.disabled_features.binary_search(value).is_ok())
        || profile.executable_identity.inode == 0
        || !is_sha256(&profile.executable_identity.sha256)
        || !bounded_identity(&profile.codex_version, 128)
        || !is_sha256(&profile.app_server_schema_sha256)
        || !is_sha256(&profile.compatibility_manifest_sha256)
        || !is_sha256(&profile.launch_contract_sha256)
        || !is_sha256(&profile.initial_server_key)
    {
        return Err("Runtime Profile snapshot is incomplete or invalid");
    }
    let executable_path = profile
        .executable_identity
        .resolved_path
        .to_path_buf()
        .map_err(|_| "Runtime Profile executable path encoding is invalid")?;
    if !executable_path.is_absolute() {
        return Err("Runtime Profile executable path must be absolute");
    }
    if profile.launch_contract_sha256 != launch_contract_digest(profile)? {
        return Err("Runtime Profile launch-contract digest disagrees with its snapshot");
    }
    if !canonical_round_trip_preserves(profile)? {
        return Err("Runtime Profile snapshot contains a non-lossless JSON number");
    }
    Ok(())
}

fn validate_agent_configuration(manifest: &RunManifest) -> Result<(), &'static str> {
    let agent = &manifest.agent_configuration;
    if agent.schema_version != 1
        || agent.runtime_profile != manifest.profile.profile_name
        || !is_sha256(&agent.runtime_profile_snapshot_sha256)
        || agent.runtime_profile_snapshot_sha256 != canonical_digest(&manifest.profile)?
        || agent.model != manifest.model
        || agent.default_effort != manifest.default_reasoning_effort
        || agent.purpose != manifest.purpose
        || agent.required_capabilities != manifest.required_capabilities
        || agent.instructions != manifest.instructions
        || agent.execution_lane != manifest.execution_lane
        || agent.required_assurance != manifest.requested_assurance
        || agent.native_subagent_policy != "enabled"
        || agent.normalized_instructions.is_empty()
        || agent.normalized_instructions.len() > 65_536
        || agent.instructions.normalized_byte_length
            != u64::try_from(agent.normalized_instructions.len()).unwrap_or(u64::MAX)
        || agent.instructions.normalized_sha256
            != sha256_hex(agent.normalized_instructions.as_bytes())
    {
        return Err("Agent Configuration snapshot disagrees with fixed Run identity");
    }
    Ok(())
}

pub fn launch_contract_digest(profile: &ProfileSnapshot) -> Result<String, &'static str> {
    canonical_digest(&serde_json::json!({
        "schema_version": profile.schema_version,
        "canonical_codex_home": profile.canonical_codex_home,
        "normalized_argv": profile.normalized_argv,
        "launch_cwd_policy": profile.launch_cwd_policy,
        "executable_identity": profile.executable_identity,
        "launch_mode": "app_server_unix_socket_v1",
        "sanitized_environment": profile.sanitized_environment,
        "process_static_configuration": profile.process_static_configuration,
        "codex_version": profile.codex_version,
        "app_server_schema_sha256": profile.app_server_schema_sha256,
        "compatibility_manifest_sha256": profile.compatibility_manifest_sha256,
        "enabled_features": profile.enabled_features,
        "disabled_features": profile.disabled_features,
    }))
}

pub fn runtime_profile_snapshot_digest(profile: &ProfileSnapshot) -> Result<String, &'static str> {
    canonical_digest(profile)
}

pub fn agent_configuration_digest(
    configuration: &AgentConfigurationSnapshot,
) -> Result<String, &'static str> {
    canonical_digest(configuration)
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<String, &'static str> {
    let serialized = serde_json::to_string(value).map_err(|_| "snapshot serialization failed")?;
    let parsed = parse(&serialized).map_err(|_| "snapshot canonicalization failed")?;
    let canonical = canonicalize(&parsed).map_err(|_| "snapshot canonicalization failed")?;
    Ok(sha256_hex(&canonical))
}

fn canonical_round_trip_preserves<T: Serialize>(value: &T) -> Result<bool, &'static str> {
    let original = serde_json::to_value(value).map_err(|_| "snapshot serialization failed")?;
    let serialized = serde_json::to_string(value).map_err(|_| "snapshot serialization failed")?;
    let parsed = parse(&serialized).map_err(|_| "snapshot canonicalization failed")?;
    let canonical = canonicalize(&parsed).map_err(|_| "snapshot canonicalization failed")?;
    let round_trip: serde_json::Value =
        serde_json::from_slice(&canonical).map_err(|_| "snapshot canonicalization failed")?;
    Ok(original == round_trip)
}

fn sorted_unique(values: &[String]) -> bool {
    values.iter().all(|value| !value.is_empty())
        && !values.windows(2).any(|pair| pair[0] >= pair[1])
}

fn validate_fork_provenance(provenance: Option<&ForkProvenance>) -> Result<(), &'static str> {
    let Some(provenance) = provenance else {
        return Ok(());
    };
    if provenance.source_run_id.get_version_num() != 7
        || !bounded_identity(&provenance.source_turn_id, 256)
        || !bounded_identity(&provenance.source_thread_id, 256)
        || !bounded_identity(&provenance.last_confirmed_boundary, 256)
    {
        return Err("fork provenance is incomplete or invalid");
    }
    Ok(())
}

fn validate_controller_and_aggregate(manifest: &RunManifest) -> Result<(), &'static str> {
    let controller_kind = manifest.controller.identity.kind;
    if controller_kind == ControllerKind::Other {
        return Err("Controller kind other cannot bind a v1 Run");
    }
    match manifest.control_mode {
        ControlMode::DirectInteractive
            if !matches!(
                controller_kind,
                ControllerKind::HumanCli | ControllerKind::InteractiveClient
            ) =>
        {
            return Err("direct_interactive requires a human or interactive Controller");
        }
        ControlMode::ManagedAgent
            if !matches!(
                controller_kind,
                ControllerKind::WorkflowOrchestrator | ControllerKind::Automation
            ) =>
        {
            return Err("managed_agent requires a workflow or automation Controller");
        }
        _ => {}
    }

    let Some(binding) = manifest.aggregate_binding.as_ref() else {
        if manifest.parent_ref.as_ref().is_some_and(|parent| {
            matches!(
                parent.namespace.as_str(),
                "dolgorae.orchestrated-session.v1" | "dolgorae.external-specialist-engagement.v1"
            )
        }) {
            return Err("reserved parent namespaces require authoritative aggregate binding");
        }
        return Ok(());
    };
    if binding.aggregate_id.get_version_num() != 7 || binding.operation_id.get_version_num() != 7 {
        return Err("aggregate and operation identities must be UUIDv7");
    }
    let specialist = binding.member_kind == AggregateMemberKind::Specialist;
    if specialist
        != (binding.role_reference.is_some()
            && binding
                .role_snapshot_sha256
                .as_deref()
                .is_some_and(is_sha256)
            && binding
                .agent_configuration_sha256
                .as_deref()
                .is_some_and(is_sha256))
    {
        return Err(
            "Specialist aggregate bindings require complete role and Agent Configuration digests",
        );
    }
    if binding.member_kind == AggregateMemberKind::Primary
        && (binding.role_reference.is_some()
            || binding.role_snapshot_sha256.is_some()
            || binding.agent_configuration_sha256.is_some())
    {
        return Err("Primary aggregate bindings cannot carry Specialist role metadata");
    }
    if binding
        .role_reference
        .as_deref()
        .is_some_and(|value| !bounded_identity(value, 256))
    {
        return Err("aggregate role reference is invalid");
    }
    if specialist
        && (manifest.agent_configuration.role_reference != binding.role_reference
            || binding.agent_configuration_sha256.as_deref()
                != Some(canonical_digest(&manifest.agent_configuration)?.as_str()))
    {
        return Err("Specialist binding disagrees with the Agent Configuration snapshot");
    }

    match (binding.aggregate_kind, binding.member_kind) {
        (AggregateKind::OrchestratedSession, AggregateMemberKind::Primary) => {
            if manifest.control_mode != ControlMode::DirectInteractive
                || !matches!(
                    controller_kind,
                    ControllerKind::HumanCli | ControllerKind::InteractiveClient
                )
                || manifest.run_id != binding.aggregate_id
                || manifest.parent_ref.is_some()
                || !binding.policy_sha256.as_deref().is_some_and(is_sha256)
                || manifest.agent_configuration.role_reference.is_some()
            {
                return Err("Orchestrated Session Primary binding is inconsistent");
            }
        }
        (AggregateKind::OrchestratedSession, AggregateMemberKind::Specialist) => {
            if manifest.control_mode != ControlMode::ManagedAgent
                || controller_kind != ControllerKind::Automation
                || !parent_matches(
                    manifest.parent_ref.as_ref(),
                    "dolgorae.orchestrated-session.v1",
                    binding.aggregate_id,
                )
                || binding.policy_sha256.is_some()
            {
                return Err("brokered Specialist binding is inconsistent");
            }
        }
        (AggregateKind::ExternalSpecialistEngagement, AggregateMemberKind::Specialist) => {
            if manifest.control_mode != ControlMode::ManagedAgent
                || !matches!(
                    controller_kind,
                    ControllerKind::WorkflowOrchestrator | ControllerKind::Automation
                )
                || !parent_matches(
                    manifest.parent_ref.as_ref(),
                    "dolgorae.external-specialist-engagement.v1",
                    binding.aggregate_id,
                )
                || binding.policy_sha256.is_some()
            {
                return Err("External Specialist Engagement binding is inconsistent");
            }
        }
        (AggregateKind::ExternalSpecialistEngagement, AggregateMemberKind::Primary) => {
            return Err("External Specialist Engagement has no Primary Run binding");
        }
    }
    Ok(())
}

fn parent_matches(parent: Option<&ParentReference>, namespace: &str, aggregate_id: Uuid) -> bool {
    parent.is_some_and(|parent| {
        parent.namespace == namespace
            && parent.kind == "specialist"
            && parent.id == aggregate_id.to_string()
    })
}

fn bounded_identity(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value.chars().any(|character| character.is_control())
}

fn validate_bounded_metadata(manifest: &RunManifest) -> Result<(), &'static str> {
    let bounded = bounded_identity;
    if let Some(label) = manifest.purpose.external_label.as_deref()
        && !bounded(label, 128)
    {
        return Err("purpose external label is invalid");
    }
    if let Some(parent) = &manifest.parent_ref
        && (!bounded(&parent.namespace, 128)
            || !bounded(&parent.kind, 64)
            || !bounded(&parent.id, 256))
    {
        return Err("parent_ref metadata is invalid");
    }
    Ok(())
}

const fn assurance_rank(value: Assurance) -> u8 {
    match value {
        Assurance::BestEffortPersonalAlpha => 0,
        Assurance::VerifiedThreadScopedControl => 1,
        Assurance::StrongProcessContainment => 2,
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn run_path_error(path: impl AsRef<Path>, reason: impl Into<String>) -> MachineError {
    MachineError::runtime_path_invalid(path, reason)
}

fn cleanup_staging(staging: &Path) {
    let _ = fs::remove_file(staging.join("manifest.json"));
    let _ = fs::remove_file(staging.join("audit.jsonl"));
    let _ = fs::remove_dir(staging.join("recovery"));
    let _ = fs::remove_dir(staging);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_digest_is_domain_separated() {
        let capability = [7_u8; 32];
        assert_eq!(controller_capability_digest(&capability).len(), 64);
        assert_ne!(
            controller_capability_digest(&capability),
            sha256_hex(&capability)
        );
    }
}
