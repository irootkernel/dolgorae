//! Internal Markdown Specialist Policy successor support.
//!
//! The public policy CLI and legacy readers retain their v2 contracts. These
//! APIs prepare v3 snapshots for the caller-workspace cutover.

use crate::darwin::DarwinSystem;
use crate::global_runtime::GlobalProfileBinding;
use crate::instructions::{InstructionSnapshot, InstructionSourceStore, RoleCapture};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::orchestration::{AcceptedPolicySessionSnapshot, OrchestrationStore};
use crate::paths::DolgoraeHome;
use crate::run::CallerAgentConfigurationSnapshot;
use crate::semantic::{
    CallerAgentConfigurationInput, ExternalAgentConfigurationInput,
    prepare_caller_agent_configuration_read_only, prepare_external_specialist_read_only,
};
use crate::workspace::{LosslessPath, WorkspaceService, WorkspaceView, open_relative_nofollow};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

// Covers 64 maximally sized captures, JSON escaping, and the bounded input
// metadata. A legacy one-megabyte reader would reject valid v3 snapshots.
const MAX_POLICY_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSourceReference {
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAgentConfigurationInput {
    pub schema_version: u32,
    pub selected_profile: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_string"
    )]
    pub global_profile_binding_sha256: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub model: Option<String>,
    pub default_effort: String,
    pub purpose: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub purpose_label: Option<String>,
    pub required_capabilities: Vec<String>,
    pub execution_lane: String,
    pub required_assurance: String,
    pub native_subagent_policy: String,
}

impl PolicyAgentConfigurationInput {
    fn validate(&self) -> Result<(), MachineError> {
        if self.schema_version != 3
            || !name(&self.selected_profile, 128)
            || self
                .global_profile_binding_sha256
                .as_ref()
                .is_some_and(|digest| !is_sha256(digest))
            || self
                .model
                .as_ref()
                .is_some_and(|model| !bounded(model, 128))
            || !bounded(&self.default_effort, 64)
            || !matches!(
                self.purpose.as_str(),
                "planning"
                    | "implementation"
                    | "review"
                    | "research"
                    | "discussion"
                    | "workflow_stage"
                    | "other"
            )
            || (self.purpose == "other") != self.purpose_label.is_some()
            || self
                .purpose_label
                .as_ref()
                .is_some_and(|label| !bounded(label, 128))
            || self.required_capabilities.len() > 64
            || self
                .required_capabilities
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != self.required_capabilities.len()
            || self
                .required_capabilities
                .iter()
                .any(|value| !bounded(value, 128))
            || self.execution_lane != "dedicated"
            || self.required_assurance != "best_effort_personal_alpha"
            || self.native_subagent_policy != "enabled"
        {
            return Err(invalid("unsupported Agent Configuration contract"));
        }
        Ok(())
    }

    fn caller_input(&self, role: &str) -> CallerAgentConfigurationInput {
        CallerAgentConfigurationInput {
            schema_version: self.schema_version,
            selected_profile: self.selected_profile.clone(),
            global_profile_binding_sha256: self.global_profile_binding_sha256.clone(),
            model: self.model.clone(),
            default_effort: self.default_effort.clone(),
            purpose: self.purpose.clone(),
            purpose_label: self.purpose_label.clone(),
            required_capabilities: self.required_capabilities.clone(),
            execution_lane: self.execution_lane.clone(),
            required_assurance: self.required_assurance.clone(),
            native_subagent_policy: self.native_subagent_policy.clone(),
            role: role.to_owned(),
        }
    }

    fn legacy_observation_input(&self, body: &str) -> ExternalAgentConfigurationInput {
        ExternalAgentConfigurationInput {
            schema_version: 2,
            runtime_profile: self.selected_profile.clone(),
            global_profile_binding_sha256: self.global_profile_binding_sha256.clone(),
            model: self.model.clone(),
            default_effort: self.default_effort.clone(),
            purpose: self.purpose.clone(),
            purpose_label: self.purpose_label.clone(),
            required_capabilities: self.required_capabilities.clone(),
            instructions: body.to_owned(),
            execution_lane: self.execution_lane.clone(),
            required_assurance: self.required_assurance.clone(),
            native_subagent_policy: self.native_subagent_policy.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePolicyInput {
    pub role_ref: String,
    pub role_source: RoleSourceReference,
    pub agent_configuration: PolicyAgentConfigurationInput,
    pub max_active_instances: u32,
    pub reuse_policy: String,
    pub allowed_access: Vec<String>,
    pub activation_policy: String,
    pub primary_may_request: bool,
    pub collaboration_source: bool,
    pub collaboration_target: bool,
    pub auto_approve_when_fully_delegated: bool,
    pub display_name: String,
    pub description: String,
}

impl RolePolicyInput {
    fn validate(&self) -> Result<(), MachineError> {
        self.agent_configuration.validate()?;
        if !name(&self.role_ref, 64)
            || !name(&self.role_source.name, 64)
            || !bounded(&self.display_name, 128)
            || !bounded(&self.description, 1024)
            || !(1..=16).contains(&self.max_active_instances)
            || !matches!(
                self.reuse_policy.as_str(),
                "never" | "reuse_idle_compatible" | "reuse_any_compatible"
            )
            || self.allowed_access.is_empty()
            || self.allowed_access.len() > 2
            || self.allowed_access.iter().collect::<BTreeSet<_>>().len()
                != self.allowed_access.len()
            || self
                .allowed_access
                .iter()
                .any(|value| !matches!(value.as_str(), "read_only" | "canonical_workspace_write"))
            || !matches!(
                self.activation_policy.as_str(),
                "on_mail" | "manual" | "keep_resident" | "never"
            )
            || ((self.collaboration_source || self.collaboration_target)
                && !matches!(self.activation_policy.as_str(), "on_mail" | "keep_resident"))
        {
            return Err(invalid("Role admission controls are invalid"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyInput {
    pub schema_version: u32,
    pub policy_name: String,
    pub revision: u64,
    pub approval_policy: String,
    pub max_active_specialists: u32,
    pub roles: Vec<RolePolicyInput>,
}

impl PolicyInput {
    pub fn validate(&self) -> Result<(), MachineError> {
        if self.schema_version != 3
            || !name(&self.policy_name, 64)
            || self.revision == 0
            || !matches!(
                self.approval_policy.as_str(),
                "user_approval_required" | "fully_delegated"
            )
            || !(1..=64).contains(&self.max_active_specialists)
            || self.roles.is_empty()
            || self.roles.len() > 64
        {
            return Err(invalid("unsupported Specialist Policy contract"));
        }
        let mut refs = BTreeSet::new();
        for role in &self.roles {
            role.validate()?;
            if !refs.insert(&role.role_ref) {
                return Err(invalid("role_ref values must be unique"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledRole {
    pub role_ref: String,
    pub role_source: RoleSourceReference,
    pub agent_configuration: PolicyAgentConfigurationInput,
    pub max_active_instances: u32,
    pub reuse_policy: String,
    pub allowed_access: Vec<String>,
    pub activation_policy: String,
    pub primary_may_request: bool,
    pub collaboration_source: bool,
    pub collaboration_target: bool,
    pub auto_approve_when_fully_delegated: bool,
    pub display_name: String,
    pub description: String,
    pub role_capture: RoleCapture,
}

impl InstalledRole {
    fn input(&self) -> RolePolicyInput {
        RolePolicyInput {
            role_ref: self.role_ref.clone(),
            role_source: self.role_source.clone(),
            agent_configuration: self.agent_configuration.clone(),
            max_active_instances: self.max_active_instances,
            reuse_policy: self.reuse_policy.clone(),
            allowed_access: self.allowed_access.clone(),
            activation_policy: self.activation_policy.clone(),
            primary_may_request: self.primary_may_request,
            collaboration_source: self.collaboration_source,
            collaboration_target: self.collaboration_target,
            auto_approve_when_fully_delegated: self.auto_approve_when_fully_delegated,
            display_name: self.display_name.clone(),
            description: self.description.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySnapshot {
    pub schema_version: u32,
    pub policy_name: String,
    pub revision: u64,
    pub approval_policy: String,
    pub max_active_specialists: u32,
    pub roles: Vec<InstalledRole>,
}

impl PolicySnapshot {
    pub fn validate(&self) -> Result<(), MachineError> {
        PolicyInput {
            schema_version: self.schema_version,
            policy_name: self.policy_name.clone(),
            revision: self.revision,
            approval_policy: self.approval_policy.clone(),
            max_active_specialists: self.max_active_specialists,
            roles: self.roles.iter().map(InstalledRole::input).collect(),
        }
        .validate()?;
        let mut captures = BTreeMap::new();
        for role in &self.roles {
            role.role_capture.validate()?;
            if role.role_source.name != role.role_capture.name {
                return Err(invalid(
                    "installed Role identity differs from its source reference",
                ));
            }
            if let Some(previous) = captures.insert(&role.role_capture.name, &role.role_capture)
                && previous != &role.role_capture
            {
                return Err(invalid(
                    "one source name has inconsistent installed captures",
                ));
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, MachineError> {
        self.validate()?;
        Ok(sha256_hex(&canonical_bytes(self)?))
    }
}

/// Bootstrap preparation is internal; its child configurations fit the
/// accepted Agent Configuration v3 wire contract. OrchestrationStore persists
/// these bytes inside the checked private accepted-session receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPolicySession {
    home_root: LosslessPath,
    workspace_id: String,
    canonical_workspace: LosslessPath,
    policy: PolicySnapshot,
    configurations: BTreeMap<String, CallerAgentConfigurationSnapshot>,
    profile_bindings: BTreeMap<String, GlobalProfileBinding>,
}

impl PreparedPolicySession {
    pub(crate) fn validate_for_state_root(&self, state_root: &Path) -> Result<(), MachineError> {
        self.policy.validate()?;
        let home = self.home_root.to_path_buf()?;
        let workspace = self.canonical_workspace.to_path_buf()?;
        let canonical_path = |path: &Path| {
            path.is_absolute()
                && !path.as_os_str().as_bytes().contains(&0)
                && !path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
                && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
        };
        if !canonical_path(&home)
            || home.file_name() != Some(OsStr::new(".dolgorae"))
            || !canonical_path(&workspace)
            || crate::workspace::workspace_id(&workspace) != self.workspace_id
            || home.join("workspaces").join(&self.workspace_id) != state_root
        {
            return Err(invalid(
                "accepted session home and workspace identities differ",
            ));
        }
        let references = self
            .policy
            .roles
            .iter()
            .map(|role| &role.role_ref)
            .collect::<BTreeSet<_>>();
        if self.configurations.keys().collect::<BTreeSet<_>>() != references
            || self.profile_bindings.keys().collect::<BTreeSet<_>>() != references
        {
            return Err(invalid(
                "accepted session child references differ from its policy",
            ));
        }
        let mut common = None;
        for role in &self.policy.roles {
            let configuration = self.child(&role.role_ref)?;
            let binding = self.profile_binding(&role.role_ref)?;
            configuration.validate()?;
            binding.validate_for_recovery()?;
            let input = &role.agent_configuration;
            let mut capabilities = input.required_capabilities.clone();
            capabilities.sort();
            let instruction_home = home.join("home");
            let codex_home = Path::new(&binding.launch_snapshot.canonical_codex_home);
            if configuration.instructions.role != role.role_capture
                || configuration.role_reference.as_deref() != Some(&role.role_ref)
                || configuration.selected_profile != input.selected_profile
                || configuration.selected_profile != binding.selected_name
                || configuration.global_profile_binding_sha256 != binding.digest()?
                || input
                    .global_profile_binding_sha256
                    .as_ref()
                    .is_some_and(|expected| {
                        expected != &configuration.global_profile_binding_sha256
                    })
                || input
                    .model
                    .as_ref()
                    .is_some_and(|model| model != &configuration.model)
                || configuration.default_effort != input.default_effort
                || configuration.purpose.kind.as_str() != input.purpose
                || configuration.purpose.external_label != input.purpose_label
                || configuration.required_capabilities != capabilities
                || common.is_some_and(|previous| previous != &configuration.instructions.common)
                || workspace == instruction_home
                || workspace == codex_home
                || instruction_home == codex_home
                || !canonical_path(codex_home)
            {
                return Err(invalid(
                    "accepted session child differs from its captured policy",
                ));
            }
            common = Some(&configuration.instructions.common);
        }
        Ok(())
    }

    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    #[must_use]
    pub fn policy(&self) -> &PolicySnapshot {
        &self.policy
    }

    pub fn child(&self, role_ref: &str) -> Result<&CallerAgentConfigurationSnapshot, MachineError> {
        self.configurations
            .get(role_ref)
            .ok_or_else(|| invalid("Role is not admitted by the accepted session"))
    }

    pub fn profile_binding(&self, role_ref: &str) -> Result<&GlobalProfileBinding, MachineError> {
        self.profile_bindings
            .get(role_ref)
            .ok_or_else(|| invalid("Role is not admitted by the accepted session"))
    }
}

pub struct MarkdownPolicyRegistry {
    home: DolgoraeHome,
    workspace: WorkspaceView,
    uid: u32,
}

impl MarkdownPolicyRegistry {
    pub fn discover(workspace: Option<&Path>) -> Result<Self, MachineError> {
        let workspace = WorkspaceService::system()?.discover(workspace)?;
        let home = DolgoraeHome::system()?;
        Ok(Self::new(home, workspace))
    }

    #[must_use]
    pub fn new(home: DolgoraeHome, workspace: WorkspaceView) -> Self {
        Self {
            home,
            workspace,
            uid: DarwinSystem.current_uid(),
        }
    }

    /// Read-only validation captures named Roles without common instructions
    /// or source creation, and observes an already-running Profile Server.
    pub fn validate_file(&self, path: &Path) -> Result<PolicySnapshot, MachineError> {
        let input = decode_checked(&read_input(path)?)?;
        self.compile(input)
    }

    pub fn add_file(
        &self,
        path: &Path,
        expected_name: &str,
    ) -> Result<PolicySnapshot, MachineError> {
        let input: PolicyInput = decode_checked(&read_input(path)?)?;
        if input.policy_name != expected_name {
            return Err(invalid("requested policy name differs from its input"));
        }
        let policy = self.compile(input)?;
        self.publish(&policy)?;
        Ok(policy)
    }

    pub fn resolve(&self, name: &str) -> Result<PolicySnapshot, MachineError> {
        let tree = RegistryTree::open(self, false)?;
        tree.read(name).map(|(policy, _)| policy)
    }

    pub fn remove(&self, name: &str) -> Result<(), MachineError> {
        let tree = RegistryTree::open(self, false)?;
        let (_, metadata) = tree.read(name)?;
        tree.verify()?;
        verify_entry(tree.directory(), &filename(name)?, &metadata)?;
        DarwinSystem
            .unlinkat_file(tree.directory(), &filename(name)?)
            .map_err(io_invalid)?;
        tree.directory().sync_all().map_err(io_invalid)
    }

    pub fn prepare_session(&self, name: &str) -> Result<PreparedPolicySession, MachineError> {
        let workspace = self.workspace.canonical_path.to_path_buf()?;
        self.prepare_session_with(name, |role, instructions| {
            let prepared = prepare_caller_agent_configuration_read_only(
                Some(&workspace),
                Some(&role.role_ref),
                role.agent_configuration
                    .caller_input(&role.role_source.name),
                instructions,
            )?;
            if prepared.view.workspace_id != self.workspace.workspace_id
                || prepared.view.canonical_path != self.workspace.canonical_path
            {
                return Err(invalid("prepared child has a different workspace identity"));
            }
            Ok((
                prepared.agent_configuration,
                prepared.global_profile_binding,
            ))
        })
    }

    /// Retry lookup precedes source admission. Accepted receipt readback does
    /// not reopen common, Role, installed policy, or Profile authoring inputs.
    pub fn accept_session(
        &self,
        name: &str,
        store: &mut OrchestrationStore,
        root_run_id: Uuid,
        idempotency_key: &str,
        request_sha256: &str,
    ) -> Result<AcceptedPolicySessionSnapshot, MachineError> {
        self.accept_session_with(
            name,
            store,
            root_run_id,
            idempotency_key,
            request_sha256,
            || self.prepare_session(name),
        )
    }

    fn accept_session_with(
        &self,
        name: &str,
        store: &mut OrchestrationStore,
        root_run_id: Uuid,
        idempotency_key: &str,
        request_sha256: &str,
        prepare: impl FnOnce() -> Result<PreparedPolicySession, MachineError>,
    ) -> Result<AcceptedPolicySessionSnapshot, MachineError> {
        if let Some(accepted) = store.find_accepted_policy_session(
            &self.workspace.workspace_id,
            root_run_id,
            idempotency_key,
            request_sha256,
        )? {
            accepted
                .captures
                .validate_for_state_root(&self.home.workspace_root(&self.workspace.workspace_id))?;
            if accepted.captures.policy().policy_name != name {
                return Err(invalid(
                    "accepted session policy differs from the requested policy",
                ));
            }
            return Ok(accepted);
        }
        store.accept_policy_session(root_run_id, idempotency_key, request_sha256, prepare()?)
    }

    fn compile(&self, input: PolicyInput) -> Result<PolicySnapshot, MachineError> {
        let workspace = self.workspace.canonical_path.to_path_buf()?;
        self.compile_with(input, |role, capture| {
            prepare_external_specialist_read_only(
                Some(&workspace),
                &role.role_ref,
                role.agent_configuration
                    .legacy_observation_input(&capture.body),
            )
            .map(|_| ())
        })
    }

    fn compile_with(
        &self,
        input: PolicyInput,
        mut validate_profile: impl FnMut(&RolePolicyInput, &RoleCapture) -> Result<(), MachineError>,
    ) -> Result<PolicySnapshot, MachineError> {
        input.validate()?;
        let names = input
            .roles
            .iter()
            .map(|role| role.role_source.name.clone())
            .collect::<Vec<_>>();
        let captures = InstructionSourceStore::new(&self.home).capture_roles(&names)?;
        let mut roles = Vec::with_capacity(input.roles.len());
        for role in input.roles {
            let capture = captures
                .get(&role.role_source.name)
                .ok_or_else(|| invalid("selected Role capture is unavailable"))?
                .clone();
            validate_profile(&role, &capture)?;
            roles.push(InstalledRole {
                role_ref: role.role_ref,
                role_source: role.role_source,
                agent_configuration: role.agent_configuration,
                max_active_instances: role.max_active_instances,
                reuse_policy: role.reuse_policy,
                allowed_access: role.allowed_access,
                activation_policy: role.activation_policy,
                primary_may_request: role.primary_may_request,
                collaboration_source: role.collaboration_source,
                collaboration_target: role.collaboration_target,
                auto_approve_when_fully_delegated: role.auto_approve_when_fully_delegated,
                display_name: role.display_name,
                description: role.description,
                role_capture: capture,
            });
        }
        let policy = PolicySnapshot {
            schema_version: 3,
            policy_name: input.policy_name,
            revision: input.revision,
            approval_policy: input.approval_policy,
            max_active_specialists: input.max_active_specialists,
            roles,
        };
        policy.validate()?;
        Ok(policy)
    }

    fn publish(&self, policy: &PolicySnapshot) -> Result<(), MachineError> {
        policy.validate()?;
        let bytes = canonical_bytes(policy)?;
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err(invalid("installed policy exceeds its checked bound"));
        }
        let tree = RegistryTree::open(self, true)?;
        let temporary = OsString::from(format!(".markdown-policy-{}", Uuid::now_v7()));
        let mut file = DarwinSystem
            .createat_private(tree.directory(), &temporary, true)
            .map_err(io_invalid)?;
        let metadata = file.metadata().map_err(io_invalid)?;
        let result = (|| {
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(io_invalid)?;
            file.write_all(&bytes).map_err(io_invalid)?;
            file.sync_all().map_err(io_invalid)?;
            tree.verify()?;
            DarwinSystem
                .renameat_exclusive(
                    tree.directory(),
                    &temporary,
                    &filename(&policy.policy_name)?,
                )
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        invalid("the Specialist Policy already exists")
                    } else {
                        io_invalid(error)
                    }
                })?;
            tree.directory().sync_all().map_err(io_invalid)?;
            tree.verify()
        })();
        if result.is_err() && verify_entry(tree.directory(), &temporary, &metadata).is_ok() {
            let _ = DarwinSystem.unlinkat_file(tree.directory(), &temporary);
        }
        result
    }

    fn prepare_session_with(
        &self,
        name: &str,
        mut prepare: impl FnMut(
            &InstalledRole,
            InstructionSnapshot,
        ) -> Result<
            (CallerAgentConfigurationSnapshot, GlobalProfileBinding),
            MachineError,
        >,
    ) -> Result<PreparedPolicySession, MachineError> {
        let policy = self.resolve(name)?;
        let roles = policy
            .roles
            .iter()
            .map(|role| role.role_capture.clone())
            .collect::<Vec<_>>();
        // Every combined ceiling is checked before any Profile observation.
        let instructions = InstructionSourceStore::new(&self.home).capture_for_roles(&roles)?;
        let mut configurations = BTreeMap::new();
        let mut profile_bindings = BTreeMap::new();
        for (role, instructions) in policy.roles.iter().zip(instructions) {
            let accepted = instructions.clone();
            let (configuration, binding) = prepare(role, instructions)?;
            configuration.validate()?;
            binding.validate_for_recovery()?;
            let input = &role.agent_configuration;
            let mut capabilities = input.required_capabilities.clone();
            capabilities.sort();
            if configuration.instructions != accepted
                || configuration.role_reference.as_deref() != Some(&role.role_ref)
                || configuration.selected_profile != input.selected_profile
                || configuration.selected_profile != binding.selected_name
                || configuration.global_profile_binding_sha256 != binding.digest()?
                || input
                    .global_profile_binding_sha256
                    .as_ref()
                    .is_some_and(|expected| {
                        expected != &configuration.global_profile_binding_sha256
                    })
                || input
                    .model
                    .as_ref()
                    .is_some_and(|model| model != &configuration.model)
                || configuration.default_effort != input.default_effort
                || configuration.purpose.kind.as_str() != input.purpose
                || configuration.purpose.external_label != input.purpose_label
                || configuration.required_capabilities != capabilities
            {
                return Err(invalid(
                    "prepared child differs from the accepted Role capture",
                ));
            }
            configurations.insert(role.role_ref.clone(), configuration);
            profile_bindings.insert(role.role_ref.clone(), binding);
        }
        let prepared = PreparedPolicySession {
            home_root: LosslessPath::from_path(self.home.root()),
            workspace_id: self.workspace.workspace_id.clone(),
            canonical_workspace: self.workspace.canonical_path.clone(),
            policy,
            configurations,
            profile_bindings,
        };
        prepared
            .validate_for_state_root(&self.home.workspace_root(&self.workspace.workspace_id))?;
        Ok(prepared)
    }
}

struct CheckedDirectory {
    file: File,
    metadata: Metadata,
    component: Option<OsString>,
}

struct RegistryTree {
    root: PathBuf,
    uid: u32,
    directories: Vec<CheckedDirectory>,
}

impl RegistryTree {
    fn open(registry: &MarkdownPolicyRegistry, create: bool) -> Result<Self, MachineError> {
        if !is_sha256(&registry.workspace.workspace_id) {
            return Err(invalid("workspace identity is invalid"));
        }
        let root = registry.home.root().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&root)
            .map_err(io_invalid)?;
        let metadata = file.metadata().map_err(io_invalid)?;
        check_directory(&metadata, registry.uid)?;
        let mut tree = Self {
            root,
            uid: registry.uid,
            directories: vec![CheckedDirectory {
                file,
                metadata,
                component: None,
            }],
        };
        for component in [
            "workspaces",
            &registry.workspace.workspace_id,
            "specialist-policies",
        ] {
            let parent = tree.directory();
            if create && component == "specialist-policies" {
                match DarwinSystem.mkdirat_private(parent, OsStr::new(component)) {
                    Ok(()) => parent.sync_all().map_err(io_invalid)?,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(io_invalid(error)),
                }
            }
            let file =
                open_relative_nofollow(parent, OsStr::new(component), true).map_err(io_invalid)?;
            let metadata = file.metadata().map_err(io_invalid)?;
            check_directory(&metadata, registry.uid)?;
            tree.directories.push(CheckedDirectory {
                file,
                metadata,
                component: Some(OsString::from(component)),
            });
        }
        tree.verify()?;
        Ok(tree)
    }

    fn directory(&self) -> &File {
        &self.directories.last().expect("registry descriptor").file
    }

    fn verify(&self) -> Result<(), MachineError> {
        let root = fs::symlink_metadata(&self.root).map_err(io_invalid)?;
        check_directory(&root, self.uid)?;
        if !same_identity(&root, &self.directories[0].metadata) {
            return Err(invalid("policy registry was replaced"));
        }
        for (index, directory) in self.directories.iter().enumerate() {
            let current = directory.file.metadata().map_err(io_invalid)?;
            check_directory(&current, self.uid)?;
            if !same_identity(&current, &directory.metadata) {
                return Err(invalid("policy registry changed during admission"));
            }
            if let Some(component) = &directory.component {
                verify_entry(&self.directories[index - 1].file, component, &current)?;
            }
        }
        Ok(())
    }

    fn read(&self, name: &str) -> Result<(PolicySnapshot, Metadata), MachineError> {
        self.read_with(name, || {})
    }

    fn read_with(
        &self,
        name: &str,
        after_read: impl FnOnce(),
    ) -> Result<(PolicySnapshot, Metadata), MachineError> {
        let component = filename(name)?;
        let mut file =
            open_relative_nofollow(self.directory(), &component, false).map_err(io_invalid)?;
        let before = file.metadata().map_err(io_invalid)?;
        if !before.is_file()
            || before.uid() != self.uid
            || before.mode() & 0o7777 != 0o600
            || before.len() > MAX_POLICY_BYTES
        {
            return Err(invalid("installed policy file is unsafe"));
        }
        let bytes = read_bounded(&mut file)?;
        after_read();
        let after = file.metadata().map_err(io_invalid)?;
        if !stable_file(&before, &after) || after.len() != bytes.len() as u64 {
            return Err(invalid("installed policy changed during admission"));
        }
        verify_entry(self.directory(), &component, &after)?;
        self.verify()?;
        let policy: PolicySnapshot = decode_checked(&bytes)?;
        policy.validate()?;
        if policy.policy_name != name {
            return Err(invalid(
                "installed policy identity differs from its filename",
            ));
        }
        Ok((policy, after))
    }
}

fn check_directory(metadata: &Metadata, uid: u32) -> Result<(), MachineError> {
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
        return Err(invalid("policy registry directory is unsafe"));
    }
    Ok(())
}

fn same_identity(left: &Metadata, right: &Metadata) -> bool {
    (left.dev(), left.ino(), left.uid(), left.mode())
        == (right.dev(), right.ino(), right.uid(), right.mode())
}

fn stable_file(left: &Metadata, right: &Metadata) -> bool {
    same_identity(left, right)
        && (
            left.len(),
            left.mtime(),
            left.mtime_nsec(),
            left.ctime(),
            left.ctime_nsec(),
        ) == (
            right.len(),
            right.mtime(),
            right.mtime_nsec(),
            right.ctime(),
            right.ctime_nsec(),
        )
}

fn verify_entry(directory: &File, name: &OsStr, metadata: &Metadata) -> Result<(), MachineError> {
    let entry = DarwinSystem
        .statat_nofollow(directory, name)
        .map_err(io_invalid)?;
    if (entry.device, entry.inode, entry.uid, entry.mode)
        != (
            metadata.dev(),
            metadata.ino(),
            metadata.uid(),
            metadata.mode(),
        )
    {
        return Err(invalid("policy registry entry was replaced"));
    }
    Ok(())
}

fn filename(value: &str) -> Result<OsString, MachineError> {
    if !name(value, 64) {
        return Err(invalid("policy name is invalid"));
    }
    Ok(OsString::from(format!("{value}.json")))
}

fn name(value: &str, maximum: usize) -> bool {
    bounded(value, maximum)
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn bounded(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.contains('\0')
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn deserialize_present_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

fn read_input(path: &Path) -> Result<Vec<u8>, MachineError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_invalid)?;
    let before = file.metadata().map_err(io_invalid)?;
    if !before.is_file() || before.len() > MAX_POLICY_BYTES {
        return Err(invalid("policy input must be a bounded regular file"));
    }
    let bytes = read_bounded(&mut file)?;
    let after = file.metadata().map_err(io_invalid)?;
    let path_metadata = fs::symlink_metadata(path).map_err(io_invalid)?;
    if !stable_file(&before, &after)
        || !same_identity(&after, &path_metadata)
        || after.len() != bytes.len() as u64
    {
        return Err(invalid("policy input changed during admission"));
    }
    Ok(bytes)
}

fn read_bounded(file: &mut File) -> Result<Vec<u8>, MachineError> {
    let mut bytes = Vec::new();
    file.take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_invalid)?;
    if bytes.len() as u64 > MAX_POLICY_BYTES {
        return Err(invalid("policy exceeds its checked bound"));
    }
    Ok(bytes)
}

fn decode_checked<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, MachineError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("policy is not UTF-8"))?;
    let canonical = canonicalize(&parse(text).map_err(|_| invalid("policy JSON is invalid"))?)
        .map_err(|_| invalid("policy JSON is invalid"))?;
    serde_json::from_slice(&canonical)
        .map_err(|_| invalid("policy does not match its checked contract"))
}

fn canonical_bytes(value: &impl Serialize) -> Result<Vec<u8>, MachineError> {
    let text = serde_json::to_string(value).map_err(|_| invalid("policy cannot be serialized"))?;
    canonicalize(&parse(&text).map_err(|_| invalid("policy cannot be serialized"))?)
        .map_err(|_| invalid("policy cannot be serialized"))
}

fn io_invalid(_error: std::io::Error) -> MachineError {
    invalid("policy storage is missing, unsafe, or changed during admission")
}

fn invalid(reason: &str) -> MachineError {
    MachineError::config_invalid("specialist-policy", reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Assurance, ExecutionLane, Purpose, PurposeKind};
    use crate::profile::{CompatibilityVerdict, ExecutableIdentity, ProfileSnapshot};
    use crate::workspace::{LosslessPath, NativeSubagents, RuntimeProfile, WorkspaceMode};
    use std::os::unix::fs::symlink;

    struct Fixture {
        root: PathBuf,
        registry: MarkdownPolicyRegistry,
    }

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("dolgorae-markdown-policy-{}", Uuid::now_v7()));
            let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
            let id = crate::workspace::workspace_id(&root.join("workspace"));
            for path in [
                root.clone(),
                home.root().to_path_buf(),
                home.root().join("roles"),
                home.root().join("workspaces"),
                home.workspace_root(&id),
                root.join("workspace"),
            ] {
                fs::create_dir(&path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let workspace = WorkspaceView {
                workspace_id: id,
                canonical_path: LosslessPath::from_path(&root.join("workspace")),
                mode: WorkspaceMode::NonGit,
                created: false,
            };
            Self {
                root,
                registry: MarkdownPolicyRegistry::new(home, workspace),
            }
        }

        fn role(&self, name: &str, body: &str) -> PathBuf {
            let directory = self.registry.home.root().join("roles").join(name);
            if !directory.exists() {
                fs::create_dir(&directory).unwrap();
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let path = directory.join("AGENTS.md");
            write_private(&path, body.as_bytes());
            path
        }

        fn common(&self, body: &str) -> PathBuf {
            let directory = self.registry.home.root().join("home");
            if !directory.exists() {
                fs::create_dir(&directory).unwrap();
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let path = directory.join("AGENTS.md");
            write_private(&path, body.as_bytes());
            path
        }

        fn registry_root(&self) -> PathBuf {
            self.registry
                .home
                .workspace_root(&self.registry.workspace.workspace_id)
                .join("specialist-policies")
        }

        fn policy_path(&self) -> PathBuf {
            self.registry_root().join("brokered-review.json")
        }

        fn install(&self, input: PolicyInput) -> PolicySnapshot {
            let policy = self.registry.compile_with(input, |_, _| Ok(())).unwrap();
            self.registry.publish(&policy).unwrap();
            policy
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn input() -> PolicyInput {
        let mut input: PolicyInput = decode_checked(include_bytes!(
            "../docs/protocol/examples/workspace-policy-input-v3.valid.json"
        ))
        .unwrap();
        input.roles[0]
            .agent_configuration
            .global_profile_binding_sha256 = None;
        input
    }

    fn two_roles() -> PolicyInput {
        let mut policy = input();
        let mut second = policy.roles[0].clone();
        second.role_ref = "second-reviewer".to_owned();
        policy.roles.push(second);
        policy
    }

    fn binding() -> GlobalProfileBinding {
        let definition = RuntimeProfile {
            argv: vec!["/isolated/codex".to_owned()],
            codex_home: "/isolated/profile-home".to_owned(),
            environment: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("LANG".to_owned(), "C".to_owned()),
                ("LC_ALL".to_owned(), "C".to_owned()),
            ]),
            native_subagents: NativeSubagents::Enabled,
        };
        let launch_snapshot = ProfileSnapshot {
            schema_version: 1,
            profile_name: "reviewer".to_owned(),
            canonical_codex_home: definition.codex_home.clone(),
            normalized_argv: vec![
                "/isolated/codex".to_owned(),
                "--strict-config".to_owned(),
                "--enable".to_owned(),
                "multi_agent".to_owned(),
            ],
            launch_cwd_policy: "profile_state_directory_v1".to_owned(),
            derived_launch_cwd: "/isolated/server-state".to_owned(),
            sanitized_environment: definition.environment.clone(),
            enabled_features: vec!["multi_agent".to_owned()],
            disabled_features: Vec::new(),
            process_static_configuration: BTreeMap::new(),
            initial_configuration_observation: BTreeMap::new(),
            executable_identity: ExecutableIdentity {
                resolved_path: "/isolated/codex".to_owned(),
                device: 1,
                inode: 1,
                sha256: "1".repeat(64),
            },
            codex_version: "0.159.3".to_owned(),
            schema_bundle_sha256: "2".repeat(64),
            compatibility_manifest_sha256: "3".repeat(64),
            launch_contract_sha256: "4".repeat(64),
            compatibility_verdict: CompatibilityVerdict::Tested,
            server_key: "5".repeat(64),
        };
        GlobalProfileBinding {
            schema_version: 2,
            selected_name: "reviewer".to_owned(),
            definition_sha256: sha256_hex(&canonical_bytes(&definition).unwrap()),
            definition,
            launch_snapshot_sha256: sha256_hex(&canonical_bytes(&launch_snapshot).unwrap()),
            server_key: launch_snapshot.server_key.clone(),
            launch_snapshot,
        }
    }

    fn prepare_child(
        role: &InstalledRole,
        instructions: InstructionSnapshot,
    ) -> Result<(CallerAgentConfigurationSnapshot, GlobalProfileBinding), MachineError> {
        let binding = binding();
        Ok((
            CallerAgentConfigurationSnapshot {
                schema_version: 3,
                selected_profile: role.agent_configuration.selected_profile.clone(),
                global_profile_binding_sha256: binding.digest()?,
                model: role
                    .agent_configuration
                    .model
                    .clone()
                    .unwrap_or_else(|| "gpt-5.6".to_owned()),
                default_effort: role.agent_configuration.default_effort.clone(),
                purpose: Purpose {
                    kind: PurposeKind::Review,
                    external_label: None,
                },
                required_capabilities: role.agent_configuration.required_capabilities.clone(),
                role_reference: Some(role.role_ref.clone()),
                instructions,
                execution_lane: ExecutionLane::Dedicated,
                required_assurance: Assurance::BestEffortPersonalAlpha,
                native_subagent_policy: "enabled".to_owned(),
            },
            binding,
        ))
    }

    #[test]
    fn schema_examples_and_required_nullable_fields_match_the_successor() {
        let policy: PolicyInput = decode_checked(include_bytes!(
            "../docs/protocol/examples/workspace-policy-input-v3.valid.json"
        ))
        .unwrap();
        policy.validate().unwrap();
        let snapshot: PolicySnapshot = decode_checked(include_bytes!(
            "../docs/protocol/examples/workspace-policy-snapshot-v3.valid.json"
        ))
        .unwrap();
        snapshot.validate().unwrap();
        assert!(
            decode_checked::<PolicyInput>(include_bytes!(
                "../docs/protocol/examples/workspace-policy-input-v3.invalid-project-fallback.json"
            ))
            .is_err()
        );
        let mut value = serde_json::to_value(input()).unwrap();
        let configuration = value["roles"][0]["agent_configuration"]
            .as_object_mut()
            .unwrap();
        assert!(!configuration.contains_key("global_profile_binding_sha256"));
        configuration.remove("model");
        assert!(serde_json::from_value::<PolicyInput>(value).is_err());
        let mut value = serde_json::to_value(input()).unwrap();
        value["roles"][0]["agent_configuration"]
            .as_object_mut()
            .unwrap()
            .remove("purpose_label");
        assert!(serde_json::from_value::<PolicyInput>(value).is_err());
        let mut value = serde_json::to_value(input()).unwrap();
        value["roles"][0]["agent_configuration"]["global_profile_binding_sha256"] =
            serde_json::Value::Null;
        assert!(serde_json::from_value::<PolicyInput>(value).is_err());
        assert!(
            decode_checked::<PolicyInput>(br#"{"schema_version":3,"schema_version":3}"#).is_err()
        );
    }

    #[test]
    fn successor_controls_reject_legacy_access_and_invalid_combinations() {
        let mutations: &[fn(&mut PolicyInput)] = &[
            |p| p.schema_version = 2,
            |p| p.roles.push(p.roles[0].clone()),
            |p| p.roles[0].allowed_access = vec!["isolated_write".to_owned()],
            |p| p.roles[0].allowed_access.push("read_only".to_owned()),
            |p| p.roles[0].agent_configuration.execution_lane = "shared_readonly".to_owned(),
            |p| {
                p.roles[0].collaboration_source = true;
                p.roles[0].activation_policy = "manual".to_owned();
            },
            |p| p.roles[0].display_name = "é".repeat(65),
            |p| p.roles[0].description.push('\0'),
            |p| p.roles[0].agent_configuration.purpose = "other".to_owned(),
            |p| p.roles[0].agent_configuration.global_profile_binding_sha256 = Some("a".repeat(63)),
            |p| p.roles[0].max_active_instances = 17,
            |p| p.max_active_specialists = 0,
        ];
        for mutation in mutations {
            let mut policy = input();
            mutation(&mut policy);
            assert!(policy.validate().is_err());
        }
        let mut policy = input();
        policy.approval_policy = "fully_delegated".to_owned();
        policy.roles[0].auto_approve_when_fully_delegated = false;
        // The bit restricts hiring; it does not invalidate the policy itself.
        policy.validate().unwrap();
    }

    #[test]
    fn installation_captures_each_role_once_without_reading_common() {
        let fixture = Fixture::new();
        let body = "  e\u{301}\r\n역할\n\n";
        let path = fixture.role("reviewer", body);
        let mut observations = 0;
        let policy = fixture
            .registry
            .compile_with(two_roles(), |_, capture| {
                assert_eq!(capture.body, body);
                if observations == 0 {
                    fs::remove_file(&path).unwrap();
                }
                observations += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(observations, 2);
        assert_eq!(policy.roles[0].role_capture, policy.roles[1].role_capture);
        assert!(!fixture.registry.home.root().join("home").exists());
        fixture.registry.publish(&policy).unwrap();
        assert_eq!(
            fixture.registry.resolve(&policy.policy_name).unwrap(),
            policy
        );
        fixture.role("reviewer", "Changed live source");
        assert_eq!(
            fixture.registry.resolve(&policy.policy_name).unwrap(),
            policy
        );
        assert_eq!(
            fs::metadata(fixture.policy_path()).unwrap().mode() & 0o7777,
            0o600
        );
    }

    #[test]
    fn validation_failure_does_not_publish_or_create_sources() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .registry
                .compile_with(input(), |_, _| Ok(()))
                .is_err()
        );
        assert!(!fixture.registry_root().exists());
        assert!(!fixture.registry.home.root().join("roles/reviewer").exists());
        fixture.role("reviewer", "Accepted Role");
        let error = fixture
            .registry
            .compile_with(input(), |_, _| Err(invalid("Profile rejected")))
            .unwrap_err();
        assert_eq!(error.code, "CONFIG_INVALID");
        assert!(!fixture.registry_root().exists());
        assert!(!fixture.registry.home.root().join("home").exists());
    }

    #[test]
    fn bootstrap_captures_common_once_and_children_keep_accepted_bytes() {
        let fixture = Fixture::new();
        let role_path = fixture.role("reviewer", "Role at installation\r\n");
        let policy = fixture.install(two_roles());
        fs::remove_file(&role_path).unwrap();
        let common_path = fixture.common("Common at bootstrap\n");
        let mut prepared = 0;
        let session = fixture
            .registry
            .prepare_session_with(&policy.policy_name, |role, instructions| {
                if prepared == 0 {
                    write_private(&common_path, b"Later common");
                }
                prepared += 1;
                prepare_child(role, instructions)
            })
            .unwrap();
        assert_eq!(prepared, 2);
        assert_eq!(session.policy(), &policy);
        let child = session.child("reviewer").unwrap();
        assert_eq!(child.instructions.common.body, "Common at bootstrap\n");
        assert_eq!(child.instructions.role.body, "Role at installation\r\n");
        assert_eq!(
            session
                .child("second-reviewer")
                .unwrap()
                .instructions
                .common,
            child.instructions.common
        );
        let before = serde_json::to_vec(child).unwrap();
        fs::remove_dir_all(fixture.registry.home.root().join("home")).unwrap();
        fs::remove_dir_all(fixture.registry.home.root().join("roles")).unwrap();
        fs::remove_file(fixture.policy_path()).unwrap();
        assert_eq!(
            serde_json::to_vec(session.child("reviewer").unwrap()).unwrap(),
            before
        );
        session
            .profile_binding("reviewer")
            .unwrap()
            .validate_for_recovery()
            .unwrap();
        assert!(session.child("unadmitted").is_err());
    }

    #[test]
    fn combined_ceiling_is_checked_for_all_roles_before_profile_observation() {
        let fixture = Fixture::new();
        fixture.role("reviewer", "r");
        fixture.role("large", &"r".repeat(65_535));
        let mut policy = two_roles();
        policy.roles[1].role_source.name = "large".to_owned();
        fixture.install(policy);
        fixture.common("cc");
        let mut observed = false;
        let error = fixture
            .registry
            .prepare_session_with("brokered-review", |role, instructions| {
                observed = true;
                prepare_child(role, instructions)
            })
            .unwrap_err();
        assert_eq!(error.code, "INSTRUCTION_SOURCE_INVALID");
        assert!(!observed);
    }

    #[test]
    fn maximally_escaped_captures_survive_the_registry_reader_bound() {
        let fixture = Fixture::new();
        fixture.role("reviewer", &"\u{0001}".repeat(65_535));
        let mut policy = input();
        for index in 1..8 {
            let mut role = policy.roles[0].clone();
            role.role_ref = format!("reviewer-{index}");
            policy.roles.push(role);
        }
        let policy = fixture.install(policy);
        assert!(fs::metadata(fixture.policy_path()).unwrap().len() > 1_048_576);
        assert_eq!(fixture.registry.resolve("brokered-review").unwrap(), policy);
    }

    #[test]
    fn exclusive_publication_preserves_legacy_collisions_and_removal_needs_no_sources() {
        let fixture = Fixture::new();
        fixture.role("reviewer", "Role");
        let policy = fixture
            .registry
            .compile_with(input(), |_, _| Ok(()))
            .unwrap();
        fs::create_dir(fixture.registry_root()).unwrap();
        fs::set_permissions(fixture.registry_root(), fs::Permissions::from_mode(0o700)).unwrap();
        let legacy = b"{\"schema_version\":2,\"legacy\":true}\n";
        write_private(&fixture.policy_path(), legacy);
        assert!(fixture.registry.publish(&policy).is_err());
        assert_eq!(fs::read(fixture.policy_path()).unwrap(), legacy);
        assert_eq!(fs::read_dir(fixture.registry_root()).unwrap().count(), 1);
        fs::remove_file(fixture.policy_path()).unwrap();
        fixture.registry.publish(&policy).unwrap();
        fs::remove_dir_all(fixture.registry.home.root().join("roles")).unwrap();
        fixture.registry.remove("brokered-review").unwrap();
        assert!(!fixture.policy_path().exists());
    }

    #[test]
    fn registry_rejects_symlinks_unsafe_modes_and_wrong_owners() {
        let fixture = Fixture::new();
        fixture.role("reviewer", "Role");
        fixture.install(input());
        fs::set_permissions(fixture.policy_path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fixture.registry.resolve("brokered-review").is_err());
        fs::set_permissions(fixture.policy_path(), fs::Permissions::from_mode(0o600)).unwrap();
        let saved = fixture.root.join("saved-policy");
        fs::rename(fixture.policy_path(), &saved).unwrap();
        symlink(&saved, fixture.policy_path()).unwrap();
        assert!(fixture.registry.resolve("brokered-review").is_err());
        fs::remove_file(fixture.policy_path()).unwrap();
        fs::rename(&saved, fixture.policy_path()).unwrap();
        let registry_path = fixture.registry_root();
        let moved = registry_path.with_extension("saved");
        fs::rename(&registry_path, &moved).unwrap();
        symlink(&moved, &registry_path).unwrap();
        assert!(fixture.registry.resolve("brokered-review").is_err());
        fs::remove_file(&registry_path).unwrap();
        fs::rename(&moved, &registry_path).unwrap();
        let mut wrong_owner = MarkdownPolicyRegistry::new(
            fixture.registry.home.clone(),
            fixture.registry.workspace.clone(),
        );
        wrong_owner.uid = wrong_owner.uid.wrapping_add(1);
        assert!(wrong_owner.resolve("brokered-review").is_err());
    }

    #[test]
    fn registry_admission_rejects_replacement_in_place_edits_and_directory_swaps() {
        for mutation in 0..3 {
            let fixture = Fixture::new();
            fixture.role("reviewer", "Role");
            fixture.install(input());
            let tree = RegistryTree::open(&fixture.registry, false).unwrap();
            let result = tree.read_with("brokered-review", || match mutation {
                0 => {
                    let bytes = fs::read(fixture.policy_path()).unwrap();
                    let replacement = fixture.root.join("replacement");
                    write_private(&replacement, &bytes);
                    fs::rename(replacement, fixture.policy_path()).unwrap();
                }
                1 => write_private(&fixture.policy_path(), b"Changed"),
                _ => {
                    fs::rename(
                        fixture.registry_root(),
                        fixture.registry_root().with_extension("old"),
                    )
                    .unwrap();
                    fs::create_dir(fixture.registry_root()).unwrap();
                    fs::set_permissions(fixture.registry_root(), fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
            });
            assert!(result.is_err(), "mutation {mutation}");
        }
    }

    #[test]
    fn installed_capture_tampering_and_inconsistent_duplicate_sources_fail_closed() {
        let fixture = Fixture::new();
        fixture.role("reviewer", "Role");
        let policy = fixture
            .registry
            .compile_with(two_roles(), |_, _| Ok(()))
            .unwrap();
        for field in 0..4 {
            let mut changed = policy.clone();
            match field {
                0 => changed.roles[0].role_capture.body.push('!'),
                1 => changed.roles[0].role_capture.byte_length += 1,
                2 => changed.roles[0].role_capture.sha256 = "0".repeat(64),
                _ => changed.roles[0].role_capture.name = "different".to_owned(),
            }
            assert!(changed.validate().is_err());
        }
        let mut changed = policy.clone();
        changed.roles[0].role_capture.body = "Other".to_owned();
        changed.roles[0].role_capture.byte_length = 5;
        changed.roles[0].role_capture.sha256 = sha256_hex(b"Other");
        assert!(changed.validate().is_err());
        fixture.registry.publish(&policy).unwrap();
        fixture.common("Common");
        assert!(
            fixture
                .registry
                .prepare_session_with("brokered-review", |role, instructions| {
                    let (mut configuration, binding) = prepare_child(role, instructions)?;
                    configuration.role_reference = None;
                    Ok((configuration, binding))
                })
                .is_err()
        );
    }

    #[test]
    fn durable_acceptance_reopens_and_retries_without_authoring_inputs() {
        let fixture = Fixture::new();
        let role_path = fixture.role("reviewer", "Role at installation\r\n");
        let policy = fixture.install(two_roles());
        let common_path = fixture.common("Common at acceptance\n");
        let state_root = fixture
            .registry
            .home
            .workspace_root(&fixture.registry.workspace.workspace_id);
        let root_run_id = Uuid::now_v7();
        let mut store = OrchestrationStore::open(&state_root).unwrap();
        let accepted = fixture
            .registry
            .accept_session_with(
                &policy.policy_name,
                &mut store,
                root_run_id,
                "durable-bootstrap",
                &"6".repeat(64),
                || {
                    fixture
                        .registry
                        .prepare_session_with(&policy.policy_name, prepare_child)
                },
            )
            .unwrap();
        let expected_child =
            serde_json::to_vec(accepted.captures.child("reviewer").unwrap()).unwrap();
        let expected_binding =
            serde_json::to_vec(accepted.captures.profile_binding("reviewer").unwrap()).unwrap();
        let expected_receipt = canonical_bytes(&accepted).unwrap();
        drop(accepted);
        drop(store);
        write_private(&role_path, b"Changed Role");
        write_private(&common_path, b"Changed common");
        write_private(&fixture.policy_path(), b"Changed installed policy");
        let observer = OrchestrationStore::open_observer(&state_root).unwrap();
        let recovered = observer.accepted_policy_session(root_run_id).unwrap();
        assert_eq!(canonical_bytes(&recovered).unwrap(), expected_receipt);
        drop(recovered);
        drop(observer);
        fs::remove_dir_all(fixture.registry.home.root().join("roles")).unwrap();
        fs::remove_dir_all(fixture.registry.home.root().join("home")).unwrap();
        fs::remove_file(fixture.policy_path()).unwrap();
        let registry = MarkdownPolicyRegistry::new(
            fixture.registry.home.clone(),
            fixture.registry.workspace.clone(),
        );
        let mut reopened = OrchestrationStore::open(&state_root).unwrap();
        // This uses the production retry wrapper. Source/Profile preparation
        // would fail because all authoring inputs are now gone.
        let replay = registry
            .accept_session(
                &policy.policy_name,
                &mut reopened,
                root_run_id,
                "durable-bootstrap",
                &"6".repeat(64),
            )
            .unwrap();
        assert_eq!(canonical_bytes(&replay).unwrap(), expected_receipt);
        assert_eq!(
            serde_json::to_vec(replay.captures.child("reviewer").unwrap()).unwrap(),
            expected_child
        );
        assert_eq!(
            serde_json::to_vec(replay.captures.profile_binding("reviewer").unwrap()).unwrap(),
            expected_binding
        );
        assert!(!state_root.join("runs").exists());
        let database = crate::engagement::EngagementStore::workspace_database_path(&state_root);
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM orchestrated_sessions", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
    }

    #[test]
    fn durable_acceptance_rejects_conflicting_inputs_and_preserves_the_original() {
        let fixture = Fixture::new();
        fixture.role("reviewer", "Role");
        fixture.install(input());
        let common = fixture.common("Common");
        let state_root = fixture
            .registry
            .home
            .workspace_root(&fixture.registry.workspace.workspace_id);
        let root_run_id = Uuid::now_v7();
        let original = fixture
            .registry
            .prepare_session_with("brokered-review", prepare_child)
            .unwrap();
        let mut store = OrchestrationStore::open(&state_root).unwrap();
        let accepted = store
            .accept_policy_session(root_run_id, "same", &"6".repeat(64), original.clone())
            .unwrap();
        assert_eq!(
            store
                .accept_policy_session(root_run_id, "same", &"6".repeat(64), original.clone())
                .unwrap(),
            accepted
        );
        for (root, key, request) in [
            (Uuid::now_v7(), "same", "6".repeat(64)),
            (root_run_id, "same", "7".repeat(64)),
            (root_run_id, "different", "6".repeat(64)),
        ] {
            assert!(
                store
                    .accept_policy_session(root, key, &request, original.clone())
                    .is_err()
            );
        }
        write_private(&common, b"Different accepted source");
        let changed = fixture
            .registry
            .prepare_session_with("brokered-review", prepare_child)
            .unwrap();
        assert_eq!(
            store
                .accept_policy_session(root_run_id, "same", &"6".repeat(64), changed)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        assert_eq!(
            store.accepted_policy_session(root_run_id).unwrap(),
            accepted
        );
        assert!(
            fixture
                .registry
                .accept_session(
                    "different-policy",
                    &mut store,
                    root_run_id,
                    "same",
                    &"6".repeat(64)
                )
                .is_err()
        );
    }

    #[test]
    fn durable_receipt_readback_rejects_digest_identity_and_semantic_tampering() {
        for mutation in 0..16 {
            let fixture = Fixture::new();
            fixture.role("reviewer", "Role");
            fixture.install(two_roles());
            fixture.common("Common");
            let state_root = fixture
                .registry
                .home
                .workspace_root(&fixture.registry.workspace.workspace_id);
            let mut store = OrchestrationStore::open(&state_root).unwrap();
            let root = Uuid::now_v7();
            let captures = fixture
                .registry
                .prepare_session_with("brokered-review", prepare_child)
                .unwrap();
            let accepted = store
                .accept_policy_session(root, "tamper", &"6".repeat(64), captures)
                .unwrap();
            drop(store);
            let database = crate::engagement::EngagementStore::workspace_database_path(&state_root);
            let connection = rusqlite::Connection::open(database).unwrap();
            let mut value = serde_json::to_value(&accepted).unwrap();
            match mutation {
                0 => value["schema"] = "dolgorae.accepted-policy-session/v2".into(),
                1 => value["root_run_id"] = Uuid::now_v7().to_string().into(),
                2 => value["captures"]["home_root"] = "/different/home/.dolgorae".into(),
                3 => value["captures"]["canonical_workspace"] = "/different/workspace".into(),
                4 => {
                    value["captures"]["configurations"]
                        .as_object_mut()
                        .unwrap()
                        .remove("reviewer");
                }
                5 => {
                    value["captures"]["configurations"]["reviewer"]["global_profile_binding_sha256"] =
                        "0".repeat(64).into()
                }
                6 => {
                    value["captures"]["profile_bindings"]["reviewer"]["definition"]["codex_home"] =
                        "/changed/profile".into()
                }
                7 => {
                    let common = crate::instructions::SourceCapture {
                        body: "Other common".to_owned(),
                        byte_length: 12,
                        sha256: sha256_hex(b"Other common"),
                    };
                    let snapshot = InstructionSnapshot::compose(
                        common,
                        accepted
                            .captures
                            .child("reviewer")
                            .unwrap()
                            .instructions
                            .role
                            .clone(),
                    )
                    .unwrap();
                    value["captures"]["configurations"]["reviewer"]["instructions"] =
                        serde_json::to_value(snapshot).unwrap();
                }
                8 => {
                    value["captures"]["configurations"]["reviewer"]["role_reference"] =
                        "second-reviewer".into()
                }
                9 => {
                    value["captures"]["policy"]["roles"][0]["role_capture"]["body"] =
                        "Other Role".into()
                }
                _ => {}
            }
            let mut bytes = canonical_bytes(&value).unwrap();
            if mutation == 10 {
                let text = String::from_utf8(bytes).unwrap();
                bytes = format!(
                    "{{\"schema\":\"dolgorae.accepted-policy-session/v1\",{}",
                    &text[1..]
                )
                .into_bytes();
            }
            let recorded = if mutation == 11 {
                "0".repeat(64)
            } else {
                sha256_hex(&bytes)
            };
            connection.execute("UPDATE caller_policy_session_acceptances SET receipt_json=?1,receipt_sha256=?2 WHERE session_id=?3",
                rusqlite::params![String::from_utf8(bytes).unwrap(), recorded, root.to_string()]).unwrap();
            if mutation >= 12 {
                let (column, changed) = match mutation {
                    12 => ("root_run_id", Uuid::now_v7().to_string()),
                    13 => ("workspace_id", "0".repeat(64)),
                    14 => ("idempotency_key", "other-key".to_owned()),
                    _ => ("request_sha256", "0".repeat(64)),
                };
                connection.execute(&format!("UPDATE caller_policy_session_acceptances SET {column}=?1 WHERE session_id=?2"),
                    rusqlite::params![changed, root.to_string()]).unwrap();
            }
            drop(connection);
            let observer = OrchestrationStore::open_observer(&state_root).unwrap();
            assert_eq!(
                observer.accepted_policy_session(root).unwrap_err().code,
                "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn durable_acceptance_faults_replay_one_identity_before_and_after_commit() {
        struct Fault(
            crate::orchestration::OrchestrationBarrier,
            std::sync::Mutex<bool>,
        );
        impl crate::orchestration::OrchestrationFaultInjector for Fault {
            fn check(
                &self,
                barrier: crate::orchestration::OrchestrationBarrier,
            ) -> Result<(), MachineError> {
                let mut fired = self.1.lock().unwrap();
                if barrier == self.0 && !*fired {
                    *fired = true;
                    return Err(invalid("injected acceptance fault"));
                }
                Ok(())
            }
        }
        use crate::orchestration::OrchestrationBarrier::{AfterSessionCommit, BeforeSessionCommit};
        for barrier in [BeforeSessionCommit, AfterSessionCommit] {
            let fixture = Fixture::new();
            fixture.role("reviewer", "Role");
            fixture.install(input());
            fixture.common("Common");
            let state_root = fixture
                .registry
                .home
                .workspace_root(&fixture.registry.workspace.workspace_id);
            let root = Uuid::now_v7();
            let captures = fixture
                .registry
                .prepare_session_with("brokered-review", prepare_child)
                .unwrap();
            let mut store = OrchestrationStore::open_with_faults(
                &state_root,
                std::sync::Arc::new(Fault(barrier, std::sync::Mutex::new(false))),
            )
            .unwrap();
            let failure = store
                .accept_policy_session(root, "fault", &"6".repeat(64), captures.clone())
                .unwrap_err();
            if barrier == AfterSessionCommit {
                assert_eq!(failure.code, "INTERRUPTED_UNKNOWN");
            }
            drop(store);
            let observer = OrchestrationStore::open_observer(&state_root).unwrap();
            assert_eq!(
                observer
                    .find_accepted_policy_session(
                        &fixture.registry.workspace.workspace_id,
                        root,
                        "fault",
                        &"6".repeat(64)
                    )
                    .unwrap()
                    .is_some(),
                barrier == AfterSessionCommit
            );
            drop(observer);
            let mut store = OrchestrationStore::open(&state_root).unwrap();
            let accepted = store
                .accept_policy_session(root, "fault", &"6".repeat(64), captures)
                .unwrap();
            assert_eq!(accepted.root_run_id, root);
            drop(store);
            let connection = rusqlite::Connection::open(
                crate::engagement::EngagementStore::workspace_database_path(&state_root),
            )
            .unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM caller_policy_session_acceptances",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
        }
    }

    #[test]
    fn policy_labels_cannot_exceed_the_accepted_child_purpose_bound() {
        let mut policy = input();
        policy.roles[0].agent_configuration.purpose = "other".to_owned();
        policy.roles[0].agent_configuration.purpose_label = Some("x".repeat(128));
        policy.validate().unwrap();
        policy.roles[0].agent_configuration.purpose_label = Some("x".repeat(129));
        assert!(policy.validate().is_err());
    }
}
