//! Explicit legacy authoring migration. Accepted runtime snapshots are untouched.

use crate::darwin::DarwinSystem;
use crate::instructions::{InstructionSourceStore, validate_role_name};
use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use crate::markdown_policy::{PolicyInput, RolePolicyInput};
use crate::paths::DolgoraeHome;
use crate::specialist_policy::{
    RoleSource, RoleSourceReference, RoleSourceScope, SpecialistPolicyRegistry,
    decode_policy_input, read_input_file, validate_policy_input,
};
use crate::workspace::WorkspaceService;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_REQUEST_BYTES: usize = 1_048_576;
const MAX_ROLES: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSelection {
    pub source: RoleSourceReference,
    pub source_sha256: String,
    pub destination_name: String,
    pub publish: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAccessSelection {
    pub role_ref: String,
    pub allowed_access: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySelection {
    pub source_file: PathBuf,
    pub source_sha256: String,
    pub destination_file: PathBuf,
    pub access: Vec<PolicyAccessSelection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationRequest {
    pub schema: String,
    pub roles: Vec<RoleSelection>,
    pub policies: Vec<PolicySelection>,
}

struct LegacyRole {
    reference: RoleSourceReference,
    source: RoleSource,
    digest: String,
}

pub fn execute_cli(args: &[OsString]) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|_| invalid("invalid workspace option"))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let root = view.canonical_path.to_path_buf()?;
    let home = DolgoraeHome::system()?;
    let registry = SpecialistPolicyRegistry::discover(Some(&root))?;
    let roles = enumerate(&home, &root, &registry)?;
    if args.iter().any(|arg| arg == "--request-stdin") {
        if std::io::stdin().is_terminal() {
            return Err(invalid("request requires non-TTY stdin"));
        }
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_REQUEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("request read failed"))?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(invalid("request exceeds its byte bound"));
        }
        let request: MigrationRequest = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("request does not match the migration contract"))?;
        return apply(&home, &roles, request);
    }
    let mut entries = Vec::new();
    for role in &roles {
        let duplicate = roles
            .iter()
            .filter(|other| other.reference.name == role.reference.name)
            .count()
            > 1;
        entries.push(json!({
            "source": role.reference,
            "source_sha256": role.digest,
            "destination_name": role.reference.name,
            "destination_exists": destination_exists(&home, &role.reference.name)?,
            "name_collision": duplicate,
        }));
    }
    let policy = crate::cli::option_path(args, "--file").map_err(|_| invalid("invalid file option"))?
        .map(|path| {
            let bytes = read_input_file(&path)?;
            let input = decode_policy_input(&path, &bytes)?;
            validate_policy_input(&input)?;
            Ok::<_, MachineError>(json!({"source_sha256": sha256_hex(&bytes), "policy_name": input.policy_name,
                "role_sources": input.roles.iter().map(|role| &role.role_source).collect::<Vec<_>>() }))
        }).transpose()?;
    Ok(json!({"schema":"dolgorae-instruction-migration-plan/v1", "roles":entries, "policy":policy}))
}

fn enumerate(
    home: &DolgoraeHome,
    workspace: &Path,
    registry: &SpecialistPolicyRegistry,
) -> Result<Vec<LegacyRole>, MachineError> {
    let mut roles = Vec::new();
    for (scope, directory) in [
        (RoleSourceScope::Common, home.root().join("roles")),
        (RoleSourceScope::Project, workspace.join(".dolgorae/roles")),
    ] {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(invalid("legacy Role directory cannot be enumerated")),
        };
        for entry in entries {
            let entry = entry.map_err(|_| invalid("legacy Role entry cannot be read"))?;
            let filename = entry.file_name();
            if filename.as_bytes().ends_with(b".json") && filename.to_str().is_none() {
                return Err(invalid("legacy JSON Role filename must be UTF-8"));
            }
            let Some(name) = filename
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            validate_role_name(name)?;
            if roles.len() == MAX_ROLES {
                return Err(invalid("legacy Role count exceeds 128"));
            }
            let reference = RoleSourceReference {
                scope: scope.clone(),
                name: name.to_owned(),
            };
            let (source, digest) = registry.capture_role_for_migration(workspace, &reference)?;
            roles.push(LegacyRole {
                reference,
                source,
                digest,
            });
        }
    }
    roles.sort_by(|left, right| {
        scope_name(&left.reference.scope)
            .cmp(scope_name(&right.reference.scope))
            .then(left.reference.name.cmp(&right.reference.name))
    });
    Ok(roles)
}

fn apply(
    home: &DolgoraeHome,
    sources: &[LegacyRole],
    request: MigrationRequest,
) -> Result<Value, MachineError> {
    if request.schema != "dolgorae-instruction-migration-request/v1"
        || request.roles.len() > MAX_ROLES
        || request.policies.len() > 64
        || (request.roles.is_empty() && request.policies.is_empty())
    {
        return Err(invalid("unsupported or empty migration request"));
    }
    let store = InstructionSourceStore::new(home);
    let mut selected_sources = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    let mut publish = Vec::new();
    for selection in &request.roles {
        validate_role_name(&selection.destination_name)?;
        if !selected_sources.insert((scope_name(&selection.source.scope), &selection.source.name))
            || (selection.publish && !destinations.insert(&selection.destination_name))
        {
            return Err(invalid("duplicate source or publication destination"));
        }
        let source = sources
            .iter()
            .find(|role| role.reference == selection.source)
            .ok_or_else(|| invalid("selected legacy Role is absent"))?;
        if source.digest != selection.source_sha256 {
            return Err(invalid("selected legacy Role changed; replan"));
        }
        if selection.publish {
            if destination_exists(home, &selection.destination_name)? {
                return Err(invalid("Role destination already exists"));
            }
            // Validate the full bounded source before any output is published.
            crate::instructions::RoleCapture {
                name: selection.destination_name.clone(),
                body: source.source.instructions.clone(),
                byte_length: source.source.instructions.len() as u64,
                sha256: sha256_hex(source.source.instructions.as_bytes()),
            }
            .validate()?;
            publish.push((&selection.destination_name, &source.source.instructions));
        } else {
            store.capture(&selection.destination_name)?;
        }
    }
    let mut policies = Vec::new();
    let mut policy_destinations = BTreeSet::new();
    for selection in &request.policies {
        if !selection.source_file.is_absolute()
            || !selection.destination_file.is_absolute()
            || selection.source_file.as_os_str().as_bytes().len() > 8192
            || selection.destination_file.as_os_str().as_bytes().len() > 8192
            || !policy_destinations.insert(&selection.destination_file)
        {
            return Err(invalid(
                "policy paths must be absolute and destinations unique",
            ));
        }
        let bytes = read_input_file(&selection.source_file)?;
        if sha256_hex(&bytes) != selection.source_sha256 {
            return Err(invalid("policy input changed; replan"));
        }
        let input = decode_policy_input(&selection.source_file, &bytes)?;
        validate_policy_input(&input)?;
        if selection.access.len() != input.roles.len() {
            return Err(invalid(
                "every policy Role needs one explicit access selection",
            ));
        }
        let mut refs = BTreeSet::new();
        let mut roles = Vec::new();
        for legacy in input.roles {
            let mapping = request
                .roles
                .iter()
                .find(|mapping| mapping.source == legacy.role_source)
                .ok_or_else(|| invalid("policy Role has no explicit source mapping"))?;
            let source = sources
                .iter()
                .find(|source| source.reference == legacy.role_source)
                .ok_or_else(|| invalid("policy Role source is absent"))?;
            let access = selection
                .access
                .iter()
                .find(|access| access.role_ref == legacy.role_ref)
                .ok_or_else(|| invalid("policy Role has no explicit access selection"))?;
            if !refs.insert(&access.role_ref) {
                return Err(invalid("duplicate policy Role access selection"));
            }
            let mut configuration = serde_json::to_value(legacy.agent_configuration)
                .map_err(|_| invalid("configuration serialization failed"))?;
            configuration["schema_version"] = json!(3);
            // Successor Agent Configuration requires an explicit dedicated lane.
            // A different legacy lane is not silently rewritten.
            if configuration["execution_lane"] != "dedicated" {
                return Err(invalid(
                    "migrate the policy authoring lane to dedicated explicitly",
                ));
            }
            if configuration["global_profile_binding_sha256"].is_null() {
                configuration
                    .as_object_mut()
                    .expect("typed configuration")
                    .remove("global_profile_binding_sha256");
            }
            roles.push(RolePolicyInput {
                role_ref: legacy.role_ref,
                role_source: crate::markdown_policy::RoleSourceReference {
                    name: mapping.destination_name.clone(),
                },
                agent_configuration: serde_json::from_value(configuration)
                    .map_err(|_| invalid("configuration does not match v3"))?,
                max_active_instances: legacy.max_active_instances,
                reuse_policy: legacy.reuse_policy,
                allowed_access: access.allowed_access.clone(),
                activation_policy: legacy.activation_policy,
                primary_may_request: legacy.primary_may_request,
                collaboration_source: legacy.collaboration_source,
                collaboration_target: legacy.collaboration_target,
                auto_approve_when_fully_delegated: legacy.auto_approve_when_fully_delegated,
                display_name: source.source.display_name.clone(),
                description: source.source.description.clone(),
            });
        }
        let policy = PolicyInput {
            schema_version: 3,
            policy_name: input.policy_name,
            revision: input.revision,
            approval_policy: input.approval_policy,
            max_active_specialists: input.max_active_specialists,
            roles,
        };
        policy.validate()?;
        let output = serde_json::to_vec_pretty(&policy)
            .map_err(|_| invalid("policy serialization failed"))?;
        let directory = output_directory(&selection.destination_file)?;
        policies.push((&selection.destination_file, directory, output));
    }
    for (name, body) in &publish {
        store.publish_role_exclusive(name, body)?;
    }
    for (path, directory, bytes) in &policies {
        publish_policy(path, directory, bytes)?;
    }
    Ok(
        json!({"schema":"dolgorae-instruction-migration-result/v1", "published_roles":publish.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "published_policy_count":policies.len(), "sources_retained":true, "accepted_snapshots_changed":false}),
    )
}

fn scope_name(scope: &RoleSourceScope) -> &'static str {
    match scope {
        RoleSourceScope::Common => "common",
        RoleSourceScope::Project => "project",
    }
}

fn destination_exists(home: &DolgoraeHome, name: &str) -> Result<bool, MachineError> {
    validate_role_name(name)?;
    match fs::symlink_metadata(home.root().join("roles").join(name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(invalid("Role destination cannot be inspected")),
    }
}

fn output_directory(path: &Path) -> Result<File, MachineError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("policy output has no parent"))?;
    if parent
        .canonicalize()
        .map_err(|_| invalid("policy output parent is unavailable"))?
        != parent
    {
        return Err(invalid("policy output parent must be canonical"));
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(|_| invalid("policy output parent is unavailable"))?;
    let metadata = directory
        .metadata()
        .map_err(|_| invalid("policy output parent is unavailable"))?;
    if metadata.uid() != DarwinSystem.current_uid() || metadata.mode() & 0o022 != 0 {
        return Err(invalid("policy output parent is unsafe"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| invalid("policy output has no filename"))?;
    match DarwinSystem.statat_nofollow(&directory, name) {
        Ok(_) => Err(invalid("policy output already exists")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(directory),
        Err(_) => Err(invalid("policy output cannot be inspected")),
    }
}

fn publish_policy(path: &Path, directory: &File, bytes: &[u8]) -> Result<(), MachineError> {
    verify_output_parent(path, directory)?;
    let temporary = OsString::from(format!(".migration-{}", Uuid::now_v7()));
    let mut file = DarwinSystem
        .createat_private(directory, &temporary, true)
        .map_err(|_| invalid("policy output publication failed"))?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        DarwinSystem.renameat_exclusive(directory, &temporary, path.file_name().unwrap())?;
        directory.sync_all()
    })();
    if let (Ok(observed), Ok(owned)) = (
        DarwinSystem.statat_nofollow(directory, &temporary),
        file.metadata(),
    ) && (observed.device, observed.inode) == (owned.dev(), owned.ino())
    {
        let _ = DarwinSystem.unlinkat_file(directory, &temporary);
    }
    result.map_err(|_| {
        invalid("policy output publication failed; inspect completed outputs and replan")
    })?;
    verify_output_parent(path, directory)
}

fn verify_output_parent(path: &Path, directory: &File) -> Result<(), MachineError> {
    let observed = fs::symlink_metadata(path.parent().unwrap())
        .map_err(|_| invalid("policy output parent changed"))?;
    let owned = directory
        .metadata()
        .map_err(|_| invalid("policy output parent changed"))?;
    if (
        observed.dev(),
        observed.ino(),
        observed.uid(),
        observed.mode(),
    ) != (owned.dev(), owned.ino(), owned.uid(), owned.mode())
    {
        return Err(invalid(
            "policy output parent changed; inspect outputs and replan",
        ));
    }
    Ok(())
}

fn invalid(reason: &str) -> MachineError {
    MachineError::invalid_argument("migration", reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    struct Fixture {
        root: PathBuf,
        home: DolgoraeHome,
        workspace: PathBuf,
        registry: SpecialistPolicyRegistry,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dolgorae-migration-{}", Uuid::now_v7()));
            let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
            let workspace = root.join("workspace");
            for path in [
                &root,
                home.root(),
                &home.root().join("roles"),
                &workspace,
                &workspace.join(".dolgorae"),
                &workspace.join(".dolgorae/roles"),
            ] {
                fs::create_dir(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            InstructionSourceStore::new(&home)
                .initialize_templates()
                .unwrap();
            let view = crate::workspace::WorkspaceView {
                workspace_id: crate::workspace::workspace_id(&workspace),
                canonical_path: crate::workspace::LosslessPath::from_path(&workspace),
                mode: crate::workspace::WorkspaceMode::NonGit,
                created: false,
            };
            let registry = SpecialistPolicyRegistry::new(home.clone(), view);
            Self {
                root,
                home,
                workspace,
                registry,
            }
        }
        fn legacy(&self, scope: RoleSourceScope, name: &str, body: &str) -> PathBuf {
            let directory = match scope {
                RoleSourceScope::Common => self.home.root().join("roles"),
                RoleSourceScope::Project => self.workspace.join(".dolgorae/roles"),
            };
            let path = directory.join(format!("{name}.json"));
            let bytes = serde_json::to_vec(&RoleSource {
                schema_version: 1,
                name: name.to_owned(),
                display_name: name.to_owned(),
                description: "Legacy authoring input".to_owned(),
                instructions: body.to_owned(),
            })
            .unwrap();
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            path
        }
        fn sources(&self) -> Vec<LegacyRole> {
            enumerate(&self.home, &self.workspace, &self.registry).unwrap()
        }
        fn request(&self, sources: &[LegacyRole], destinations: &[&str]) -> MigrationRequest {
            MigrationRequest {
                schema: "dolgorae-instruction-migration-request/v1".to_owned(),
                policies: vec![],
                roles: sources
                    .iter()
                    .zip(destinations)
                    .map(|(source, name)| RoleSelection {
                        source: source.reference.clone(),
                        source_sha256: source.digest.clone(),
                        destination_name: (*name).to_owned(),
                        publish: true,
                    })
                    .collect(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn duplicate_legacy_names_require_explicit_distinct_destinations_and_retain_bytes() {
        let fixture = Fixture::new();
        let common = fixture.legacy(RoleSourceScope::Common, "audit", "Common legacy body.\n");
        let project = fixture.legacy(RoleSourceScope::Project, "audit", "Project legacy body.\n");
        let original = [fs::read(&common).unwrap(), fs::read(&project).unwrap()];
        let sources = fixture.sources();
        assert_eq!(sources.len(), 2);
        assert!(
            apply(
                &fixture.home,
                &sources,
                fixture.request(&sources, &["audit", "audit"])
            )
            .is_err()
        );
        assert!(!fixture.home.root().join("roles/audit").exists());
        apply(
            &fixture.home,
            &sources,
            fixture.request(&sources, &["common-audit", "project-audit"]),
        )
        .unwrap();
        let store = InstructionSourceStore::new(&fixture.home);
        assert_eq!(
            store.capture("common-audit").unwrap().role.body,
            "Common legacy body.\n"
        );
        assert_eq!(
            store.capture("project-audit").unwrap().role.body,
            "Project legacy body.\n"
        );
        assert_eq!(fs::read(&common).unwrap(), original[0]);
        assert_eq!(fs::read(&project).unwrap(), original[1]);
    }

    #[test]
    fn explicit_policy_conversion_retains_sources_and_accepted_records() {
        let fixture = Fixture::new();
        let original_role =
            fixture.legacy(RoleSourceScope::Project, "reviewer", "Legacy reviewer.\n");
        let source = fixture.root.join("policy-input.json");
        let input =
            include_bytes!("../docs/protocol/examples/specialist-policy-input-v2.valid.json");
        fs::write(&source, input).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        let accepted = fixture.home.root().join("accepted-state.json");
        let historical = include_bytes!(
            "../docs/protocol/examples/accepted-policy-session-v1-captured.valid.json"
        );
        fs::write(&accepted, historical).unwrap();
        let role_before = fs::read(&original_role).unwrap();
        let output = fixture.root.join("policy-successor.json");
        let sources = fixture.sources();
        let mut request = fixture.request(&sources, &["migrated-reviewer"]);
        request.policies.push(PolicySelection {
            source_file: source.clone(),
            source_sha256: sha256_hex(input),
            destination_file: output.clone(),
            access: vec![PolicyAccessSelection {
                role_ref: "reviewer".to_owned(),
                allowed_access: vec!["read_only".to_owned()],
            }],
        });
        let result = apply(&fixture.home, &sources, request.clone()).unwrap();
        assert_eq!(result["published_policy_count"], 1);
        let policy: PolicyInput = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        policy.validate().unwrap();
        assert_eq!(policy.roles[0].role_source.name, "migrated-reviewer");
        assert_eq!(policy.roles[0].agent_configuration.schema_version, 3);
        assert_eq!(policy.roles[0].allowed_access, ["read_only"]);
        assert_eq!(fs::read(source).unwrap(), input);
        assert_eq!(fs::read(original_role).unwrap(), role_before);
        assert_eq!(fs::read(accepted).unwrap(), historical);
        let bytes = fs::read(&output).unwrap();
        assert!(apply(&fixture.home, &sources, request).is_err());
        assert_eq!(fs::read(&output).unwrap(), bytes);
    }

    #[test]
    fn stale_selection_and_existing_destinations_fail_before_publication() {
        let fixture = Fixture::new();
        fixture.legacy(RoleSourceScope::Common, "audit", "Original body.\n");
        let sources = fixture.sources();
        let stale = fixture.request(&sources, &["new-audit"]);
        fixture.legacy(RoleSourceScope::Common, "audit", "Changed body.\n");
        assert!(apply(&fixture.home, &fixture.sources(), stale).is_err());
        assert!(!fixture.home.root().join("roles/new-audit").exists());
        let sources = fixture.sources();
        let general = fs::read(fixture.home.root().join("roles/general/AGENTS.md")).unwrap();
        assert!(
            apply(
                &fixture.home,
                &sources,
                fixture.request(&sources, &["general"])
            )
            .is_err()
        );
        assert_eq!(
            fs::read(fixture.home.root().join("roles/general/AGENTS.md")).unwrap(),
            general
        );
    }

    #[test]
    fn symlinked_legacy_sources_are_refused_and_publication_never_replaces_an_entry() {
        let fixture = Fixture::new();
        let path = fixture.legacy(RoleSourceScope::Common, "audit", "Body.\n");
        let target = fixture.root.join("unrelated.json");
        fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(enumerate(&fixture.home, &fixture.workspace, &fixture.registry).is_err());
        let store = InstructionSourceStore::new(&fixture.home);
        store
            .publish_role_exclusive("new-audit", "First body.\n")
            .unwrap();
        assert!(
            store
                .publish_role_exclusive("new-audit", "Second body.\n")
                .is_err()
        );
        assert_eq!(
            store.capture("new-audit").unwrap().role.body,
            "First body.\n"
        );
    }
}
