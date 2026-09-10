//! Machine-local Specialist Role and installed-policy authority.

use crate::darwin::DarwinSystem;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::run::AgentConfigurationSnapshot;
use crate::semantic::{
    ExternalAgentConfigurationInput, prepare_external_specialist,
    prepare_external_specialist_read_only,
};
use crate::workspace::{
    SystemWorkspacePlatform, WorkspaceService, WorkspaceView, atomic_create, create_directory,
    open_relative_nofollow, sync_directory, verify_secure_directory,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

const MAX_CONFIG_BYTES: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyOperation {
    Add,
    List,
    Show,
    Validate,
    Remove,
}

pub fn execute(
    operation: PolicyOperation,
    args: &[std::ffi::OsString],
) -> Result<serde_json::Value, MachineError> {
    let workspace = option_path(args, "--workspace")?;
    let registry = SpecialistPolicyRegistry::discover(workspace.as_deref())?;
    match operation {
        PolicyOperation::Validate => {
            let path = required_path(args, "--file")?;
            let policy = registry.validate_file(&path)?;
            serde_json::to_value(policy).map_err(internal)
        }
        PolicyOperation::Add => {
            let expected = positional(args, 0)?;
            let path = required_path(args, "--file")?;
            let policy = registry.add_file(&path, &expected)?;
            serde_json::to_value(policy).map_err(internal)
        }
        PolicyOperation::List => Ok(serde_json::json!({"items": registry.list()?})),
        PolicyOperation::Show => {
            let policy = registry.resolve(&positional(args, 0)?)?;
            serde_json::to_value(policy).map_err(internal)
        }
        PolicyOperation::Remove => {
            let name = positional(args, 0)?;
            registry.remove(&name)?;
            Ok(serde_json::json!({"deleted": true}))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSource {
    pub schema_version: u32,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub instructions: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleSourceScope {
    Common,
    Project,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSourceReference {
    pub scope: RoleSourceScope,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAgentConfigurationInput {
    pub schema_version: u32,
    pub selected_profile: String,
    #[serde(default)]
    pub global_profile_binding_sha256: Option<String>,
    pub model: Option<String>,
    pub default_effort: String,
    pub purpose: String,
    pub purpose_label: Option<String>,
    pub required_capabilities: Vec<String>,
    pub execution_lane: String,
    pub required_assurance: String,
    pub native_subagent_policy: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecialistRolePolicyInput {
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecialistPolicyInput {
    pub schema_version: u32,
    pub policy_name: String,
    pub revision: u64,
    pub approval_policy: String,
    pub max_active_specialists: u32,
    pub roles: Vec<SpecialistRolePolicyInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledSpecialistRole {
    pub role_ref: String,
    pub role_source: RoleSourceReference,
    pub role_source_sha256: String,
    pub role: RoleSource,
    pub agent_configuration: AgentConfigurationSnapshot,
    pub max_active_instances: u32,
    pub reuse_policy: String,
    pub allowed_access: Vec<String>,
    pub activation_policy: String,
    pub primary_may_request: bool,
    pub collaboration_source: bool,
    pub collaboration_target: bool,
    pub auto_approve_when_fully_delegated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledSpecialistPolicy {
    pub schema_version: u32,
    pub policy_name: String,
    pub revision: u64,
    pub approval_policy: String,
    pub max_active_specialists: u32,
    pub roles: Vec<InstalledSpecialistRole>,
}

impl InstalledSpecialistPolicy {
    pub fn digest(&self) -> Result<String, MachineError> {
        let value = serde_json::to_string(self).map_err(internal)?;
        let canonical = canonicalize(&parse(&value).map_err(internal)?).map_err(internal)?;
        Ok(sha256_hex(&canonical))
    }

    pub fn role(&self, role_ref: &str) -> Result<&InstalledSpecialistRole, MachineError> {
        self.roles
            .iter()
            .find(|role| role.role_ref == role_ref)
            .ok_or_else(|| policy_denied(role_ref, "role is not admitted by the session policy"))
    }
}

pub struct SpecialistPolicyRegistry {
    home: DolgoraeHome,
    workspace: WorkspaceView,
    registry_root: PathBuf,
    uid: u32,
}

impl SpecialistPolicyRegistry {
    pub fn discover(workspace: Option<&Path>) -> Result<Self, MachineError> {
        let workspace = WorkspaceService::system()?.discover(workspace)?;
        let home = DolgoraeHome::system()?;
        let registry_root = home
            .workspace_root(&workspace.workspace_id)
            .join("specialist-policies");
        Ok(Self {
            home,
            workspace,
            registry_root,
            uid: DarwinSystem.current_uid(),
        })
    }

    pub fn validate_file(&self, input: &Path) -> Result<InstalledSpecialistPolicy, MachineError> {
        let bytes = read_input_file(input)?;
        let policy = decode_policy_input(input, &bytes)?;
        self.compile(policy)
    }

    pub fn add_file(
        &self,
        input: &Path,
        expected_name: &str,
    ) -> Result<InstalledSpecialistPolicy, MachineError> {
        let policy = self.validate_file(input)?;
        if policy.policy_name != expected_name {
            return Err(MachineError::invalid_argument(
                "name",
                "the requested policy name differs from the compiled policy",
            ));
        }
        self.install(policy)
    }

    fn install(
        &self,
        policy: InstalledSpecialistPolicy,
    ) -> Result<InstalledSpecialistPolicy, MachineError> {
        self.ensure_registry_root()?;
        let path = self.policy_path(&policy.policy_name)?;
        let bytes = canonical_bytes(&policy)?;
        atomic_create(&SystemWorkspacePlatform, &path, &bytes, 0o600).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                MachineError::new(
                    "POLICY_REJECTED",
                    "the Specialist Policy already exists",
                    false,
                    serde_json::json!({"policy_name": policy.policy_name}),
                )
            } else {
                MachineError::runtime_path_invalid(&path, error.to_string())
            }
        })?;
        Ok(policy)
    }

    pub fn resolve(&self, name: &str) -> Result<InstalledSpecialistPolicy, MachineError> {
        let path = self.policy_path(name)?;
        verify_secure_directory(&self.registry_root, self.uid)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|_| policy_denied(name, "installed policy is unavailable"))?;
        let metadata = file
            .metadata()
            .map_err(|_| policy_denied(name, "installed policy cannot be inspected"))?;
        if !metadata.is_file()
            || metadata.uid() != self.uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() > MAX_CONFIG_BYTES
        {
            return Err(policy_denied(name, "installed policy file is unsafe"));
        }
        let bytes = read_bounded(&mut file, MAX_CONFIG_BYTES)
            .map_err(|_| policy_denied(name, "installed policy cannot be read"))?;
        let policy: InstalledSpecialistPolicy = decode_checked(&path, &bytes)?;
        validate_installed_policy(&policy)?;
        if policy.policy_name != name {
            return Err(policy_denied(
                name,
                "installed policy identity does not match its file",
            ));
        }
        Ok(policy)
    }

    /// Resolves an installed snapshot and revalidates every pinned global
    /// Profile binding and required capability before a new session exists.
    pub fn resolve_for_session(
        &self,
        name: &str,
    ) -> Result<InstalledSpecialistPolicy, MachineError> {
        let policy = self.resolve(name)?;
        let workspace = self.workspace.canonical_path.to_path_buf()?;
        for role in &policy.roles {
            let configuration = &role.agent_configuration;
            let prepared = prepare_external_specialist(
                Some(&workspace),
                &role.role_ref,
                ExternalAgentConfigurationInput {
                    schema_version: 2,
                    runtime_profile: configuration.runtime_profile.clone(),
                    global_profile_binding_sha256: Some(
                        configuration.runtime_profile_snapshot_sha256.clone(),
                    ),
                    model: Some(configuration.model.clone()),
                    default_effort: configuration.default_effort.clone(),
                    purpose: configuration.purpose.kind.as_str().to_owned(),
                    purpose_label: configuration.purpose.external_label.clone(),
                    required_capabilities: configuration.required_capabilities.clone(),
                    instructions: configuration.normalized_instructions.clone(),
                    execution_lane: configuration.execution_lane.as_str().to_owned(),
                    required_assurance: configuration.required_assurance.as_str().to_owned(),
                    native_subagent_policy: configuration.native_subagent_policy.clone(),
                },
            )?;
            if prepared.agent_configuration != *configuration {
                return Err(config(
                    "specialist-policy",
                    "current global Profile binding differs from the installed Role snapshot",
                ));
            }
        }
        Ok(policy)
    }

    pub fn list(&self) -> Result<Vec<InstalledSpecialistPolicy>, MachineError> {
        if !self.registry_root.exists() {
            return Ok(Vec::new());
        }
        verify_secure_directory(&self.registry_root, self.uid)?;
        let mut policies = Vec::new();
        for entry in fs::read_dir(&self.registry_root).map_err(|error| {
            MachineError::runtime_path_invalid(&self.registry_root, error.to_string())
        })? {
            let entry = entry.map_err(internal)?;
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(OsStr::to_str)
                .and_then(|name| name.strip_suffix(".json"))
            else {
                return Err(MachineError::runtime_path_invalid(
                    path,
                    "registry contains an unsupported entry",
                ));
            };
            validate_name(name, "policy_name")?;
            policies.push(self.resolve(name)?);
        }
        policies.sort_by(|left, right| left.policy_name.cmp(&right.policy_name));
        Ok(policies)
    }

    pub fn remove(&self, name: &str) -> Result<(), MachineError> {
        let path = self.policy_path(name)?;
        let _ = self.resolve(name)?;
        fs::remove_file(&path)
            .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
        sync_directory(&self.registry_root).map_err(|error| {
            MachineError::runtime_path_invalid(&self.registry_root, error.to_string())
        })
    }

    fn compile(
        &self,
        input: SpecialistPolicyInput,
    ) -> Result<InstalledSpecialistPolicy, MachineError> {
        validate_policy_input(&input)?;
        let workspace_path = self.workspace.canonical_path.to_path_buf()?;
        let mut captured = Vec::<(RoleSourceReference, RoleSource, String)>::new();
        let mut roles = Vec::with_capacity(input.roles.len());
        for role in input.roles {
            let capture = match captured
                .iter()
                .find(|(reference, _, _)| reference == &role.role_source)
            {
                Some(value) => value.clone(),
                None => {
                    let (source, digest) = self.capture_role(&workspace_path, &role.role_source)?;
                    let value = (role.role_source.clone(), source, digest);
                    captured.push(value.clone());
                    value
                }
            };
            let prepared = prepare_external_specialist_read_only(
                Some(&workspace_path),
                &role.role_ref,
                ExternalAgentConfigurationInput {
                    schema_version: role.agent_configuration.schema_version,
                    runtime_profile: role.agent_configuration.selected_profile,
                    global_profile_binding_sha256: role
                        .agent_configuration
                        .global_profile_binding_sha256,
                    model: role.agent_configuration.model,
                    default_effort: role.agent_configuration.default_effort,
                    purpose: role.agent_configuration.purpose,
                    purpose_label: role.agent_configuration.purpose_label,
                    required_capabilities: role.agent_configuration.required_capabilities,
                    instructions: capture.1.instructions.clone(),
                    execution_lane: role.agent_configuration.execution_lane,
                    required_assurance: role.agent_configuration.required_assurance,
                    native_subagent_policy: role.agent_configuration.native_subagent_policy,
                },
            )?;
            roles.push(InstalledSpecialistRole {
                role_ref: role.role_ref,
                role_source: capture.0,
                role_source_sha256: capture.2,
                role: capture.1,
                agent_configuration: prepared.agent_configuration,
                max_active_instances: role.max_active_instances,
                reuse_policy: role.reuse_policy,
                allowed_access: role.allowed_access,
                activation_policy: role.activation_policy,
                primary_may_request: role.primary_may_request,
                collaboration_source: role.collaboration_source,
                collaboration_target: role.collaboration_target,
                auto_approve_when_fully_delegated: role.auto_approve_when_fully_delegated,
            });
        }
        let installed = InstalledSpecialistPolicy {
            schema_version: 2,
            policy_name: input.policy_name,
            revision: input.revision,
            approval_policy: input.approval_policy,
            max_active_specialists: input.max_active_specialists,
            roles,
        };
        validate_installed_policy(&installed)?;
        Ok(installed)
    }

    fn capture_role(
        &self,
        workspace: &Path,
        reference: &RoleSourceReference,
    ) -> Result<(RoleSource, String), MachineError> {
        self.capture_role_before_read(workspace, reference, || Ok(()))
    }

    fn capture_role_before_read<F>(
        &self,
        workspace: &Path,
        reference: &RoleSourceReference,
        before_read: F,
    ) -> Result<(RoleSource, String), MachineError>
    where
        F: FnOnce() -> Result<(), MachineError>,
    {
        validate_name(&reference.name, "role_source.name")?;
        let (root, directory_mode, file_mode) = match reference.scope {
            RoleSourceScope::Common => (self.home.root().to_path_buf(), Some(0o700), Some(0o600)),
            RoleSourceScope::Project => (workspace.to_path_buf(), None, None),
        };
        let root_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&root)
            .map_err(|error| config(&root, error.to_string()))?;
        let first = match reference.scope {
            RoleSourceScope::Common => "roles",
            RoleSourceScope::Project => ".dolgorae",
        };
        let first = open_relative_nofollow(&root_file, OsStr::new(first), true)
            .map_err(|error| config(&root, error.to_string()))?;
        let roles = if reference.scope == RoleSourceScope::Project {
            open_relative_nofollow(&first, OsStr::new("roles"), true)
                .map_err(|error| config(workspace, error.to_string()))?
        } else {
            first
        };
        let directory = roles.metadata().map_err(internal)?;
        let unsafe_directory = !directory.is_dir()
            || directory.uid() != self.uid
            || directory.mode() & 0o022 != 0
            || directory_mode.is_some_and(|mode| directory.mode() & 0o777 != mode);
        if unsafe_directory {
            return Err(config(workspace, "Role source directory is unsafe"));
        }
        let filename = format!("{}.json", reference.name);
        let mut file = open_relative_nofollow(&roles, OsStr::new(&filename), false)
            .map_err(|error| config(workspace, error.to_string()))?;
        let metadata = file.metadata().map_err(internal)?;
        let unsafe_file = !metadata.is_file()
            || metadata.uid() != self.uid
            || metadata.mode() & 0o022 != 0
            || file_mode.is_some_and(|mode| metadata.mode() & 0o777 != mode)
            || metadata.len() > MAX_CONFIG_BYTES;
        if unsafe_file {
            return Err(config(workspace, "Role source file is unsafe"));
        }
        before_read()?;
        let bytes = read_bounded(&mut file, MAX_CONFIG_BYTES)
            .map_err(|error| config(workspace, error.to_string()))?;
        let role: RoleSource = decode_checked(workspace, &bytes)?;
        validate_role(&role)?;
        if role.name != reference.name {
            return Err(config(
                workspace,
                "Role source name does not match its filename",
            ));
        }
        let canonical = canonical_bytes(&role)?;
        Ok((role, sha256_hex(&canonical)))
    }

    fn ensure_registry_root(&self) -> Result<(), MachineError> {
        let parent = self
            .registry_root
            .parent()
            .ok_or_else(|| internal("registry root has no parent"))?;
        verify_secure_directory(parent, self.uid)?;
        if !self.registry_root.exists() {
            create_directory(&self.registry_root, 0o700).map_err(|error| {
                MachineError::runtime_path_invalid(&self.registry_root, error.to_string())
            })?;
        }
        verify_secure_directory(&self.registry_root, self.uid)
    }

    fn policy_path(&self, name: &str) -> Result<PathBuf, MachineError> {
        validate_name(name, "policy_name")?;
        Ok(self.registry_root.join(format!("{name}.json")))
    }
}

fn validate_policy_input(policy: &SpecialistPolicyInput) -> Result<(), MachineError> {
    if policy.schema_version != 2
        || policy.revision == 0
        || !matches!(
            policy.approval_policy.as_str(),
            "user_approval_required" | "fully_delegated"
        )
        || !(1..=64).contains(&policy.max_active_specialists)
        || policy.roles.is_empty()
        || policy.roles.len() > 64
    {
        return Err(config("specialist-policy", "unsupported policy contract"));
    }
    validate_name(&policy.policy_name, "policy_name")?;
    let mut refs = BTreeSet::new();
    for role in &policy.roles {
        validate_name(&role.role_ref, "role_ref")?;
        validate_name(&role.role_source.name, "role_source.name")?;
        if !refs.insert(&role.role_ref) {
            return Err(config(
                "specialist-policy",
                "role_ref values must be unique",
            ));
        }
        validate_role_controls(role)?;
    }
    Ok(())
}

pub(crate) fn validate_installed_policy(
    policy: &InstalledSpecialistPolicy,
) -> Result<(), MachineError> {
    if policy.schema_version != 2 || policy.revision == 0 {
        return Err(config(
            "specialist-policy",
            "installed policy version is unsupported",
        ));
    }
    validate_name(&policy.policy_name, "policy_name")?;
    let mut refs = BTreeSet::new();
    for role in &policy.roles {
        validate_name(&role.role_ref, "role_ref")?;
        validate_name(&role.role_source.name, "role_source.name")?;
        if !refs.insert(&role.role_ref) {
            return Err(config(
                "specialist-policy",
                "installed role_ref values must be unique",
            ));
        }
        validate_role(&role.role)?;
        let digest = sha256_hex(&canonical_bytes(&role.role)?);
        if digest != role.role_source_sha256
            || role.role.name != role.role_source.name
            || role.agent_configuration.role_reference.as_deref() != Some(&role.role_ref)
        {
            return Err(config(
                "specialist-policy",
                "installed Role snapshot integrity failed",
            ));
        }
        let configuration = &role.agent_configuration;
        let instructions = &configuration.instructions;
        if configuration.schema_version != 2
            || !bounded(&configuration.runtime_profile, 128)
            || !is_sha256(&configuration.runtime_profile_snapshot_sha256)
            || !bounded(&configuration.model, 128)
            || !bounded(&configuration.default_effort, 64)
            || configuration.required_capabilities.len() > 64
            || configuration
                .required_capabilities
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != configuration.required_capabilities.len()
            || configuration
                .required_capabilities
                .iter()
                .any(|capability| !bounded(capability, 128))
            || configuration.normalized_instructions != role.role.instructions
            || instructions.schema != "dolgorae.instructions/v1"
            || instructions.common_prefix_version != 1
            || instructions.mode_prefix_version != 1
            || instructions.purpose_prefix_version != 1
            || instructions.normalized_byte_length
                != u64::try_from(configuration.normalized_instructions.len()).unwrap_or(u64::MAX)
            || instructions.normalized_sha256
                != sha256_hex(configuration.normalized_instructions.as_bytes())
            || configuration.native_subagent_policy != "enabled"
        {
            return Err(config(
                "specialist-policy",
                "installed Agent Configuration snapshot integrity failed",
            ));
        }
        validate_installed_role_controls(role)?;
    }
    if refs.is_empty()
        || policy.roles.len() > 64
        || !(1..=64).contains(&policy.max_active_specialists)
        || !matches!(
            policy.approval_policy.as_str(),
            "user_approval_required" | "fully_delegated"
        )
    {
        return Err(config(
            "specialist-policy",
            "installed policy contract is invalid",
        ));
    }
    Ok(())
}

fn validate_installed_role_controls(role: &InstalledSpecialistRole) -> Result<(), MachineError> {
    if !(1..=16).contains(&role.max_active_instances)
        || !matches!(
            role.reuse_policy.as_str(),
            "never" | "reuse_idle_compatible" | "reuse_any_compatible"
        )
        || role.allowed_access.is_empty()
        || role.allowed_access.len() > 3
        || role.allowed_access.iter().collect::<BTreeSet<_>>().len() != role.allowed_access.len()
        || role.allowed_access.iter().any(|value| {
            !matches!(
                value.as_str(),
                "read_only" | "isolated_write" | "canonical_workspace_write"
            )
        })
        || !matches!(
            role.activation_policy.as_str(),
            "on_mail" | "manual" | "keep_resident" | "never"
        )
        || ((role.collaboration_source || role.collaboration_target)
            && !matches!(role.activation_policy.as_str(), "on_mail" | "keep_resident"))
        || (role
            .allowed_access
            .iter()
            .any(|access| access != "read_only")
            && role.agent_configuration.execution_lane.as_str() != "dedicated")
    {
        return Err(config(
            "specialist-policy",
            "installed Role admission controls are invalid",
        ));
    }
    Ok(())
}

fn validate_role_controls(role: &SpecialistRolePolicyInput) -> Result<(), MachineError> {
    if role.agent_configuration.schema_version != 2
        || !(1..=16).contains(&role.max_active_instances)
        || !matches!(
            role.reuse_policy.as_str(),
            "never" | "reuse_idle_compatible" | "reuse_any_compatible"
        )
        || role.allowed_access.is_empty()
        || role.allowed_access.len() > 3
        || role.allowed_access.iter().collect::<BTreeSet<_>>().len() != role.allowed_access.len()
        || role.allowed_access.iter().any(|value| {
            !matches!(
                value.as_str(),
                "read_only" | "isolated_write" | "canonical_workspace_write"
            )
        })
        || !matches!(
            role.activation_policy.as_str(),
            "on_mail" | "manual" | "keep_resident" | "never"
        )
        || ((role.collaboration_source || role.collaboration_target)
            && !matches!(role.activation_policy.as_str(), "on_mail" | "keep_resident"))
        || (role
            .allowed_access
            .iter()
            .any(|access| access != "read_only")
            && role.agent_configuration.execution_lane != "dedicated")
    {
        return Err(config(
            "specialist-policy",
            "Role admission controls are invalid",
        ));
    }
    Ok(())
}

fn validate_role(role: &RoleSource) -> Result<(), MachineError> {
    validate_name(&role.name, "role.name")?;
    if role.schema_version != 1
        || !bounded(&role.display_name, 128)
        || !bounded(&role.description, 1_024)
        || !bounded(&role.instructions, 65_536)
    {
        return Err(config("role-source", "Role source fields are invalid"));
    }
    Ok(())
}

fn validate_name(name: &str, field: &str) -> Result<(), MachineError> {
    if name.is_empty()
        || name.len() > 64
        || !name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return Err(config(field, "name does not match the checked pattern"));
    }
    Ok(())
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

fn read_input_file(path: &Path) -> Result<Vec<u8>, MachineError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| config(path, error.to_string()))?;
    let metadata = file.metadata().map_err(internal)?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err(config(path, "policy input must be a bounded regular file"));
    }
    read_bounded(&mut file, MAX_CONFIG_BYTES).map_err(|error| config(path, error.to_string()))
}

fn read_bounded(file: &mut File, maximum: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds the checked bound",
        ));
    }
    Ok(bytes)
}

fn decode_policy_input(path: &Path, bytes: &[u8]) -> Result<SpecialistPolicyInput, MachineError> {
    decode_checked(path, bytes)
}

fn decode_checked<T: for<'de> Deserialize<'de>>(
    path: impl AsRef<Path>,
    bytes: &[u8],
) -> Result<T, MachineError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| config(path.as_ref(), "file is not UTF-8"))?;
    let canonical =
        canonicalize(&parse(text).map_err(|error| config(path.as_ref(), error.to_string()))?)
            .map_err(|error| config(path.as_ref(), error.to_string()))?;
    serde_json::from_slice(&canonical)
        .map_err(|_| config(path.as_ref(), "file does not match the checked contract"))
}

fn canonical_bytes(value: &impl Serialize) -> Result<Vec<u8>, MachineError> {
    let text = serde_json::to_string(value).map_err(internal)?;
    canonicalize(&parse(&text).map_err(internal)?).map_err(internal)
}

fn config(path: impl AsRef<Path>, reason: impl Into<String>) -> MachineError {
    MachineError::config_invalid(path, reason)
}

fn policy_denied(role: &str, reason: &str) -> MachineError {
    MachineError::new(
        "SPECIALIST_POLICY_DENIED",
        "the Specialist Policy denied the operation",
        false,
        serde_json::json!({"role_ref": role, "reason": reason}),
    )
}

fn option_path(args: &[std::ffi::OsString], flag: &str) -> Result<Option<PathBuf>, MachineError> {
    let mut value = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == OsStr::new(flag) {
            let next = args
                .get(index + 1)
                .ok_or_else(|| MachineError::invalid_argument(flag, "option value is missing"))?;
            if value.replace(PathBuf::from(next)).is_some() {
                return Err(MachineError::invalid_argument(
                    flag,
                    "option may appear only once",
                ));
            }
            index += 2;
        } else {
            index += if args[index].to_string_lossy().starts_with("--") {
                2
            } else {
                1
            };
        }
    }
    Ok(value)
}

fn required_path(args: &[std::ffi::OsString], flag: &str) -> Result<PathBuf, MachineError> {
    option_path(args, flag)?
        .ok_or_else(|| MachineError::invalid_argument(flag, "required option is missing"))
}

fn positional(args: &[std::ffi::OsString], requested: usize) -> Result<String, MachineError> {
    let mut positionals = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let value = &args[index];
        if value == OsStr::new("--workspace") || value == OsStr::new("--file") {
            index += 2;
        } else {
            positionals.push(value);
            index += 1;
        }
    }
    positionals
        .get(requested)
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            MachineError::invalid_argument("name", "required positional name is missing")
        })
}

fn internal(error: impl ToString) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "Specialist Policy operation failed",
        false,
        serde_json::json!({"reason": error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Assurance, ExecutionLane, Purpose, PurposeKind};
    use crate::run::InstructionSnapshot;
    use crate::workspace::{LosslessPath, WorkspaceMode};
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use uuid::Uuid;

    struct Fixture {
        root: PathBuf,
        workspace: PathBuf,
        registry: SpecialistPolicyRegistry,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dolgorae-policy-{}", Uuid::now_v7()));
            let workspace = root.join("workspace");
            fs::create_dir_all(workspace.join(".dolgorae/roles")).unwrap();
            let home = DolgoraeHome::from_canonical_home(root.join("home")).unwrap();
            fs::create_dir_all(home.root().join("roles")).unwrap();
            fs::create_dir_all(home.workspace_root("workspace")).unwrap();
            for path in [
                root.as_path(),
                root.join("home").as_path(),
                home.root(),
                home.root().join("roles").as_path(),
                home.root().join("workspaces").as_path(),
                home.workspace_root("workspace").as_path(),
            ] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let registry_root = home.workspace_root("workspace").join("specialist-policies");
            Self {
                root,
                workspace: workspace.clone(),
                registry: SpecialistPolicyRegistry {
                    home,
                    workspace: WorkspaceView {
                        workspace_id: "workspace".to_owned(),
                        canonical_path: LosslessPath::from_path(&workspace),
                        mode: WorkspaceMode::NonGit,
                        created: false,
                    },
                    registry_root,
                    uid: DarwinSystem.current_uid(),
                },
            }
        }

        fn write_role(&self, scope: RoleSourceScope, role: &RoleSource) -> PathBuf {
            let root = match scope {
                RoleSourceScope::Common => self.registry.home.root().join("roles"),
                RoleSourceScope::Project => self.workspace.join(".dolgorae/roles"),
            };
            let path = root.join(format!("{}.json", role.name));
            fs::write(&path, canonical_bytes(role).unwrap()).unwrap();
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if scope == RoleSourceScope::Common {
                    0o600
                } else {
                    0o644
                }),
            )
            .unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn role(name: &str, instructions: &str) -> RoleSource {
        RoleSource {
            schema_version: 1,
            name: name.to_owned(),
            display_name: "Reviewer".to_owned(),
            description: "Reviews bounded evidence.".to_owned(),
            instructions: instructions.to_owned(),
        }
    }

    fn installed(reference: RoleSourceReference, role: RoleSource) -> InstalledSpecialistPolicy {
        let normalized = role.instructions.clone();
        InstalledSpecialistPolicy {
            schema_version: 2,
            policy_name: "review-policy".to_owned(),
            revision: 1,
            approval_policy: "user_approval_required".to_owned(),
            max_active_specialists: 1,
            roles: vec![InstalledSpecialistRole {
                role_ref: "reviewer".to_owned(),
                role_source: reference,
                role_source_sha256: sha256_hex(&canonical_bytes(&role).unwrap()),
                role,
                agent_configuration: AgentConfigurationSnapshot {
                    schema_version: 2,
                    runtime_profile: "reviewer".to_owned(),
                    runtime_profile_snapshot_sha256: "a".repeat(64),
                    model: "gpt-5.6".to_owned(),
                    default_effort: "high".to_owned(),
                    purpose: Purpose {
                        kind: PurposeKind::Review,
                        external_label: None,
                    },
                    required_capabilities: Vec::new(),
                    role_reference: Some("reviewer".to_owned()),
                    normalized_instructions: normalized.clone(),
                    instructions: InstructionSnapshot {
                        schema: "dolgorae.instructions/v1".to_owned(),
                        common_prefix_version: 1,
                        mode_prefix_version: 1,
                        purpose_prefix_version: 1,
                        normalized_byte_length: normalized.len() as u64,
                        normalized_sha256: sha256_hex(normalized.as_bytes()),
                    },
                    execution_lane: ExecutionLane::Dedicated,
                    required_assurance: Assurance::BestEffortPersonalAlpha,
                    native_subagent_policy: "enabled".to_owned(),
                },
                max_active_instances: 1,
                reuse_policy: "never".to_owned(),
                allowed_access: vec!["read_only".to_owned(), "isolated_write".to_owned()],
                activation_policy: "keep_resident".to_owned(),
                primary_may_request: true,
                collaboration_source: false,
                collaboration_target: false,
                auto_approve_when_fully_delegated: true,
            }],
        }
    }

    #[test]
    fn explicit_scope_selects_same_named_role_and_capture_survives_source_deletion() {
        let fixture = Fixture::new();
        let common = role("reviewer", "Use the common review instructions.");
        let project = role("reviewer", "Use the project review instructions.");
        fixture.write_role(RoleSourceScope::Common, &common);
        let project_path = fixture.write_role(RoleSourceScope::Project, &project);

        let (captured_common, _) = fixture
            .registry
            .capture_role(
                &fixture.workspace,
                &RoleSourceReference {
                    scope: RoleSourceScope::Common,
                    name: "reviewer".to_owned(),
                },
            )
            .unwrap();
        let project_reference = RoleSourceReference {
            scope: RoleSourceScope::Project,
            name: "reviewer".to_owned(),
        };
        let (captured_project, _) = fixture
            .registry
            .capture_role(&fixture.workspace, &project_reference)
            .unwrap();
        assert_eq!(captured_common.instructions, common.instructions);
        assert_eq!(captured_project.instructions, project.instructions);

        let policy = installed(project_reference, captured_project);
        fixture.registry.ensure_registry_root().unwrap();
        atomic_create(
            &SystemWorkspacePlatform,
            &fixture.registry.policy_path("review-policy").unwrap(),
            &canonical_bytes(&policy).unwrap(),
            0o600,
        )
        .unwrap();
        fs::remove_file(project_path).unwrap();
        assert_eq!(fixture.registry.resolve("review-policy").unwrap(), policy);
    }

    #[test]
    fn unsafe_missing_and_symlink_role_sources_fail_closed() {
        let fixture = Fixture::new();
        let reference = RoleSourceReference {
            scope: RoleSourceScope::Project,
            name: "reviewer".to_owned(),
        };
        assert_eq!(
            fixture
                .registry
                .capture_role(&fixture.workspace, &reference)
                .unwrap_err()
                .code,
            "CONFIG_INVALID"
        );
        let path = fixture.write_role(
            RoleSourceScope::Project,
            &role("reviewer", "Inspect the bounded target."),
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            fixture
                .registry
                .capture_role(&fixture.workspace, &reference)
                .unwrap_err()
                .code,
            "CONFIG_INVALID"
        );
        fs::remove_file(&path).unwrap();
        let target = fixture.workspace.join("target.json");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, &path).unwrap();
        assert_eq!(
            fixture
                .registry
                .capture_role(&fixture.workspace, &reference)
                .unwrap_err()
                .code,
            "CONFIG_INVALID"
        );
    }

    #[test]
    fn source_path_replacement_after_open_cannot_change_captured_bytes() {
        let fixture = Fixture::new();
        let original = role("reviewer", "Capture the already opened source bytes.");
        let replacement = role("reviewer", "These replacement bytes are too late.");
        let path = fixture.write_role(RoleSourceScope::Project, &original);
        let displaced = fixture.workspace.join("opened-reviewer.json");
        let reference = RoleSourceReference {
            scope: RoleSourceScope::Project,
            name: "reviewer".to_owned(),
        };
        let (captured, digest) = fixture
            .registry
            .capture_role_before_read(&fixture.workspace, &reference, || {
                fs::rename(&path, &displaced).map_err(internal)?;
                fs::write(&path, canonical_bytes(&replacement)?).map_err(internal)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).map_err(internal)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(captured, original);
        assert_eq!(digest, sha256_hex(&canonical_bytes(&original).unwrap()));
        let current: RoleSource = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(current, replacement);
    }

    #[test]
    fn input_and_installed_policy_semantics_reject_inline_or_tampered_content() {
        let duplicate = br#"{"schema_version":2,"schema_version":2}"#;
        assert_eq!(
            decode_policy_input(Path::new("duplicate.json"), duplicate)
                .unwrap_err()
                .code,
            "CONFIG_INVALID"
        );
        let inline = br#"{
          "schema_version":2,"policy_name":"review-policy","revision":1,
          "approval_policy":"user_approval_required","max_active_specialists":1,
          "roles":[{"role_ref":"reviewer","role_source":{"scope":"project","name":"reviewer"},
          "agent_configuration":{"schema_version":2,"selected_profile":"reviewer","model":null,
          "default_effort":"high","purpose":"review","purpose_label":null,
          "required_capabilities":[],"instructions":"forbidden","execution_lane":"dedicated",
          "required_assurance":"best_effort_personal_alpha","native_subagent_policy":"enabled"},
          "max_active_instances":1,"reuse_policy":"never","allowed_access":["read_only"],
          "activation_policy":"keep_resident","primary_may_request":true,
          "collaboration_source":false,"collaboration_target":false,
          "auto_approve_when_fully_delegated":true}]}
        "#;
        assert_eq!(
            decode_policy_input(Path::new("inline.json"), inline)
                .unwrap_err()
                .code,
            "CONFIG_INVALID"
        );
        let mut policy = installed(
            RoleSourceReference {
                scope: RoleSourceScope::Project,
                name: "reviewer".to_owned(),
            },
            role("reviewer", "Inspect the bounded target."),
        );
        policy.roles[0].role_source_sha256 = "0".repeat(64);
        assert_eq!(
            validate_installed_policy(&policy).unwrap_err().code,
            "CONFIG_INVALID"
        );
    }

    #[test]
    fn create_exclusive_install_failure_preserves_the_existing_snapshot() {
        let fixture = Fixture::new();
        let policy = installed(
            RoleSourceReference {
                scope: RoleSourceScope::Project,
                name: "reviewer".to_owned(),
            },
            role("reviewer", "Inspect the bounded target."),
        );
        assert_eq!(fixture.registry.install(policy.clone()).unwrap(), policy);
        assert_eq!(
            fixture.registry.install(policy.clone()).unwrap_err().code,
            "POLICY_REJECTED"
        );
        assert_eq!(fixture.registry.resolve("review-policy").unwrap(), policy);
    }
}
