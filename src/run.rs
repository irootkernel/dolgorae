use crate::audit::{GENESIS_PREVIOUS_HASH, HASH_SCHEME, is_microsecond_utc_timestamp};
use crate::domain::{
    Access, AggregateKind, Assurance, ControlMode, ControllerIdentity, ControllerKind,
    ExecutionLane, Purpose, PurposeKind, RunLifecycle,
};
use crate::global_runtime::GlobalProfileBinding;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::workspace::{
    GitBaseline, LosslessPath, WorkspaceMode, WorkspacePlatform, atomic_create, create_directory,
    sync_directory, verify_secure_directory, verify_secure_file,
};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const CONTROLLER_CAPABILITY_DOMAIN: &[u8] = b"dolgorae.controller-capability.v1\0";
const MAX_CALLER_START_RESERVATION_BYTES: usize = 64 * 1024 * 1024;

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

#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub instructions: RunInstructionSnapshot,
    pub execution_lane: ExecutionLane,
    pub required_assurance: Assurance,
    pub native_subagent_policy: String,
}

/// Successor configuration; legacy v1/v2 records retain their original reader.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerAgentConfigurationSnapshot {
    pub schema_version: u32,
    pub selected_profile: String,
    pub global_profile_binding_sha256: String,
    pub model: String,
    pub default_effort: String,
    #[serde(deserialize_with = "deserialize_caller_purpose")]
    pub purpose: Purpose,
    pub required_capabilities: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub role_reference: Option<String>,
    pub instructions: crate::instructions::InstructionSnapshot,
    pub execution_lane: ExecutionLane,
    pub required_assurance: Assurance,
    pub native_subagent_policy: String,
}

fn deserialize_caller_purpose<'de, D>(deserializer: D) -> Result<Purpose, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CheckedPurpose {
        kind: PurposeKind,
        #[serde(deserialize_with = "Option::deserialize")]
        external_label: Option<String>,
    }
    let purpose = CheckedPurpose::deserialize(deserializer)?;
    Ok(Purpose {
        kind: purpose.kind,
        external_label: purpose.external_label,
    })
}

impl CallerAgentConfigurationSnapshot {
    pub fn validate(&self) -> Result<(), MachineError> {
        self.instructions.developer_prefix()?;
        if self.schema_version != 3
            || self.execution_lane != ExecutionLane::Dedicated
            || self.required_assurance != Assurance::BestEffortPersonalAlpha
            || self.native_subagent_policy != "enabled"
            || self.selected_profile.is_empty()
            || self.selected_profile.len() > 128
            || !self
                .selected_profile
                .bytes()
                .enumerate()
                .all(|(index, byte)| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
                })
            || self.global_profile_binding_sha256.len() != 64
            || !self
                .global_profile_binding_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !bounded_identity(&self.model, 128)
            || !bounded_identity(&self.default_effort, 64)
            || self.required_capabilities.len() > 64
            || self
                .required_capabilities
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.required_capabilities.len()
            || self
                .required_capabilities
                .iter()
                .any(|name| !bounded_identity(name, 128))
            || self
                .purpose
                .external_label
                .as_ref()
                .is_some_and(|label| !bounded_identity(label, 128))
        {
            return Err(MachineError::invalid_argument(
                "agent_configuration",
                "invalid successor Agent Configuration snapshot",
            ));
        }
        if let Some(reference) = &self.role_reference {
            crate::instructions::validate_role_name(reference).map_err(|_| {
                MachineError::invalid_argument(
                    "role_reference",
                    "role reference does not match the checked name pattern",
                )
            })?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentConfigurationWire {
    schema_version: u32,
    #[serde(default, deserialize_with = "deserialize_profile_field")]
    runtime_profile: Option<String>,
    #[serde(default, deserialize_with = "deserialize_profile_field")]
    runtime_profile_snapshot_sha256: Option<String>,
    #[serde(default, deserialize_with = "deserialize_profile_field")]
    selected_profile: Option<String>,
    #[serde(default, deserialize_with = "deserialize_profile_field")]
    global_profile_binding_sha256: Option<String>,
    model: String,
    default_effort: String,
    purpose: Purpose,
    required_capabilities: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    role_reference: Option<String>,
    #[serde(default, deserialize_with = "deserialize_profile_field")]
    normalized_instructions: Option<String>,
    instructions: RunInstructionSnapshot,
    execution_lane: ExecutionLane,
    required_assurance: Assurance,
    native_subagent_policy: String,
}

fn deserialize_profile_field<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Only an absent key receives the default None; a present key must be a string.
    String::deserialize(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for AgentConfigurationSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AgentConfigurationWire::deserialize(deserializer)?;
        let (runtime_profile, runtime_profile_snapshot_sha256) = match wire.schema_version {
            1 if wire.selected_profile.is_none()
                && wire.global_profile_binding_sha256.is_none() =>
            {
                (
                    wire.runtime_profile
                        .ok_or_else(|| serde::de::Error::missing_field("runtime_profile"))?,
                    wire.runtime_profile_snapshot_sha256.ok_or_else(|| {
                        serde::de::Error::missing_field("runtime_profile_snapshot_sha256")
                    })?,
                )
            }
            2 | 3
                if wire.runtime_profile.is_none()
                    && wire.runtime_profile_snapshot_sha256.is_none() =>
            {
                (
                    wire.selected_profile
                        .ok_or_else(|| serde::de::Error::missing_field("selected_profile"))?,
                    wire.global_profile_binding_sha256.ok_or_else(|| {
                        serde::de::Error::missing_field("global_profile_binding_sha256")
                    })?,
                )
            }
            _ => {
                return Err(serde::de::Error::custom(
                    "Agent Configuration profile fields do not match schema_version",
                ));
            }
        };
        let normalized_instructions = match (&wire.instructions, wire.schema_version) {
            (RunInstructionSnapshot::Legacy(_), 1 | 2) => wire
                .normalized_instructions
                .ok_or_else(|| serde::de::Error::missing_field("normalized_instructions"))?,
            (RunInstructionSnapshot::Caller(capture), 3)
                if wire.normalized_instructions.is_none() =>
            {
                capture.developer_prefix().map_err(|_| {
                    serde::de::Error::custom("captured instruction integrity failed")
                })?
            }
            _ => {
                return Err(serde::de::Error::custom(
                    "instruction generation does not match Agent Configuration",
                ));
            }
        };
        Ok(Self {
            schema_version: wire.schema_version,
            runtime_profile,
            runtime_profile_snapshot_sha256,
            model: wire.model,
            default_effort: wire.default_effort,
            purpose: wire.purpose,
            required_capabilities: wire.required_capabilities,
            role_reference: wire.role_reference,
            normalized_instructions,
            instructions: wire.instructions,
            execution_lane: wire.execution_lane,
            required_assurance: wire.required_assurance,
            native_subagent_policy: wire.native_subagent_policy,
        })
    }
}

impl Serialize for AgentConfigurationSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("AgentConfigurationSnapshot", 14)?;
        state.serialize_field("schema_version", &self.schema_version)?;
        if self.schema_version == 1 {
            state.serialize_field("runtime_profile", &self.runtime_profile)?;
            state.serialize_field(
                "runtime_profile_snapshot_sha256",
                &self.runtime_profile_snapshot_sha256,
            )?;
        } else {
            state.serialize_field("selected_profile", &self.runtime_profile)?;
            state.serialize_field(
                "global_profile_binding_sha256",
                &self.runtime_profile_snapshot_sha256,
            )?;
        }
        state.serialize_field("model", &self.model)?;
        state.serialize_field("default_effort", &self.default_effort)?;
        state.serialize_field("purpose", &self.purpose)?;
        state.serialize_field("required_capabilities", &self.required_capabilities)?;
        state.serialize_field("role_reference", &self.role_reference)?;
        if self.schema_version != 3 {
            state.serialize_field("normalized_instructions", &self.normalized_instructions)?;
        }
        state.serialize_field("instructions", &self.instructions)?;
        state.serialize_field("execution_lane", &self.execution_lane)?;
        state.serialize_field("required_assurance", &self.required_assurance)?;
        state.serialize_field("native_subagent_policy", &self.native_subagent_policy)?;
        state.end()
    }
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RunInstructionSnapshot {
    Legacy(InstructionSnapshot),
    Caller(crate::instructions::InstructionSnapshot),
}

impl From<InstructionSnapshot> for RunInstructionSnapshot {
    fn from(value: InstructionSnapshot) -> Self {
        Self::Legacy(value)
    }
}

impl From<crate::instructions::InstructionSnapshot> for RunInstructionSnapshot {
    fn from(value: crate::instructions::InstructionSnapshot) -> Self {
        Self::Caller(value)
    }
}

impl RunInstructionSnapshot {
    #[must_use]
    pub fn legacy(&self) -> Option<&InstructionSnapshot> {
        match self {
            Self::Legacy(value) => Some(value),
            Self::Caller(_) => None,
        }
    }

    #[must_use]
    pub fn caller(&self) -> Option<&crate::instructions::InstructionSnapshot> {
        match self {
            Self::Caller(value) => Some(value),
            Self::Legacy(_) => None,
        }
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        match self {
            Self::Legacy(value) => &value.normalized_sha256,
            Self::Caller(value) => &value.composition_sha256,
        }
    }

    #[must_use]
    pub fn contract(&self) -> serde_json::Value {
        match self {
            Self::Legacy(value) => serde_json::json!({
                "schema": value.schema, "common_prefix_version": value.common_prefix_version,
                "mode_prefix_version": value.mode_prefix_version, "purpose_prefix_version": value.purpose_prefix_version,
            }),
            Self::Caller(value) => serde_json::json!({
                "schema": value.schema, "core_prefix_version": value.core_prefix_version,
                "core_prefix_sha256": value.core_prefix_sha256, "composition_sha256": value.composition_sha256,
            }),
        }
    }
}

impl TryFrom<CallerAgentConfigurationSnapshot> for AgentConfigurationSnapshot {
    type Error = MachineError;

    fn try_from(value: CallerAgentConfigurationSnapshot) -> Result<Self, Self::Error> {
        value.validate()?;
        Ok(Self {
            schema_version: value.schema_version,
            runtime_profile: value.selected_profile,
            runtime_profile_snapshot_sha256: value.global_profile_binding_sha256,
            model: value.model,
            default_effort: value.default_effort,
            purpose: value.purpose,
            required_capabilities: value.required_capabilities,
            role_reference: value.role_reference,
            normalized_instructions: value.instructions.developer_prefix()?,
            instructions: value.instructions.into(),
            execution_lane: value.execution_lane,
            required_assurance: value.required_assurance,
            native_subagent_policy: value.native_subagent_policy,
        })
    }
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

#[must_use]
pub(crate) fn run_root(state_root: &Path, run_id: Uuid) -> PathBuf {
    state_root.join("runs").join(run_id.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkProvenance {
    pub source_run_id: Uuid,
    pub mode: String,
    pub source_turn_id: Option<String>,
    pub source_thread_id: Option<String>,
    pub last_confirmed_boundary: Option<String>,
    pub observed_source_lifecycle: String,
    pub unresolved_turn_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteContinuationProvenance {
    pub source_run_id: Uuid,
    pub source_turn_id: String,
    pub source_thread_id: String,
    pub creation_reason: String,
    pub source_controller_kind: String,
    pub destination_controller_kind: String,
    pub handoff_summary_sha256: Option<String>,
    pub artifact_refs: Vec<Uuid>,
    pub workspace_baseline_sha256: String,
    pub created_at: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_profile_binding: Option<GlobalProfileBinding>,
    pub agent_configuration: AgentConfigurationSnapshot,
    pub profile_capability_snapshot: ProfileCapabilitySnapshot,
    pub app_server: AppServerFacts,
    pub dolgorae: DolgoraeBuild,
    pub model: String,
    pub initial_reasoning_effort: String,
    pub default_reasoning_effort: String,
    pub instructions: RunInstructionSnapshot,
    pub controller: ControllerBinding,
    pub purpose: Purpose,
    pub parent_ref: Option<ParentReference>,
    pub required_capabilities: Vec<String>,
    pub thread_id: Option<String>,
    pub fork_provenance: Option<ForkProvenance>,
    #[serde(default)]
    pub write_continuation_provenance: Option<WriteContinuationProvenance>,
    pub aggregate_binding: Option<AggregateBinding>,
    pub audit: AuditPolicy,
    pub compatibility: CompatibilityVerdict,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_startup_access"
    )]
    pub requested_access: Option<crate::domain::CallerWorkspaceAccess>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_profile_field"
    )]
    pub selected_role: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_profile_field"
    )]
    pub start_contract: Option<String>,
}

fn deserialize_startup_access<'de, D>(
    deserializer: D,
) -> Result<Option<crate::domain::CallerWorkspaceAccess>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    crate::domain::CallerWorkspaceAccess::deserialize(deserializer).map(Some)
}

impl RunManifest {
    pub(crate) fn require_legacy_adapter(&self) -> Result<(), MachineError> {
        if self.schema_version != 3 {
            return Ok(());
        }
        Err(MachineError::new(
            "CAPABILITY_UNSUPPORTED",
            "the frozen legacy adapter cannot reinterpret a caller-workspace Run",
            false,
            serde_json::json!({"profile": self.profile.profile_name,
                "capability_name": "caller_workspace_legacy_adapter", "support": "unavailable"}),
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunDirectory {
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub audit: PathBuf,
    pub recovery: PathBuf,
}

/// One workspace-scoped allocation key and the Run it is bound to.
///
/// docs/specs/README.md: "Run allocation reserves its key before publishing a Run ...
/// Response loss is reconciled by retrying the identical allocation key, which
/// returns the original Run; changed normalized input is
/// `IDEMPOTENCY_CONFLICT` and can never allocate another Run under that key."
/// The reservation therefore lives beside the Runs rather than inside one:
/// a Run that was never published has no ledger to record its own key in.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartReservation {
    pub schema_version: u32,
    pub operation: String,
    pub idempotency_key: String,
    pub normalized_identity_sha256: String,
    pub run_id: Uuid,
}

fn default_caller_role() -> String {
    "general".to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerRunStartRequest {
    pub schema: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub workspace: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_binding",
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_workspace_id: Option<String>,
    pub profile: String,
    pub control_mode: ControlMode,
    #[serde(default)]
    pub access: crate::domain::CallerWorkspaceAccess,
    #[serde(default = "default_caller_role")]
    pub role: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_binding"
    )]
    pub specialist_policy: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub model: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub effort: Option<String>,
    #[serde(deserialize_with = "deserialize_start_purpose")]
    pub purpose: Purpose,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub parent_ref: Option<ParentReference>,
    pub required_capabilities: Vec<String>,
    pub idempotency_key: String,
}

fn deserialize_start_purpose<'de, D>(deserializer: D) -> Result<Purpose, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CheckedPurpose {
        kind: crate::domain::PurposeKind,
        #[serde(deserialize_with = "deserialize_required_option")]
        external_label: Option<String>,
    }
    let purpose = CheckedPurpose::deserialize(deserializer)?;
    Ok(Purpose {
        kind: purpose.kind,
        external_label: purpose.external_label,
    })
}

impl CallerRunStartRequest {
    /// Resolve the checked private request without observing workspace/Profile
    /// state or granting runtime authority. Explicit null is not omission.
    pub fn resolve(self) -> Result<Self, MachineError> {
        self.resolve_checked(false)
    }

    pub(crate) fn resolve_member(self, aggregate: &AggregateBinding) -> Result<Self, MachineError> {
        let namespace = match aggregate.aggregate_kind {
            AggregateKind::OrchestratedSession => "dolgorae.orchestrated-session.v1",
            AggregateKind::ExternalSpecialistEngagement => {
                "dolgorae.external-specialist-engagement.v1"
            }
        };
        if aggregate.member_kind != AggregateMemberKind::Specialist
            || self.control_mode != ControlMode::ManagedAgent
            || !parent_matches(self.parent_ref.as_ref(), namespace, aggregate.aggregate_id)
        {
            return Err(MachineError::invalid_argument(
                "parent_ref",
                "member startup requires its authoritative aggregate binding",
            ));
        }
        self.resolve_checked(true)
    }

    fn resolve_checked(mut self, admitted_member: bool) -> Result<Self, MachineError> {
        if self
            .specialist_policy
            .as_ref()
            .is_some_and(|name| crate::instructions::validate_role_name(name).is_err())
        {
            return Err(MachineError::invalid_argument(
                "specialist_policy",
                "policy name is invalid",
            ));
        }
        if self.schema != "dolgorae-run-start-request/v2" {
            return Err(MachineError::invalid_argument(
                "schema",
                "unsupported start request",
            ));
        }
        for (field, body, limit) in [
            ("workspace", self.workspace.as_deref(), 8192),
            ("profile", Some(self.profile.as_str()), 128),
            ("model", self.model.as_deref(), 128),
            ("effort", self.effort.as_deref(), 64),
            (
                "purpose.external_label",
                self.purpose.external_label.as_deref(),
                128,
            ),
            ("idempotency_key", Some(self.idempotency_key.as_str()), 128),
        ] {
            if body.is_some_and(|value| {
                value.is_empty() || value.len() > limit || value.contains('\0')
            }) {
                return Err(MachineError::invalid_argument(
                    field,
                    "invalid bounded startup value",
                ));
            }
        }
        if self.expected_workspace_id.as_ref().is_some_and(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(MachineError::invalid_argument(
                "expected_workspace_id",
                "expected lowercase SHA-256",
            ));
        }
        crate::instructions::validate_role_name(&self.role)?;
        if self.required_capabilities.len() > 64
            || self
                .required_capabilities
                .iter()
                .any(|value| value.is_empty() || value.len() > 128 || value.contains('\0'))
            || self
                .required_capabilities
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.required_capabilities.len()
        {
            return Err(MachineError::invalid_argument(
                "required_capabilities",
                "expected unique bounded capabilities",
            ));
        }
        if self
            .purpose
            .external_label
            .as_ref()
            .is_some_and(|value| value.chars().any(char::is_control))
        {
            return Err(MachineError::invalid_argument(
                "purpose",
                "invalid purpose label",
            ));
        }
        if let Some(parent) = &self.parent_ref
            && (self.control_mode == ControlMode::DirectInteractive
                || [
                    (parent.namespace.as_str(), 128),
                    (parent.kind.as_str(), 64),
                    (parent.id.as_str(), 256),
                ]
                .iter()
                .any(|(value, limit)| {
                    value.is_empty() || value.len() > *limit || value.chars().any(char::is_control)
                })
                || (!admitted_member
                    && matches!(
                        parent.namespace.as_str(),
                        "dolgorae.orchestrated-session.v1"
                            | "dolgorae.external-specialist-engagement.v1"
                    )))
        {
            return Err(MachineError::invalid_argument(
                "parent_ref",
                "invalid caller parent provenance",
            ));
        }
        self.required_capabilities.sort();
        self.schema = "dolgorae-run-start-resolved/v2".to_owned();
        Ok(self)
    }
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn deserialize_present_binding<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

/// Accepted caller startup input survives a crash before manifest publication.
/// No ready server epoch or advertised default is fabricated during admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerStartIntent {
    pub run_id: Uuid,
    pub created_at: String,
    pub workspace_id: String,
    pub canonical_workspace: LosslessPath,
    pub workspace_mode: WorkspaceMode,
    pub start_baseline: GitBaseline,
    #[serde(deserialize_with = "deserialize_accepted_caller_request")]
    pub request: CallerRunStartRequest,
    pub controller: ControllerBinding,
    pub global_profile_binding: GlobalProfileBinding,
    pub instructions: crate::instructions::InstructionSnapshot,
    pub readiness: CallerProfileReadiness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<crate::markdown_policy::PreparedPolicySession>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerProfileReadiness {
    pub model: String,
    pub default_effort: String,
    pub profile_capability_snapshot: ProfileCapabilitySnapshot,
}

/// Original startup acceptance, independent of later mutable Run policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerStartReceipt {
    pub schema: String,
    pub run_id: Uuid,
    pub accepted_intent_sha256: String,
    pub global_profile_binding_sha256: String,
    pub original_thread_id: String,
    pub run_generation: u64,
    #[serde(deserialize_with = "crate::domain::deserialize_caller_effective_policy")]
    pub effective_policy: crate::domain::EffectivePolicy,
}

impl CallerStartReceipt {
    fn validate(&self, manifest: &RunManifest, intent_sha256: &str) -> Result<(), MachineError> {
        let policy = &self.effective_policy;
        let access = match manifest.requested_access {
            Some(crate::domain::CallerWorkspaceAccess::Writable) => Access::Write,
            Some(crate::domain::CallerWorkspaceAccess::ReadOnly) => Access::Read,
            None => {
                return Err(reservation_invalid(
                    "caller receipt requires successor provenance",
                ));
            }
        };
        if self.schema != "dolgorae.caller-start-receipt/v1"
            || manifest.schema_version != 3
            || self.run_id != manifest.run_id
            || self.accepted_intent_sha256 != intent_sha256
            || !is_sha256(intent_sha256)
            || self.global_profile_binding_sha256
                != manifest
                    .global_profile_binding
                    .as_ref()
                    .ok_or_else(|| reservation_invalid("caller receipt has no Profile binding"))?
                    .digest()?
            || !bounded_identity(&self.original_thread_id, 256)
            || self.run_generation == 0
            || policy.access != access
            || policy.verification != crate::domain::PolicyVerification::Verified
            || policy.policy_epoch.0 == 0
            || policy
                .thread_generation
                .is_none_or(|generation| generation == 0)
            || policy.server_epoch.is_none_or(|epoch| epoch == 0)
            || if access == Access::Write {
                policy
                    .writer_generation
                    .is_none_or(|generation| generation == 0)
            } else {
                policy.writer_generation.is_some()
            }
        {
            return Err(reservation_invalid(
                "caller startup receipt is inconsistent",
            ));
        }
        Ok(())
    }
}

fn deserialize_accepted_caller_request<'de, D>(
    deserializer: D,
) -> Result<CallerRunStartRequest, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some("dolgorae-run-start-resolved/v2")
        || value.get("access").is_none()
        || value.get("role").is_none()
    {
        return Err(serde::de::Error::custom(
            "accepted caller selections must be explicit",
        ));
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

impl CallerStartIntent {
    fn validate(&self) -> Result<(), MachineError> {
        let path = self.canonical_workspace.to_path_buf()?;
        caller_start_request_digest(&self.request, &self.canonical_workspace, &self.controller)?;
        if self.run_id.get_version_num() != 7
            || !is_microsecond_utc_timestamp(&self.created_at)
            || crate::workspace::workspace_id(&path) != self.workspace_id
            || self.request.profile != self.global_profile_binding.selected_name
            || self.request.role != self.instructions.role.name
        {
            return Err(reservation_invalid(
                "caller startup identity is inconsistent",
            ));
        }
        self.global_profile_binding.validate_for_recovery()?;
        self.instructions.developer_prefix()?;
        if let Some(captures) = &self.orchestration {
            captures.validate_for_workspace(&self.workspace_id, &self.canonical_workspace)?;
            if self.request.specialist_policy.as_deref()
                != Some(captures.policy().policy_name.as_str())
            {
                return Err(reservation_invalid(
                    "caller policy capture differs from accepted selection",
                ));
            }
        } else if self.request.specialist_policy.is_some() {
            return Err(reservation_invalid(
                "caller policy selection has no accepted capture",
            ));
        }
        let ready = &self.readiness;
        let caps = &ready.profile_capability_snapshot;
        if !bounded_identity(&ready.model, 128)
            || !bounded_identity(&ready.default_effort, 64)
            || self
                .request
                .model
                .as_ref()
                .is_some_and(|model| model != &ready.model)
            || self
                .request
                .effort
                .as_ref()
                .is_some_and(|effort| effort != &ready.default_effort)
            || caps.schema_version != 1
            || caps.profile_name != self.request.profile
            || caps.server_key != self.global_profile_binding.server_key
            || caps.server_epoch == 0
            || caps.app_server_version != self.global_profile_binding.launch_snapshot.codex_version
            || caps.schema_sha256
                != self
                    .global_profile_binding
                    .launch_snapshot
                    .schema_bundle_sha256
            || self.request.required_capabilities.iter().any(|capability| {
                caps.capabilities.get(capability) != Some(&CapabilityState::Supported)
            })
        {
            return Err(reservation_invalid(
                "caller readiness does not match accepted launch input",
            ));
        }
        Ok(())
    }

    /// Retry identity excludes mutable files and path aliases, while retaining
    /// every semantic request choice and the authenticated Controller binding.
    pub fn request_digest(&self) -> Result<String, MachineError> {
        self.validate()?;
        caller_start_request_digest(&self.request, &self.canonical_workspace, &self.controller)
    }
}

/// Compute retry identity before loading instruction files or Profile state.
/// The request must already contain checked, resolved startup selections.
pub fn caller_start_request_digest(
    request: &CallerRunStartRequest,
    canonical_workspace: &LosslessPath,
    controller: &ControllerBinding,
) -> Result<String, MachineError> {
    let path = canonical_workspace.to_path_buf()?;
    let workspace_id = crate::workspace::workspace_id(&path);
    let mut checked = request.clone();
    checked.schema = "dolgorae-run-start-request/v2".to_owned();
    if &checked.resolve_checked(true)? != request
        || !path.is_absolute()
        || path.as_os_str().as_bytes().contains(&0)
        || request
            .expected_workspace_id
            .as_ref()
            .is_some_and(|id| id != &workspace_id)
        || controller.identity.controller_id.get_version_num() != 7
        || controller.identity.generation == 0
        || !bounded_identity(&controller.identity.instance_id, 128)
        || controller
            .identity
            .subject_id
            .as_deref()
            .is_some_and(|id| !bounded_identity(id, 256))
        || !is_sha256(&controller.capability_sha256)
    {
        return Err(reservation_invalid(
            "caller startup request identity is inconsistent",
        ));
    }
    let mut request = request.clone();
    request.workspace = None;
    request.expected_workspace_id = None;
    caller_start_digest(&serde_json::json!({
        "request": request, "workspace_id": workspace_id,
        "canonical_workspace": canonical_workspace, "controller": controller,
    }))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerStartReservation {
    pub schema_version: u32,
    pub operation: String,
    pub idempotency_key: String,
    pub normalized_identity_sha256: String,
    pub request_sha256: String,
    pub run_id: Uuid,
    pub accepted_intent: CallerStartIntent,
}

impl CallerStartReservation {
    pub fn new(intent: CallerStartIntent) -> Result<Self, MachineError> {
        let request_sha256 = intent.request_digest()?;
        Ok(Self {
            schema_version: 2,
            operation: "start_run".to_owned(),
            idempotency_key: intent.request.idempotency_key.clone(),
            normalized_identity_sha256: caller_start_digest(&intent)?,
            request_sha256,
            run_id: intent.run_id,
            accepted_intent: intent,
        })
    }

    fn validate(&self) -> Result<(), MachineError> {
        if self.schema_version != 2
            || self.operation != "start_run"
            || self.idempotency_key != self.accepted_intent.request.idempotency_key
            || self.run_id != self.accepted_intent.run_id
            || self.request_sha256 != self.accepted_intent.request_digest()?
            || self.normalized_identity_sha256 != caller_start_digest(&self.accepted_intent)?
        {
            return Err(reservation_invalid(
                "caller reservation differs from accepted input",
            ));
        }
        Ok(())
    }
}

fn caller_start_digest(value: &impl Serialize) -> Result<String, MachineError> {
    if !canonical_round_trip_preserves(value)
        .map_err(|_| reservation_invalid("caller startup input is unrepresentable"))?
    {
        return Err(reservation_invalid(
            "caller startup input is not lossless JSON",
        ));
    }
    let text = serde_json::to_string(value)
        .map_err(|_| reservation_invalid("caller startup input is unrepresentable"))?;
    let bytes = canonicalize(
        &parse(&text)
            .map_err(|_| reservation_invalid("caller startup input is unrepresentable"))?,
    )
    .map_err(|_| reservation_invalid("caller startup input is unrepresentable"))?;
    Ok(sha256_hex(&bytes))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AllocationReservation {
    Legacy(StartReservation),
    Caller(Box<CallerStartReservation>),
}

/// The workspace's durable index of `run start` allocation keys.
pub struct StartReservationStore<P> {
    platform: P,
    state_root: PathBuf,
}

/// Held through allocation publication and its membership/receipt boundary.
/// The permanent inode is never removed on release.
#[must_use = "allocation is serialized only while this guard is alive"]
pub(crate) struct AllocationLock {
    file: File,
}

impl Drop for AllocationLock {
    fn drop(&mut self) {
        let _ = crate::darwin::DarwinSystem.unlock(&self.file);
    }
}

impl<P: WorkspacePlatform> StartReservationStore<P> {
    #[must_use]
    pub fn new(platform: P, state_root: impl Into<PathBuf>) -> Self {
        Self {
            platform,
            state_root: state_root.into(),
        }
    }

    fn root(&self, operation: &str) -> PathBuf {
        self.state_root.join("idempotency").join(operation)
    }

    fn ensure_root(&self, directory: &str) -> Result<PathBuf, MachineError> {
        let uid = self.platform.current_uid();
        verify_secure_directory(&self.state_root, uid)?;
        let root = self.root(directory);
        for directory in [self.state_root.join("idempotency"), root.clone()] {
            match create_directory(&directory, 0o700) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(run_path_error(&directory, error.to_string())),
            }
            verify_secure_directory(&directory, uid)?;
        }
        Ok(root)
    }

    pub(crate) fn lock_allocation(
        &self,
        directory: &str,
        key: &str,
    ) -> Result<AllocationLock, MachineError> {
        let root = self.ensure_root(directory)?;
        let path = root.join(format!("{}.lock", sha256_hex(key.as_bytes())));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|error| run_path_error(&path, error.to_string()))?;
        let verify = || -> Result<(), MachineError> {
            verify_secure_directory(&self.state_root, self.platform.current_uid())?;
            verify_secure_directory(
                &self.state_root.join("idempotency"),
                self.platform.current_uid(),
            )?;
            verify_secure_directory(&root, self.platform.current_uid())?;
            verify_secure_file(&path, self.platform.current_uid())?;
            let held = file
                .metadata()
                .map_err(|error| run_path_error(&path, error.to_string()))?;
            let named = fs::symlink_metadata(&path)
                .map_err(|error| run_path_error(&path, error.to_string()))?;
            if !held.is_file()
                || held.nlink() != 1
                || (held.dev(), held.ino()) != (named.dev(), named.ino())
            {
                return Err(run_path_error(&path, "allocation lock identity changed"));
            }
            Ok(())
        };
        verify()?;
        crate::darwin::DarwinSystem
            .lock_exclusive(&file)
            .map_err(|error| run_path_error(&path, error.to_string()))?;
        verify()?;
        file.sync_all()
            .map_err(|error| run_path_error(&path, error.to_string()))?;
        sync_directory(&root).map_err(|error| run_path_error(&root, error.to_string()))?;
        Ok(AllocationLock { file })
    }

    /// The file one key is recorded in.
    ///
    /// Named by the key's digest, never by the key itself: an idempotency key
    /// is caller-supplied text and a pathname is not a safe place for it.
    fn path(&self, operation: &str, idempotency_key: &str) -> PathBuf {
        self.root(operation)
            .join(format!("{}.json", sha256_hex(idempotency_key.as_bytes())))
    }

    /// The reservation this key already holds, if any.
    pub fn load(&self, idempotency_key: &str) -> Result<Option<StartReservation>, MachineError> {
        self.load_operation("run-start", "start_run", idempotency_key)
    }

    pub fn load_operation(
        &self,
        directory: &str,
        operation: &str,
        idempotency_key: &str,
    ) -> Result<Option<StartReservation>, MachineError> {
        let Some(decoded) = self.load_record(directory, idempotency_key)? else {
            return Ok(None);
        };
        let reservation = match decoded {
            AllocationReservation::Legacy(reservation) => reservation,
            AllocationReservation::Caller(reservation) => {
                reservation.validate()?;
                if directory != "run-start"
                    || operation != "start_run"
                    || reservation.idempotency_key != idempotency_key
                {
                    return Err(reservation_invalid(
                        "caller reservation does not describe this key",
                    ));
                }
                return Err(MachineError::new(
                    "CAPABILITY_UNSUPPORTED",
                    "legacy allocation cannot reinterpret a caller startup reservation",
                    false,
                    serde_json::json!({"profile": reservation.accepted_intent.request.profile,
                        "capability_name": "caller_workspace_start_reservation", "support": "unavailable"}),
                ));
            }
        };
        if reservation.schema_version != 1
            || reservation.operation != operation
            || reservation.idempotency_key != idempotency_key
            || !is_sha256(&reservation.normalized_identity_sha256)
            || reservation.run_id.get_version_num() != 7
        {
            return Err(reservation_invalid(
                "reservation does not describe this key",
            ));
        }
        Ok(Some(reservation))
    }

    fn load_record(
        &self,
        directory: &str,
        idempotency_key: &str,
    ) -> Result<Option<AllocationReservation>, MachineError> {
        let path = self.path(directory, idempotency_key);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(run_path_error(&path, error.to_string())),
            Ok(_) => {}
        }
        verify_secure_file(&path, self.platform.current_uid())?;
        let file =
            crate::workspace::open_secure_file_if_present(&path, self.platform.current_uid())?
                .ok_or_else(|| reservation_invalid("reservation vanished"))?;
        let mut bytes = Vec::new();
        std::io::Read::take(file, (MAX_CALLER_START_RESERVATION_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| run_path_error(&path, error.to_string()))?;
        if bytes.len() > MAX_CALLER_START_RESERVATION_BYTES {
            return Err(reservation_invalid("reservation exceeds its bound"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| reservation_invalid("reservation is not UTF-8"))?;
        let canonical =
            canonicalize(&parse(text).map_err(|_| reservation_invalid("reservation is not JSON"))?)
                .map_err(|_| reservation_invalid("reservation is not canonicalizable"))?;
        let decoded: AllocationReservation = serde_json::from_slice(&canonical)
            .map_err(|_| reservation_invalid("reservation schema is invalid"))?;
        if matches!(decoded, AllocationReservation::Legacy(_))
            && bytes.len() > crate::jcs::RAW_PAYLOAD_LIMIT
        {
            return Err(reservation_invalid("legacy reservation exceeds its bound"));
        }
        if let AllocationReservation::Caller(reservation) = &decoded {
            let original: serde_json::Value = serde_json::from_slice(&canonical)
                .map_err(|_| reservation_invalid("reservation schema is invalid"))?;
            let retained = serde_json::to_value(reservation)
                .map_err(|_| reservation_invalid("reservation schema is invalid"))?;
            if original != retained {
                return Err(reservation_invalid(
                    "caller reservation contains omitted or unknown fields",
                ));
            }
        }
        Ok(Some(decoded))
    }

    /// Inspect the original accepted input before consulting live sources.
    /// A legacy key remains owned by its legacy Run and is never shadowed.
    pub fn load_caller(
        &self,
        idempotency_key: &str,
        request_sha256: &str,
    ) -> Result<Option<CallerStartReservation>, MachineError> {
        if !is_sha256(request_sha256) {
            return Err(reservation_invalid("caller request digest is invalid"));
        }
        let Some(decoded) = self.load_record("run-start", idempotency_key)? else {
            return Ok(None);
        };
        let reservation = match decoded {
            AllocationReservation::Legacy(legacy) => {
                if legacy.schema_version != 1
                    || legacy.operation != "start_run"
                    || legacy.idempotency_key != idempotency_key
                    || !is_sha256(&legacy.normalized_identity_sha256)
                    || legacy.run_id.get_version_num() != 7
                {
                    return Err(reservation_invalid(
                        "legacy reservation does not describe this key",
                    ));
                }
                return Err(idempotency_conflict(
                    legacy.run_id,
                    &legacy.normalized_identity_sha256,
                    request_sha256,
                ));
            }
            AllocationReservation::Caller(reservation) => *reservation,
        };
        reservation.validate()?;
        if reservation.idempotency_key != idempotency_key {
            return Err(reservation_invalid(
                "caller reservation does not describe this key",
            ));
        }
        if reservation.request_sha256 != request_sha256 {
            return Err(idempotency_conflict(
                reservation.run_id,
                &reservation.request_sha256,
                request_sha256,
            ));
        }
        Ok(Some(reservation))
    }

    pub fn reserve_caller(
        &self,
        reservation: &CallerStartReservation,
    ) -> Result<CallerStartReservation, MachineError> {
        reservation.validate()?;
        self.ensure_root("run-start")?;
        let path = self.path("run-start", &reservation.idempotency_key);
        let text = serde_json::to_string(reservation)
            .map_err(|_| reservation_invalid("caller reservation is unrepresentable"))?;
        let mut bytes = canonicalize(
            &parse(&text)
                .map_err(|_| reservation_invalid("caller reservation is unrepresentable"))?,
        )
        .map_err(|_| reservation_invalid("caller reservation is unrepresentable"))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_CALLER_START_RESERVATION_BYTES {
            return Err(reservation_invalid("reservation exceeds its bound"));
        }
        match atomic_create(&self.platform, &path, &bytes, 0o600) {
            Ok(()) => Ok(reservation.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self
                .load_caller(&reservation.idempotency_key, &reservation.request_sha256)?
                .ok_or_else(|| reservation_invalid("reservation vanished")),
            Err(error) => Err(run_path_error(&path, error.to_string())),
        }
    }

    /// Reserve one key for one Run, before that Run is published.
    ///
    /// The write is exclusive, so two concurrent allocations under one key
    /// cannot both win; the loser reads the winner's reservation and resolves
    /// against it exactly as a retry does.
    pub fn reserve(
        &self,
        reservation: &StartReservation,
    ) -> Result<StartReservation, MachineError> {
        let directory = match reservation.operation.as_str() {
            "start_run" => "run-start",
            "fork_run" => "run-fork",
            _ => return Err(reservation_invalid("reservation operation is invalid")),
        };
        self.ensure_root(directory)?;
        let path = self.path(directory, &reservation.idempotency_key);
        let mut bytes = canonicalize(
            &parse(
                &serde_json::to_string(reservation)
                    .map_err(|_| reservation_invalid("reservation is unrepresentable"))?,
            )
            .map_err(|_| reservation_invalid("reservation is unrepresentable"))?,
        )
        .map_err(|_| reservation_invalid("reservation is unrepresentable"))?;
        bytes.push(b'\n');
        match atomic_create(&self.platform, &path, &bytes, 0o600) {
            Ok(()) => Ok(reservation.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self
                .load_operation(
                    directory,
                    &reservation.operation,
                    &reservation.idempotency_key,
                )?
                .ok_or_else(|| reservation_invalid("reservation vanished")),
            Err(error) => Err(run_path_error(&path, error.to_string())),
        }
    }
}

/// A reservation file that cannot be trusted is durable-state corruption, not
/// a fact about the caller's arguments.
fn reservation_invalid(reason: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "run allocation reservation is unreadable",
        false,
        serde_json::json!({"invariant": reason}),
    )
}

/// The registered refusal for a key reused with different normalized input.
#[must_use]
pub fn idempotency_conflict(run_id: Uuid, recorded: &str, observed: &str) -> MachineError {
    MachineError::new(
        "IDEMPOTENCY_CONFLICT",
        "idempotency key was reused with different normalized input",
        false,
        serde_json::json!({
            "run_id": run_id,
            "recorded_input_digest": recorded,
            "observed_input_digest": observed,
        }),
    )
}

pub struct RunStore<P> {
    platform: P,
    state_root: PathBuf,
}

impl<P: WorkspacePlatform> RunStore<P> {
    pub(crate) fn state_root(&self) -> &Path {
        &self.state_root
    }
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
        let _admission = crate::ledger_generation::WorkspaceLease::for_run(&root).map_err(
            |error| match error {
                crate::ledger::LedgerError::SecurityPolicy(error) => error,
                crate::ledger::LedgerError::WriterBusy(_) => MachineError::new(
                    "RUN_BUSY",
                    "workspace maintenance prevents Run publication",
                    true,
                    serde_json::json!({"run_id":manifest.run_id,"owner_kind":"startup"}),
                ),
                error => run_path_error(&root, error.to_string()),
            },
        )?;
        if fs::symlink_metadata(&root).is_ok() {
            // The published Run's own lifecycle, not a guess: a Run whose
            // worker has committed no projection yet reads as `starting`,
            // which is exactly what it is.
            let state = self
                .load_state_projection(manifest.run_id)
                .map_or(RunLifecycle::Starting, |projection| projection.lifecycle);
            return Err(MachineError::new(
                "RUN_STATE_CONFLICT",
                "run identity already has durable state",
                false,
                serde_json::json!({
                    "run_id": manifest.run_id,
                    "state": state,
                    "operation": "run.start",
                }),
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

    pub fn load_caller_start_receipt(
        &self,
        run_id: Uuid,
        intent_sha256: &str,
    ) -> Result<Option<CallerStartReceipt>, MachineError> {
        let manifest = self.load_manifest(run_id)?;
        let path = run_root(&self.state_root, run_id).join("caller-start-receipt.json");
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(run_path_error(&path, error.to_string())),
            Ok(_) => {}
        }
        verify_secure_file(&path, self.platform.current_uid())?;
        let bytes = fs::read(&path).map_err(|error| run_path_error(&path, error.to_string()))?;
        if bytes.len() > 8192 {
            return Err(reservation_invalid("caller receipt exceeds its bound"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| reservation_invalid("caller receipt is not UTF-8"))?;
        let canonical = canonicalize(
            &parse(text).map_err(|_| reservation_invalid("caller receipt is invalid"))?,
        )
        .map_err(|_| reservation_invalid("caller receipt is invalid"))?;
        let receipt: CallerStartReceipt = serde_json::from_slice(&canonical)
            .map_err(|_| reservation_invalid("caller receipt shape is invalid"))?;
        receipt.validate(&manifest, intent_sha256)?;
        Ok(Some(receipt))
    }

    pub fn publish_caller_start_receipt(
        &self,
        receipt: &CallerStartReceipt,
    ) -> Result<CallerStartReceipt, MachineError> {
        let manifest = self.load_manifest(receipt.run_id)?;
        receipt.validate(&manifest, &receipt.accepted_intent_sha256)?;
        let path = run_root(&self.state_root, receipt.run_id).join("caller-start-receipt.json");
        let text = serde_json::to_string(receipt)
            .map_err(|_| reservation_invalid("caller receipt is unrepresentable"))?;
        let mut bytes = canonicalize(
            &parse(&text).map_err(|_| reservation_invalid("caller receipt is unrepresentable"))?,
        )
        .map_err(|_| reservation_invalid("caller receipt is unrepresentable"))?;
        bytes.push(b'\n');
        match atomic_create(&self.platform, &path, &bytes, 0o600) {
            Ok(()) => Ok(receipt.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self
                .load_caller_start_receipt(receipt.run_id, &receipt.accepted_intent_sha256)?
                .ok_or_else(|| reservation_invalid("caller receipt vanished")),
            Err(error) => Err(run_path_error(&path, error.to_string())),
        }
    }

    pub fn load_manifest(&self, run_id: Uuid) -> Result<RunManifest, MachineError> {
        verify_secure_directory(&self.state_root, self.platform.current_uid())?;
        let runs = self.state_root.join("runs");
        verify_secure_directory(&runs, self.platform.current_uid())?;
        let root = runs.join(run_id.to_string());
        match fs::symlink_metadata(&root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(run_not_found(run_id));
            }
            Err(error) => return Err(run_path_error(&root, error.to_string())),
        }
        verify_secure_directory(&root, self.platform.current_uid())?;
        let path = root.join("manifest.json");
        verify_secure_file(&path, self.platform.current_uid())?;
        let bytes = fs::read(&path).map_err(|_| run_not_found(run_id))?;
        if bytes.len() > crate::jcs::RAW_PAYLOAD_LIMIT {
            return Err(run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "run manifest exceeds the bounded representation",
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "run manifest is not UTF-8",
            )
        })?;
        let canonical = canonicalize(&parse(text).map_err(|_| {
            run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "run manifest is invalid",
            )
        })?)
        .map_err(|_| {
            run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "run manifest is not canonicalizable",
            )
        })?;
        let shape: serde_json::Value = serde_json::from_slice(&canonical).map_err(|_| {
            run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "run manifest schema is invalid",
            )
        })?;
        if shape["schema_version"] == 3
            && [
                "/parent_ref",
                "/thread_id",
                "/fork_provenance",
                "/write_continuation_provenance",
                "/aggregate_binding",
                "/purpose/external_label",
                "/agent_configuration/purpose/external_label",
                "/agent_configuration/role_reference",
            ]
            .iter()
            .any(|pointer| shape.pointer(pointer).is_none())
        {
            return Err(run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "caller-workspace manifest is missing a required nullable field",
            ));
        }
        let manifest: RunManifest = serde_json::from_slice(&canonical).map_err(|_| {
            run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "run manifest schema is invalid",
            )
        })?;
        validate_manifest(&manifest)?;
        if manifest.run_id != run_id {
            return Err(run_state_invariant(
                run_id,
                RunStateInvariant::StateVariantMismatch,
                "run directory identity does not match its manifest",
            ));
        }
        Ok(manifest)
    }

    /// Resolve whether a Codex thread belongs to a durable external Reviewer
    /// Run without exposing the Run store's private directory layout.
    pub fn reviewer_thread_registered(&self, thread_id: &str) -> Result<bool, MachineError> {
        self.external_specialist_thread_matches(thread_id, true)
    }

    /// Resolve whether a Codex thread belongs to any durable External
    /// Specialist Engagement member. Facade entry points use this to deny
    /// nested first-class hiring independently of profile aliases.
    pub fn external_specialist_thread_registered(
        &self,
        thread_id: &str,
    ) -> Result<bool, MachineError> {
        self.external_specialist_thread_matches(thread_id, false)
    }

    fn external_specialist_thread_matches(
        &self,
        thread_id: &str,
        reviewer_only: bool,
    ) -> Result<bool, MachineError> {
        if thread_id.is_empty() || thread_id.len() > 256 || thread_id.chars().any(char::is_control)
        {
            return Ok(false);
        }
        verify_secure_directory(&self.state_root, self.platform.current_uid())?;
        let runs = self.state_root.join("runs");
        verify_secure_directory(&runs, self.platform.current_uid())?;
        let mut run_ids = fs::read_dir(&runs)
            .map_err(|error| run_path_error(&runs, error.to_string()))?
            .filter_map(|entry| {
                entry.ok().and_then(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .and_then(|name| Uuid::parse_str(name).ok())
                })
            })
            .collect::<Vec<_>>();
        run_ids.sort();
        for run_id in run_ids {
            let projection = self.load_state_projection(run_id)?;
            if projection.thread_id.as_deref() != Some(thread_id) {
                continue;
            }
            let manifest = self.load_manifest(run_id)?;
            let external_reviewer = manifest.control_mode == ControlMode::ManagedAgent
                && (!reviewer_only
                    || (manifest.purpose.kind == PurposeKind::Review
                        && manifest.execution_lane == ExecutionLane::SharedReadonly))
                && manifest.aggregate_binding.as_ref().is_some_and(|binding| {
                    binding.aggregate_kind == AggregateKind::ExternalSpecialistEngagement
                        && binding.member_kind == AggregateMemberKind::Specialist
                });
            if external_reviewer {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Read the Run's durable state projection without taking the ledger.
    ///
    /// The worker holds the ledger's exclusive lock for as long as the Run
    /// lives, so an observer reads `state.json` — the projection the ledger
    /// commits — instead of reopening the ledger behind the worker's back. A
    /// Run whose worker has not committed one yet reads as `starting`, which
    /// is what it is.
    pub fn load_state_projection(
        &self,
        run_id: Uuid,
    ) -> Result<crate::projection::RunStateProjection, MachineError> {
        let uid = self.platform.current_uid();
        let path = self
            .state_root
            .join("runs")
            .join(run_id.to_string())
            .join("state.json");
        if fs::symlink_metadata(&path).is_err() {
            return Ok(crate::projection::RunStateProjection::starting(run_id));
        }
        verify_secure_file(&path, uid)?;
        // `RUN_STATE_INVARIANT_VIOLATION` names a closed set of writer, epoch,
        // and lineage invariants; an unreadable durable projection is none of
        // them, so it is reported as the internal invariant failure it is.
        let invalid = || {
            MachineError::new(
                "INTERNAL_ERROR",
                "run state projection is unreadable",
                false,
                serde_json::json!({
                    "invariant": format!("run {run_id} state projection is unreadable"),
                }),
            )
        };
        let bytes = fs::read(&path).map_err(|_| invalid())?;
        if bytes.len() > crate::jcs::RAW_PAYLOAD_LIMIT {
            return Err(invalid());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
        let canonical =
            canonicalize(&parse(text).map_err(|_| invalid())?).map_err(|_| invalid())?;
        let projection: crate::projection::RunStateProjection =
            serde_json::from_slice(&canonical).map_err(|_| invalid())?;
        if projection.run_id != run_id {
            return Err(invalid());
        }
        Ok(projection)
    }

    pub fn load_controller_binding(&self, run_id: Uuid) -> Result<ControllerBinding, MachineError> {
        let manifest = self.load_manifest(run_id)?;
        self.load_controller_binding_from_manifest(&manifest)
    }

    /// Resolve the mutable binding against this capture's freshly validated
    /// manifest. The public authority reader always loads its own manifest.
    pub(crate) fn load_controller_binding_from_manifest(
        &self,
        manifest: &RunManifest,
    ) -> Result<ControllerBinding, MachineError> {
        let run_id = manifest.run_id;
        let path = self
            .state_root
            .join("runs")
            .join(run_id.to_string())
            .join("controller.json");
        if !path.exists() {
            return Ok(manifest.controller.clone());
        }
        verify_secure_file(&path, self.platform.current_uid())?;
        let bytes = fs::read(&path).map_err(|error| run_path_error(&path, error.to_string()))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| run_path_error(&path, error.to_string()))?;
        let canonical =
            canonicalize(&parse(text).map_err(|error| run_path_error(&path, error.to_string()))?)
                .map_err(|error| run_path_error(&path, error.to_string()))?;
        let binding: ControllerBinding = serde_json::from_slice(&canonical)
            .map_err(|error| run_path_error(&path, error.to_string()))?;
        if binding.identity.generation < manifest.controller.identity.generation
            || binding.capability_sha256.len() != 64
        {
            return Err(run_path_error(&path, "controller authority regressed"));
        }
        Ok(binding)
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
            run_state_invariant(
                manifest.run_id,
                RunStateInvariant::StateVariantMismatch,
                format!("run manifest serialization failed: {error}"),
            )
        })?;
        let mut manifest_bytes = canonicalize(&parse(&serialized).map_err(|error| {
            run_state_invariant(
                manifest.run_id,
                RunStateInvariant::StateVariantMismatch,
                format!("run manifest is not canonicalizable: {error}"),
            )
        })?)
        .map_err(|error| {
            run_state_invariant(
                manifest.run_id,
                RunStateInvariant::StateVariantMismatch,
                format!("run manifest is not canonicalizable: {error}"),
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
        let controller_bytes = serde_json::to_vec(&manifest.controller)
            .map_err(|error| run_path_error(staging.join("controller.json"), error.to_string()))?;
        atomic_create(
            &self.platform,
            &staging.join("controller.json"),
            &controller_bytes,
            0o600,
        )
        .map_err(|error| run_path_error(staging.join("controller.json"), error.to_string()))?;
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

/// The closed set of run-state invariants the checked error contract names.
///
/// `RUN_STATE_INVARIANT_VIOLATION` carries `reason` from a fixed enumeration
/// and always requires the same recovery, so the specific field that
/// disagreed travels in the message rather than being invented as a detail
/// member the contract forbids.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStateInvariant {
    AssuranceOrderInvalid,
    StateVariantMismatch,
    SharedServerEpochMismatch,
}

impl RunStateInvariant {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AssuranceOrderInvalid => "assurance_order_invalid",
            Self::StateVariantMismatch => "state_variant_mismatch",
            Self::SharedServerEpochMismatch => "shared_server_epoch_mismatch",
        }
    }
}

/// The checked `RUN_STATE_INVARIANT_VIOLATION` for one run.
#[must_use]
pub fn run_state_invariant(
    run_id: Uuid,
    reason: RunStateInvariant,
    message: impl Into<String>,
) -> MachineError {
    MachineError::new(
        "RUN_STATE_INVARIANT_VIOLATION",
        message,
        false,
        serde_json::json!({
            "run_id": run_id,
            "reason": reason.as_str(),
            "required_action": "reconcile_run_state",
        }),
    )
}

fn validate_manifest(manifest: &RunManifest) -> Result<(), MachineError> {
    let invalid = |reason: &str| {
        run_state_invariant(
            manifest.run_id,
            RunStateInvariant::StateVariantMismatch,
            reason,
        )
    };
    if !matches!(manifest.schema_version, 1..=3) || manifest.run_id.get_version_num() != 7 {
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
        || !is_sha256(manifest.instructions.digest())
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
        return Err(run_state_invariant(
            manifest.run_id,
            RunStateInvariant::AssuranceOrderInvalid,
            "achieved assurance cannot be below requested assurance",
        ));
    }
    validate_profile_snapshot(&manifest.profile).map_err(invalid)?;
    match (manifest.schema_version, &manifest.global_profile_binding) {
        (1, None) => {}
        (2 | 3, Some(binding)) => {
            binding.validate_for_recovery().map_err(|_| {
                invalid("global Profile binding is incomplete or internally inconsistent")
            })?;
            if binding.selected_name != manifest.profile.profile_name
                || binding.server_key != manifest.profile.initial_server_key
            {
                return Err(invalid(
                    "global Profile binding disagrees with the Run profile",
                ));
            }
        }
        _ => {
            return Err(invalid(if manifest.schema_version == 3 {
                "schema v3 requires a global Profile binding"
            } else {
                "schema v1 excludes and schema v2 requires a global Profile binding"
            }));
        }
    }
    validate_agent_configuration(manifest).map_err(invalid)?;
    if manifest.profile.profile_name != manifest.profile_capability_snapshot.profile_name {
        return Err(invalid("profile snapshot names disagree"));
    }
    if !is_sha256(&manifest.profile_capability_snapshot.server_key)
        || manifest.profile_capability_snapshot.server_key != manifest.profile.initial_server_key
        || manifest.profile_capability_snapshot.server_epoch == 0
    {
        return Err(run_state_invariant(
            manifest.run_id,
            RunStateInvariant::SharedServerEpochMismatch,
            "profile capability snapshot server key or epoch is invalid",
        ));
    }
    if manifest.profile_capability_snapshot.schema_version != 1
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
    {
        return Err(invalid(
            "fixed build, model, or instruction metadata is invalid",
        ));
    }
    match (&manifest.instructions, manifest.schema_version) {
        (RunInstructionSnapshot::Legacy(value), 1 | 2)
            if value.schema == "dolgorae.instructions/v1"
                && value.common_prefix_version == 1
                && value.mode_prefix_version == 1
                && value.purpose_prefix_version == 1
                && manifest.requested_access.is_none()
                && manifest.selected_role.is_none()
                && manifest.start_contract.is_none() => {}
        (RunInstructionSnapshot::Caller(value), 3) => {
            value
                .developer_prefix()
                .map_err(|_| invalid("captured instruction integrity failed"))?;
            let expected_access = match manifest.requested_access {
                Some(crate::domain::CallerWorkspaceAccess::ReadOnly) => Access::Read,
                Some(crate::domain::CallerWorkspaceAccess::Writable) => Access::Write,
                None => return Err(invalid("modern startup access is missing")),
            };
            if manifest.execution_lane != ExecutionLane::Dedicated
                || manifest.initial_access != expected_access
                || manifest.selected_role.as_deref() != Some(value.role.name.as_str())
                || manifest.start_contract.as_deref() != Some("caller_workspace/v1")
            {
                return Err(invalid(
                    "modern startup selection disagrees with the captured Run",
                ));
            }
        }
        _ => {
            return Err(invalid(
                "instruction or startup generation does not match manifest",
            ));
        }
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
    validate_fork_provenance(manifest).map_err(invalid)?;
    validate_write_continuation(manifest).map_err(invalid)?;
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
        return Err("Codex Profile snapshot is incomplete or invalid");
    }
    let executable_path = profile
        .executable_identity
        .resolved_path
        .to_path_buf()
        .map_err(|_| "Codex Profile executable path encoding is invalid")?;
    if !executable_path.is_absolute() {
        return Err("Codex Profile executable path must be absolute");
    }
    if profile.launch_contract_sha256 != launch_contract_digest(profile)? {
        return Err("Codex Profile launch-contract digest disagrees with its snapshot");
    }
    if !canonical_round_trip_preserves(profile)? {
        return Err("Codex Profile snapshot contains a non-lossless JSON number");
    }
    Ok(())
}

fn validate_agent_configuration(manifest: &RunManifest) -> Result<(), &'static str> {
    let agent = &manifest.agent_configuration;
    let expected_binding_sha256 = match manifest.schema_version {
        1 => canonical_digest(&manifest.profile)?,
        2 | 3 => manifest
            .global_profile_binding
            .as_ref()
            .ok_or("global Profile binding is missing")?
            .digest()
            .map_err(|_| "global Profile binding digest is invalid")?,
        _ => return Err("unsupported Run manifest generation"),
    };
    if agent.schema_version != manifest.schema_version
        || agent.runtime_profile != manifest.profile.profile_name
        || !is_sha256(&agent.runtime_profile_snapshot_sha256)
        || agent.runtime_profile_snapshot_sha256 != expected_binding_sha256
        || agent.model != manifest.model
        || agent.default_effort != manifest.default_reasoning_effort
        || agent.purpose != manifest.purpose
        || agent.required_capabilities != manifest.required_capabilities
        || agent.instructions != manifest.instructions
        || agent.execution_lane != manifest.execution_lane
        || agent.required_assurance != manifest.requested_assurance
        || agent.native_subagent_policy != "enabled"
        || agent.normalized_instructions.is_empty()
    {
        return Err("Agent Configuration snapshot disagrees with fixed Run identity");
    }
    match (&agent.instructions, manifest.schema_version) {
        (RunInstructionSnapshot::Legacy(value), 1 | 2) => {
            if agent.normalized_instructions.len() > 65_536
                || value.normalized_byte_length != agent.normalized_instructions.len() as u64
                || value.normalized_sha256 != sha256_hex(agent.normalized_instructions.as_bytes())
            {
                return Err("legacy instruction metadata disagrees with its body");
            }
        }
        (RunInstructionSnapshot::Caller(value), 3) => {
            let configuration = CallerAgentConfigurationSnapshot {
                schema_version: 3,
                selected_profile: agent.runtime_profile.clone(),
                global_profile_binding_sha256: agent.runtime_profile_snapshot_sha256.clone(),
                model: agent.model.clone(),
                default_effort: agent.default_effort.clone(),
                purpose: agent.purpose.clone(),
                required_capabilities: agent.required_capabilities.clone(),
                role_reference: agent.role_reference.clone(),
                instructions: value.clone(),
                execution_lane: agent.execution_lane,
                required_assurance: agent.required_assurance,
                native_subagent_policy: agent.native_subagent_policy.clone(),
            };
            configuration
                .validate()
                .map_err(|_| "modern Agent Configuration is invalid")?;
            if value
                .developer_prefix()
                .map_err(|_| "captured instruction integrity failed")?
                != agent.normalized_instructions
            {
                return Err("modern developer prefix disagrees with accepted captures");
            }
        }
        _ => return Err("instruction generation disagrees with Agent Configuration"),
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

fn validate_fork_provenance(manifest: &RunManifest) -> Result<(), &'static str> {
    let Some(provenance) = manifest.fork_provenance.as_ref() else {
        return Ok(());
    };
    let history_copy = provenance.mode == "history_copy";
    let fresh = provenance.mode == "fresh";
    if provenance.source_run_id == manifest.run_id
        || provenance.source_run_id.get_version_num() != 7
        || (!history_copy && !fresh)
        || !bounded_identity(&provenance.observed_source_lifecycle, 64)
        || history_copy
            && (provenance
                .source_turn_id
                .as_deref()
                .is_none_or(|value| !bounded_identity(value, 256))
                || provenance
                    .source_thread_id
                    .as_deref()
                    .is_none_or(|value| !bounded_identity(value, 256))
                || provenance
                    .last_confirmed_boundary
                    .as_deref()
                    .is_none_or(|value| !bounded_identity(value, 256)))
        || fresh
            && (provenance.source_turn_id.is_some()
                || provenance.source_thread_id.is_some()
                || provenance.last_confirmed_boundary.is_some())
        || provenance
            .unresolved_turn_id
            .as_deref()
            .is_some_and(|value| !bounded_identity(value, 256))
    {
        return Err("fork provenance is incomplete or invalid");
    }
    Ok(())
}

fn validate_write_continuation(manifest: &RunManifest) -> Result<(), &'static str> {
    let Some(provenance) = &manifest.write_continuation_provenance else {
        return Ok(());
    };
    if manifest.execution_lane != ExecutionLane::Dedicated
        || manifest.fork_provenance.is_some()
        || provenance.source_run_id == manifest.run_id
        || provenance.source_run_id.get_version_num() != 7
        || !bounded_identity(&provenance.source_turn_id, 256)
        || !bounded_identity(&provenance.source_thread_id, 256)
        || !matches!(
            provenance.creation_reason.as_str(),
            "shared_readonly_source"
                | "access_transition_unavailable"
                | "access_transition_unverified"
        )
        || !matches!(
            provenance.source_controller_kind.as_str(),
            "human_cli" | "interactive_client" | "workflow_orchestrator" | "automation"
        )
        || !matches!(
            provenance.destination_controller_kind.as_str(),
            "human_cli" | "interactive_client" | "workflow_orchestrator" | "automation"
        )
        || provenance.artifact_refs.len() > 64
        || provenance
            .artifact_refs
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || !is_sha256(&provenance.workspace_baseline_sha256)
        || provenance
            .handoff_summary_sha256
            .as_deref()
            .is_some_and(|digest| !is_sha256(digest))
        || provenance.created_at.len() < 20
        || !provenance.created_at.ends_with('Z')
    {
        return Err("write continuation provenance is incomplete or invalid");
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

#[cfg(test)]
mod caller_member_admission_tests {
    use super::*;

    #[test]
    fn admitted_member_parent_is_resolvable_but_public_start_cannot_forge_it() {
        let aggregate = AggregateBinding {
            aggregate_kind: AggregateKind::ExternalSpecialistEngagement,
            aggregate_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            member_kind: AggregateMemberKind::Specialist,
            policy_sha256: None,
            role_reference: Some("reviewer".to_owned()),
            role_snapshot_sha256: Some("a".repeat(64)),
            agent_configuration_sha256: Some("b".repeat(64)),
        };
        let mut request: CallerRunStartRequest = serde_json::from_str(include_str!(
            "../docs/protocol/examples/workspace-execution-start-defaults.valid.json"
        ))
        .unwrap();
        request.parent_ref = Some(ParentReference {
            namespace: "dolgorae.external-specialist-engagement.v1".to_owned(),
            kind: "specialist".to_owned(),
            id: aggregate.aggregate_id.to_string(),
        });
        assert!(request.clone().resolve().is_err());
        let accepted = request.clone().resolve_member(&aggregate).unwrap();
        assert_eq!(accepted.parent_ref, request.parent_ref);
        assert_eq!(accepted.schema, "dolgorae-run-start-resolved/v2");
        let mut wrong = aggregate.clone();
        wrong.aggregate_id = Uuid::now_v7();
        assert!(request.resolve_member(&wrong).is_err());
    }
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

fn run_not_found(run_id: Uuid) -> MachineError {
    MachineError::new(
        "RUN_NOT_FOUND",
        "run is unavailable",
        false,
        serde_json::json!({"run_id": run_id}),
    )
}

fn cleanup_staging(staging: &Path) {
    let _ = fs::remove_file(staging.join("manifest.json"));
    let _ = fs::remove_file(staging.join("controller.json"));
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

    #[test]
    fn agent_configuration_serialization_preserves_v1_and_uses_v2_profile_fields() {
        let configuration = |schema_version| AgentConfigurationSnapshot {
            schema_version,
            runtime_profile: "default".to_owned(),
            runtime_profile_snapshot_sha256: "a".repeat(64),
            model: "gpt-5.6".to_owned(),
            default_effort: "high".to_owned(),
            purpose: Purpose {
                kind: PurposeKind::Review,
                external_label: None,
            },
            required_capabilities: Vec::new(),
            role_reference: None,
            normalized_instructions: "review".to_owned(),
            instructions: InstructionSnapshot {
                schema: "dolgorae.instructions/v1".to_owned(),
                common_prefix_version: 1,
                mode_prefix_version: 1,
                purpose_prefix_version: 1,
                normalized_byte_length: 6,
                normalized_sha256: "b".repeat(64),
            }
            .into(),
            execution_lane: ExecutionLane::SharedReadonly,
            required_assurance: Assurance::BestEffortPersonalAlpha,
            native_subagent_policy: "enabled".to_owned(),
        };

        let v1 = serde_json::to_value(configuration(1)).unwrap();
        assert_eq!(v1["runtime_profile"], "default");
        assert_eq!(v1["runtime_profile_snapshot_sha256"], "a".repeat(64));
        assert!(v1.get("selected_profile").is_none());

        let v2 = serde_json::to_value(configuration(2)).unwrap();
        assert_eq!(v2["selected_profile"], "default");
        assert_eq!(v2["global_profile_binding_sha256"], "a".repeat(64));
        assert!(v2.get("runtime_profile").is_none());

        for (valid, wrong_version) in [(&v1, 2), (&v2, 1)] {
            assert!(serde_json::from_value::<AgentConfigurationSnapshot>(valid.clone()).is_ok());
            for field in valid.as_object().unwrap().keys() {
                let mut missing = valid.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(
                    serde_json::from_value::<AgentConfigurationSnapshot>(missing).is_err(),
                    "accepted missing {field} in v{}",
                    valid["schema_version"]
                );
            }
            for field in [
                "runtime_profile",
                "runtime_profile_snapshot_sha256",
                "selected_profile",
                "global_profile_binding_sha256",
            ] {
                for value in [
                    serde_json::Value::Null,
                    serde_json::json!(1),
                    serde_json::json!(false),
                    serde_json::json!({}),
                    serde_json::json!([]),
                ] {
                    let mut malformed = valid.clone();
                    malformed[field] = value;
                    assert!(
                        serde_json::from_value::<AgentConfigurationSnapshot>(malformed).is_err(),
                        "accepted non-string {field} in v{}",
                        valid["schema_version"]
                    );
                }
                if valid.get(field).is_none() {
                    let mut mixed = valid.clone();
                    mixed[field] = v1.get(field).or_else(|| v2.get(field)).unwrap().clone();
                    assert!(serde_json::from_value::<AgentConfigurationSnapshot>(mixed).is_err());
                }
            }
            let mut mismatched = valid.clone();
            mismatched["schema_version"] = serde_json::json!(wrong_version);
            assert!(serde_json::from_value::<AgentConfigurationSnapshot>(mismatched).is_err());

            let mut mixed = valid.clone();
            for field in [
                "runtime_profile",
                "runtime_profile_snapshot_sha256",
                "selected_profile",
                "global_profile_binding_sha256",
            ] {
                mixed[field] = v1.get(field).or_else(|| v2.get(field)).unwrap().clone();
            }
            assert!(serde_json::from_value::<AgentConfigurationSnapshot>(mixed).is_err());
        }
    }
}
