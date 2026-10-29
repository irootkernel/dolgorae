//! Explicit setup and sealed owner provenance for the native host adapter.
//! No hook, delivery or lookup path creates missing configuration or state.

use crate::controller::{CredentialCarrier, OperatorStore};
use crate::darwin::DarwinSystem;
use crate::delegation_identity::CallerScope;
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::run::ControllerBinding;
use crate::workspace::{SystemGitRunner, SystemWorkspacePlatform, WorkspaceService, WorkspaceView};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const MAX_CONFIG: u64 = 131_072;
pub(crate) const LOCK_NAME: &str = "host.lock";

pub(crate) fn refusal() -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "protected host state is unavailable or inconsistent",
        false,
        serde_json::json!({"required_action":"restore_state","effect":"none"}),
    )
}

pub(crate) fn conflict() -> MachineError {
    MachineError::new(
        "IDEMPOTENCY_CONFLICT",
        "native invocation input differs",
        false,
        serde_json::json!({"effect":"none"}),
    )
}

pub(crate) fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, MachineError> {
    let text = serde_json::to_string(value).map_err(|_| refusal())?;
    crate::jcs::canonicalize(&crate::jcs::parse(&text).map_err(|_| refusal())?)
        .map_err(|_| refusal())
}

pub(crate) fn digest<T: Serialize>(value: &T) -> Result<String, MachineError> {
    Ok(format!(
        "sha256:{}",
        crate::jcs::sha256_hex(&canonical(value)?)
    ))
}

pub(crate) fn opaque(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
}

pub(crate) fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HostRoute {
    #[serde(rename = "codex-0.160.0")]
    Codex,
    #[serde(rename = "claude-code-2.1.295")]
    Claude,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostExecutable {
    pub path: PathBuf,
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnerKind {
    External,
    Managed { session_ref: Uuid },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedScope {
    pub kind: String,
    // Null admits the Controller's scopes of this kind, never another owner.
    #[serde(deserialize_with = "Option::deserialize")]
    pub reference: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerDraft {
    pub route: HostRoute,
    pub executable: HostExecutable,
    pub owner: OwnerKind,
    pub controller_generation: u64,
    pub controller_file: PathBuf,
    pub native_host_root: PathBuf,
    pub allowed_scopes: Vec<AllowedScope>,
    pub tools: Vec<String>,
    pub caller_scope: CallerScope,
    pub adapter_id: String,
    pub adapter_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerRecord {
    schema_version: u32,
    workspace_ref: String,
    canonical_workspace: PathBuf,
    controller: ControllerBinding,
    configuration: OwnerDraft,
}

/// A pinned private directory; every relative open checks descriptor identity.
pub(crate) struct PrivateDirectory {
    pub path: PathBuf,
    pub file: File,
}

fn same(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(test)]
thread_local! {
    static STORAGE_FAULT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn test_storage_fault() {
    let fault = STORAGE_FAULT.with(|slot| slot.borrow_mut().take());
    if let Some(fault) = fault {
        fault();
    }
}

#[cfg(test)]
pub(crate) struct StorageFault;
#[cfg(test)]
impl StorageFault {
    pub(crate) fn at(fault: impl FnOnce() + 'static) -> Self {
        STORAGE_FAULT.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(fault));
        });
        Self
    }
}
#[cfg(test)]
impl Drop for StorageFault {
    fn drop(&mut self) {
        STORAGE_FAULT.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn private_file(file: &File) -> Result<(), MachineError> {
    let metadata = file.metadata().map_err(|_| refusal())?;
    private_file_metadata(&metadata, DarwinSystem.current_uid())
}

fn private_file_metadata(metadata: &fs::Metadata, owner: u32) -> Result<(), MachineError> {
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(refusal());
    }
    Ok(())
}

fn private_directory(file: &File) -> Result<(), MachineError> {
    let metadata = file.metadata().map_err(|_| refusal())?;
    private_directory_metadata(&metadata, DarwinSystem.current_uid())
}

fn private_directory_metadata(metadata: &fs::Metadata, owner: u32) -> Result<(), MachineError> {
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o777 != 0o700 {
        return Err(refusal());
    }
    Ok(())
}

impl PrivateDirectory {
    pub fn open(path: &Path) -> Result<Self, MachineError> {
        if !path.is_absolute() {
            return Err(refusal());
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .map_err(|_| refusal())?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    file = DarwinSystem
                        .openat_nofollow(&file, name, true)
                        .map_err(|_| refusal())?;
                }
                _ => return Err(refusal()),
            }
        }
        private_directory(&file)?;
        let root = Self {
            path: path.to_path_buf(),
            file,
        };
        root.verify()?;
        Ok(root)
    }

    pub fn verify(&self) -> Result<(), MachineError> {
        private_directory(&self.file)?;
        if fs::canonicalize(&self.path).map_err(|_| refusal())? != self.path
            || !same(
                &self.file.metadata().map_err(|_| refusal())?,
                &fs::symlink_metadata(&self.path).map_err(|_| refusal())?,
            )
        {
            return Err(refusal());
        }
        Ok(())
    }

    fn parent(&self, path: &Path) -> Result<(File, OsString), MachineError> {
        self.verify()?;
        let relative = path.strip_prefix(&self.path).map_err(|_| refusal())?;
        let mut components: Vec<_> = relative.components().collect();
        let Some(Component::Normal(leaf)) = components.pop() else {
            return Err(refusal());
        };
        let mut directory = self.file.try_clone().map_err(|_| refusal())?;
        for component in components {
            let Component::Normal(name) = component else {
                return Err(refusal());
            };
            directory = DarwinSystem
                .openat_nofollow(&directory, name, true)
                .map_err(|_| refusal())?;
            private_directory(&directory)?;
        }
        Ok((directory, leaf.to_os_string()))
    }

    pub fn open_file(&self, path: &Path) -> Result<File, MachineError> {
        let (directory, leaf) = self.parent(path)?;
        let file = DarwinSystem
            .openat_nofollow(&directory, &leaf, false)
            .map_err(|_| refusal())?;
        private_file(&file)?;
        #[cfg(test)]
        test_storage_fault();
        let identity = DarwinSystem
            .statat_nofollow(&directory, &leaf)
            .map_err(|_| refusal())?;
        let metadata = file.metadata().map_err(|_| refusal())?;
        if identity.device != metadata.dev() || identity.inode != metadata.ino() {
            return Err(refusal());
        }
        self.verify()?;
        Ok(file)
    }

    pub fn create_file(&self, name: &str) -> Result<File, MachineError> {
        if Path::new(name).components().count() != 1 || !opaque(name, 128) {
            return Err(refusal());
        }
        self.verify()?;
        let file = DarwinSystem
            .createat_private(&self.file, OsStr::new(name), true)
            .map_err(|_| refusal())?;
        private_file(&file)?;
        self.file.sync_all().map_err(|_| refusal())?;
        Ok(file)
    }

    pub fn publish(&self, name: &str, bytes: &[u8]) -> Result<(), MachineError> {
        let pending = format!("{name}.pending");
        let mut file = self.create_file(&pending)?;
        file.write_all(bytes).map_err(|_| refusal())?;
        file.sync_all().map_err(|_| refusal())?;
        self.verify()?;
        DarwinSystem
            .renameat_exclusive(&self.file, OsStr::new(&pending), OsStr::new(name))
            .map_err(|_| refusal())?;
        self.file.sync_all().map_err(|_| refusal())?;
        self.open_file(&self.path.join(name))?;
        Ok(())
    }

    pub fn read<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<T, MachineError> {
        self.read_bounded(path, MAX_CONFIG)
    }

    pub(crate) fn read_bounded<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
        maximum: u64,
    ) -> Result<T, MachineError> {
        let mut file = self.open_file(path)?;
        let before = file.metadata().map_err(|_| refusal())?;
        if before.len() > maximum {
            return Err(refusal());
        }
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(maximum.checked_add(1).ok_or_else(refusal)?)
            .read_to_end(&mut bytes)
            .map_err(|_| refusal())?;
        if bytes.len() as u64 > maximum {
            return Err(refusal());
        }
        #[cfg(test)]
        test_storage_fault();
        let after = file.metadata().map_err(|_| refusal())?;
        let named = self.open_file(path)?.metadata().map_err(|_| refusal())?;
        if !same(&before, &after)
            || !same(&after, &named)
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(refusal());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| refusal())?;
        let canonical = crate::jcs::canonicalize(&crate::jcs::parse(text).map_err(|_| refusal())?)
            .map_err(|_| refusal())?;
        serde_json::from_slice(&canonical).map_err(|_| refusal())
    }

    pub fn lock(&self) -> Result<HostLock, MachineError> {
        let file = self.open_file(&self.path.join(LOCK_NAME))?;
        DarwinSystem.lock_exclusive(&file).map_err(|_| refusal())?;
        self.verify()?;
        Ok(HostLock(file))
    }

    pub fn verify_journal(&self, path: &Path) -> Result<(), MachineError> {
        let relative = path.strip_prefix(&self.path).map_err(|_| refusal())?;
        if relative.as_os_str().is_empty() {
            return Err(refusal());
        }
        let mut file = self.file.try_clone().map_err(|_| refusal())?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(refusal());
            };
            file = DarwinSystem
                .openat_nofollow(&file, name, true)
                .map_err(|_| refusal())?;
            private_directory(&file)?;
        }
        // The journal is private identity state, including every retained leaf.
        let mut stack = vec![(path.to_path_buf(), file, 0_u8)];
        let mut count = 0_usize;
        while let Some((directory_path, directory, depth)) = stack.pop() {
            for entry in fs::read_dir(&directory_path).map_err(|_| refusal())? {
                count += 1;
                if count > 10_000 || depth >= 64 {
                    return Err(refusal());
                }
                let entry = entry.map_err(|_| refusal())?;
                let node = DarwinSystem
                    .statat_nofollow(&directory, &entry.file_name())
                    .map_err(|_| refusal())?;
                #[cfg(test)]
                test_storage_fault();
                let is_directory = node.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32;
                let file = DarwinSystem
                    .openat_nofollow(&directory, &entry.file_name(), is_directory)
                    .map_err(|_| refusal())?;
                let metadata = file.metadata().map_err(|_| refusal())?;
                if metadata.dev() != node.device || metadata.ino() != node.inode {
                    return Err(refusal());
                }
                if is_directory {
                    private_directory(&file)?;
                    stack.push((entry.path(), file, depth + 1));
                } else {
                    private_file(&file)?;
                }
            }
        }
        self.verify()
    }
}

pub(crate) struct HostLock(File);
impl Drop for HostLock {
    fn drop(&mut self) {
        let _ = DarwinSystem.unlock(&self.0);
    }
}

/// Constructed only from a protected configuration and its live owner carrier.
pub struct VerifiedOwner {
    pub(crate) root: PrivateDirectory,
    pub(crate) path: PathBuf,
    pub(crate) view: WorkspaceView,
    pub(crate) state_root: PathBuf,
    pub(crate) controller: ControllerBinding,
    pub(crate) configuration: OwnerDraft,
    pub(crate) configuration_digest: String,
}

fn validate_draft(
    root: &PrivateDirectory,
    view: &WorkspaceView,
    draft: &OwnerDraft,
) -> Result<(), MachineError> {
    draft.caller_scope.validate().map_err(|_| refusal())?;
    let expected = match draft.route {
        HostRoute::Codex => (
            "0.164.0",
            "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf",
        ),
        HostRoute::Claude => (
            "2.1.310",
            "ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd",
        ),
    };
    let workspace = view.canonical_path.to_path_buf()?;
    if root.path.starts_with(&workspace)
        || workspace.starts_with(&root.path)
        || !draft.executable.path.is_absolute()
        || draft.executable.version != expected.0
        || draft.executable.sha256 != expected.1
        || !(1..=9_007_199_254_740_991).contains(&draft.controller_generation)
        || draft.adapter_id
            != match draft.owner {
                OwnerKind::External => "dolgorae.external/1",
                OwnerKind::Managed { .. } => "dolgorae.managed/1",
            }
        || draft.adapter_version != env!("CARGO_PKG_VERSION")
        || matches!(draft.owner, OwnerKind::Managed { session_ref } if session_ref.get_version_num() != 7 || session_ref.get_variant() != uuid::Variant::RFC4122)
        || draft.tools.is_empty()
        || draft.tools.len() > 32
        || draft.tools.iter().any(|tool| !opaque(tool, 128))
        || draft
            .tools
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != draft.tools.len()
        || draft.allowed_scopes.is_empty()
        || draft.allowed_scopes.len() > 64
    {
        return Err(refusal());
    }
    for scope in &draft.allowed_scopes {
        if scope
            .reference
            .as_ref()
            .is_some_and(|reference| !opaque(reference, 512))
            || !match &draft.owner {
                OwnerKind::External => {
                    scope.kind == "external_engagement"
                        || (scope.kind == "workspace"
                            && scope.reference.as_deref() == Some(&view.workspace_id))
                }
                OwnerKind::Managed { session_ref } => {
                    scope.kind == "orchestrated_session"
                        && scope.reference.as_deref() == Some(session_ref.to_string().as_str())
                }
            }
        {
            return Err(refusal());
        }
    }
    root.open_file(&draft.controller_file)?;
    root.verify_journal(&draft.native_host_root)
}

pub(crate) fn authenticate_owner(
    state_root: &Path,
    draft: &OwnerDraft,
    binding: &ControllerBinding,
    workspace: &str,
) -> Result<(), MachineError> {
    if !matches!(
        binding.identity.kind,
        crate::domain::ControllerKind::HumanCli
            | crate::domain::ControllerKind::InteractiveClient
            | crate::domain::ControllerKind::Automation
            | crate::domain::ControllerKind::WorkflowOrchestrator
    ) {
        return Err(refusal());
    }
    let carrier = CredentialCarrier::open_path(&draft.controller_file)
        .map_err(|_| refusal())?
        .with_expected_generation(binding.identity.generation);
    crate::controller::authorize_controller(Uuid::nil(), "trusted_host", binding, &carrier)
        .map_err(|_| refusal())?;
    if let OwnerKind::Managed { session_ref } = draft.owner {
        let observer = crate::orchestration::OrchestrationStore::open_observer(state_root)?;
        let session = observer.session(session_ref)?;
        let manifest = crate::run::RunStore::new(SystemWorkspacePlatform, state_root)
            .load_manifest(session.root_run_id)?;
        let current =
            crate::controller::load_reconciled_controller_binding(state_root, session.root_run_id)?;
        if &current != binding
            || (current.identity.generation == manifest.controller.identity.generation
                && current != manifest.controller)
            || manifest.workspace_id != workspace
            || session.workspace_id != workspace
            || session.root_run_id != session_ref
        {
            return Err(refusal());
        }
        if !manifest
            .aggregate_binding
            .as_ref()
            .is_some_and(|aggregate| {
                aggregate.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                    && aggregate.aggregate_id == session_ref
                    && aggregate.member_kind == crate::run::AggregateMemberKind::Primary
                    && aggregate.operation_id == session.bootstrap_operation_id
                    && aggregate.policy_sha256.as_deref() == Some(&session.specialist_policy_sha256)
            })
        {
            return Err(refusal());
        }
    }
    Ok(())
}

impl VerifiedOwner {
    pub fn load(home: &DolgoraeHome, path: &Path) -> Result<Self, MachineError> {
        if path.file_name() != Some(OsStr::new("owner.json")) {
            return Err(refusal());
        }
        let root = PrivateDirectory::open(path.parent().ok_or_else(refusal)?)?;
        let record: OwnerRecord = root.read(path)?;
        let view = WorkspaceService::new(
            SystemWorkspacePlatform,
            SystemGitRunner,
            home.root().to_path_buf(),
        )
        .discover(Some(&record.canonical_workspace))?;
        if record.schema_version != 1
            || record.workspace_ref != view.workspace_id
            || record.canonical_workspace != view.canonical_path.to_path_buf()?
            || record.controller.identity.generation != record.configuration.controller_generation
        {
            return Err(refusal());
        }
        validate_draft(&root, &view, &record.configuration)?;
        let state_root = home.workspace_root(&view.workspace_id);
        authenticate_owner(
            &state_root,
            &record.configuration,
            &record.controller,
            &view.workspace_id,
        )?;
        Ok(Self {
            configuration_digest: digest(&record)?,
            root,
            path: path.to_path_buf(),
            view,
            state_root,
            controller: record.controller,
            configuration: record.configuration,
        })
    }

    pub(crate) fn revalidate(&self) -> Result<(), MachineError> {
        let record: OwnerRecord = self.root.read(&self.path)?;
        if digest(&record)? != self.configuration_digest
            || record.configuration != self.configuration
            || record.controller != self.controller
            || record.workspace_ref != self.view.workspace_id
            || record.canonical_workspace != self.view.canonical_path.to_path_buf()?
        {
            return Err(refusal());
        }
        validate_draft(&self.root, &self.view, &self.configuration)?;
        authenticate_owner(
            &self.state_root,
            &self.configuration,
            &self.controller,
            &self.view.workspace_id,
        )
    }

    pub fn permit(&self, scope: Option<(&str, &str)>) -> Result<(), MachineError> {
        let (kind, reference) = scope.unwrap_or(("workspace", &self.view.workspace_id));
        if !self.configuration.allowed_scopes.iter().any(|allowed| {
            allowed.kind == kind
                && allowed
                    .reference
                    .as_deref()
                    .is_none_or(|value| value == reference)
        }) {
            return Err(refusal());
        }
        Ok(())
    }

    pub fn ready(self) -> Result<crate::host_invocation::HostStore, MachineError> {
        crate::host_invocation::HostStore::open(self)
    }
}

fn create_owner(
    home: &DolgoraeHome,
    workspace: &Path,
    path: &Path,
    draft: OwnerDraft,
    carrier: &CredentialCarrier,
) -> Result<(), MachineError> {
    if path.file_name() != Some(OsStr::new("owner.json")) {
        return Err(refusal());
    }
    let root = PrivateDirectory::open(path.parent().ok_or_else(refusal)?)?;
    let view = WorkspaceService::new(
        SystemWorkspacePlatform,
        SystemGitRunner,
        home.root().to_path_buf(),
    )
    .discover(Some(workspace))?;
    validate_draft(&root, &view, &draft)?;
    let controller = crate::controller::binding_from_carrier(carrier, draft.controller_generation)
        .map_err(|_| refusal())?;
    authenticate_owner(
        &home.workspace_root(&view.workspace_id),
        &draft,
        &controller,
        &view.workspace_id,
    )?;
    let lock_path = root.path.join(LOCK_NAME);
    if !lock_path.try_exists().map_err(|_| refusal())? {
        if path.try_exists().map_err(|_| refusal())? {
            return Err(refusal());
        }
        match DarwinSystem.createat_private(&root.file, OsStr::new(LOCK_NAME), true) {
            Ok(file) => {
                private_file(&file)?;
                root.file.sync_all().map_err(|_| refusal())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(refusal()),
        }
    }
    let _lock = root.lock()?;
    let record = OwnerRecord {
        schema_version: 1,
        workspace_ref: view.workspace_id,
        canonical_workspace: view.canonical_path.to_path_buf()?,
        controller,
        configuration: draft,
    };
    if path.try_exists().map_err(|_| refusal())? {
        let old: OwnerRecord = root.read(path)?;
        return if old == record {
            Ok(())
        } else {
            Err(conflict())
        };
    }
    let bytes = canonical(&record)?;
    root.publish("owner.json", &bytes)?;
    root.open_file(path)?;
    root.verify()
}

/// Administrative errors expose a bounded code, never credential/input/path text.
pub fn administrative_error_code(error: &MachineError) -> &str {
    if !error.code.is_empty()
        && error.code.len() <= 64
        && error
            .code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        &error.code
    } else {
        "INTERNAL_ERROR"
    }
}

/// Private administrative dispatcher. Never writes a Machine/facade receipt.
pub fn run_admin(args: &[OsString]) -> Result<(), MachineError> {
    run_admin_at(args, None)
}

fn inherited_fd(value: &OsStr) -> Result<i32, MachineError> {
    let value = value
        .to_str()
        .ok_or_else(refusal)?
        .parse::<i32>()
        .map_err(|_| refusal())?;
    if value < 0 {
        return Err(refusal());
    }
    Ok(value)
}

fn recovery_reference(value: &OsStr) -> Result<Uuid, MachineError> {
    let text = value.to_str().ok_or_else(refusal)?;
    crate::delegation_identity::deserialize_invocation_ref(serde::de::value::StrDeserializer::<
        serde::de::value::Error,
    >::new(text))
    .map_err(|_| refusal())
}

fn run_admin_at(args: &[OsString], test_home: Option<&DolgoraeHome>) -> Result<(), MachineError> {
    let operation = args
        .first()
        .and_then(|value| value.to_str())
        .ok_or_else(refusal)?;
    let mut values = std::collections::BTreeMap::new();
    let (pairs, remainder) = args[1..].as_chunks::<2>();
    for pair in pairs {
        let key = pair[0].to_str().ok_or_else(refusal)?;
        if !matches!(
            key,
            "--workspace"
                | "--owner-config"
                | "--request"
                | "--output"
                | "--operator-fd"
                | "--controller-fd"
                | "--scope-ref"
                | "--member-ref"
                | "--task-ref"
        ) || values.insert(key, &pair[1]).is_some()
        {
            return Err(refusal());
        }
    }
    if !remainder.is_empty() {
        return Err(refusal());
    }
    let get = |key| values.get(key).copied().ok_or_else(refusal);
    let fd = |key| inherited_fd(get(key)?);
    // Duplicate inherited authority before opening anything that might reuse a bad FD.
    let operator = CredentialCarrier::duplicate_fd(fd("--operator-fd")?)?;
    let controller = CredentialCarrier::duplicate_fd(fd("--controller-fd")?)?;
    private_file(&File::from(
        DarwinSystem
            .duplicate_fd_cloexec(operator.raw_fd())
            .map_err(|_| refusal())?,
    ))?;
    private_file(&File::from(
        DarwinSystem
            .duplicate_fd_cloexec(controller.raw_fd())
            .map_err(|_| refusal())?,
    ))?;
    let system_home;
    let home = match test_home {
        Some(home) => home,
        None => {
            system_home = DolgoraeHome::system().map_err(|_| refusal())?;
            &system_home
        }
    };
    let authorization = OperatorStore::new(home.operator_root()).authorize(&operator)?;
    #[cfg(test)]
    assert!(
        OperatorStore::new(home.operator_root())
            .try_lock_exclusive()?
            .is_none(),
        "setup publication lost the Operator authorization hold"
    );
    let path = Path::new(get("--owner-config")?);
    match operation {
        "owner-config-create" if values.len() == 5 => {
            let request = Path::new(get("--request")?);
            let root = PrivateDirectory::open(request.parent().ok_or_else(refusal)?)?;
            let draft: OwnerDraft = root.read(request)?;
            create_owner(
                home,
                Path::new(get("--workspace")?),
                path,
                draft,
                &controller,
            )?;
        }
        "namespace-setup" | "host-state-init" if values.len() == 3 => {
            let owner = VerifiedOwner::load(home, path)?;
            crate::controller::authorize_controller(
                Uuid::nil(),
                "trusted_host_setup",
                &owner.controller,
                &controller,
            )?;
            let _lock = owner.root.lock()?;
            if operation == "namespace-setup" {
                crate::engagement::ensure_delegation_namespace(&owner.state_root)?;
            } else {
                crate::host_invocation::initialize(&owner)?;
            }
        }
        "member-recover" if values.len() == 6 => {
            let owner = VerifiedOwner::load(home, path)?;
            administrative_member_recover(
                &owner,
                &controller,
                recovery_reference(get("--scope-ref")?)?,
                recovery_reference(get("--member-ref")?)?,
                recovery_reference(get("--task-ref")?)?,
            )?;
        }
        "managed-connect" if values.len() == 3 => {
            let owner = VerifiedOwner::load(home, path)?;
            crate::controller::authorize_controller(
                Uuid::nil(),
                "trusted_host_setup",
                &owner.controller,
                &controller,
            )?;
            crate::managed_specialist::connect(&owner)?;
        }
        "consumer-init" if values.len() == 4 => {
            let owner = VerifiedOwner::load(home, path)?;
            crate::controller::authorize_controller(
                Uuid::nil(),
                "trusted_host_setup",
                &owner.controller,
                &controller,
            )?;
            let request = Path::new(get("--request")?);
            let root = PrivateDirectory::open(request.parent().ok_or_else(refusal)?)?;
            let mode: crate::native_consumer::Mode = root.read(request)?;
            crate::native_consumer::initialize(&owner, mode)?;
        }
        "preparation-init" if values.len() == 3 => {
            let owner = VerifiedOwner::load(home, path)?;
            crate::controller::authorize_controller(
                Uuid::nil(),
                "trusted_host_setup",
                &owner.controller,
                &controller,
            )?;
            crate::source_binding::initialize(&owner)?;
        }
        "provisioning-prepare" | "assignment-prepare" | "preparation-project"
            if values.len() == 5 =>
        {
            let owner = VerifiedOwner::load(home, path)?;
            crate::controller::authorize_controller(
                Uuid::nil(),
                "trusted_host_setup",
                &owner.controller,
                &controller,
            )?;
            crate::source_binding::projection_destination(&owner, Path::new(get("--output")?))?;
            let request = Path::new(get("--request")?);
            let input = PrivateDirectory::open(request.parent().ok_or_else(refusal)?)?;
            let projection = match operation {
                "provisioning-prepare" => {
                    serde_json::to_value(crate::source_binding::prepare_provisioning(
                        home,
                        &owner,
                        input.read_bounded(request, crate::source_binding::selection_limit())?,
                    )?)
                    .map_err(|_| refusal())?
                }
                "assignment-prepare" => {
                    serde_json::to_value(crate::source_binding::prepare_assignment(
                        &owner,
                        input.read_bounded(request, crate::source_binding::assignment_limit())?,
                    )?)
                    .map_err(|_| refusal())?
                }
                _ => crate::source_binding::project(
                    &owner,
                    input.read_bounded(request, crate::source_binding::public_limit())?,
                )?,
            };
            crate::source_binding::write_projection(
                &owner,
                Path::new(get("--output")?),
                &projection,
            )?;
        }
        _ => return Err(refusal()),
    }
    authorization.release();
    Ok(())
}

/// The local administrative boundary carries no native invocation or new key.
fn administrative_member_recover(
    owner: &VerifiedOwner,
    carrier: &CredentialCarrier,
    scope_ref: Uuid,
    member_ref: Uuid,
    task_ref: Uuid,
) -> Result<(), MachineError> {
    crate::controller::authorize_controller(
        Uuid::nil(),
        "member_recover",
        &owner.controller,
        carrier,
    )?;
    let kind = match owner.configuration.owner {
        OwnerKind::External => "external_engagement",
        OwnerKind::Managed { session_ref } if session_ref == scope_ref => "orchestrated_session",
        _ => return Err(refusal()),
    };
    owner.permit(Some((kind, &scope_ref.to_string())))?;
    let scope = crate::delegation_service::DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    let original = match owner.configuration.owner {
        OwnerKind::Managed { session_ref } => {
            Some(crate::delegation_service::retained_managed_recovery(
                &scope,
                session_ref,
                member_ref,
                task_ref,
            )?)
        }
        OwnerKind::External => None,
    };
    owner.revalidate()?;
    let port = crate::semantic::SemanticRunSettlementPort { carrier };
    match original {
        Some(original) => crate::delegation_service::reconcile_managed_member_task(
            &scope,
            &original.context(),
            &port,
            member_ref,
            task_ref,
        )?,
        None => crate::delegation_service::reconcile_external_member_task(
            &scope, &port, scope_ref, member_ref, task_ref,
        )?,
    };
    Ok(())
}

pub(crate) struct ConfiguredCallerPort<'a>(pub &'a VerifiedOwner);
impl crate::caller_scope::sealed::Sealed for ConfiguredCallerPort<'_> {}
impl crate::caller_scope::TrustedCallerPort for ConfiguredCallerPort<'_> {
    fn load(&self) -> Result<([String; 5], u64), MachineError> {
        let s = &self.0.configuration.caller_scope;
        Ok((
            [
                s.binding_ref.clone(),
                s.podway_workspace_ref.clone(),
                s.podway_session_ref.clone(),
                s.podway_attempt_ref.clone(),
                s.delegated_placement_ref.clone(),
            ],
            s.max_rounds,
        ))
    }
}

impl HostRoute {
    pub(crate) fn identifier(self) -> &'static str {
        match self {
            Self::Codex => "codex-0.160.0",
            Self::Claude => "claude-code-2.1.295",
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::domain::ControllerKind;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    pub(crate) fn administrative(
        args: &[OsString],
        home: &DolgoraeHome,
    ) -> Result<(), MachineError> {
        run_admin_at(args, Some(home))
    }

    pub(crate) struct Fixture {
        pub root: PathBuf,
        pub home: DolgoraeHome,
        pub workspace: PathBuf,
        pub private: PathBuf,
        pub operator: PathBuf,
        pub controller: PathBuf,
        pub draft: OwnerDraft,
    }
    impl Fixture {
        pub fn new(route: HostRoute) -> Self {
            let root = std::env::temp_dir().join(format!("dolgorae-host-{}", Uuid::now_v7()));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let root = root.canonicalize().unwrap();
            let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
            let workspace = root.join("workspace");
            let private = root.join("private");
            for path in [&workspace, &private, &private.join("native")] {
                fs::create_dir(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            WorkspaceService::new(
                SystemWorkspacePlatform,
                SystemGitRunner,
                home.root().to_path_buf(),
            )
            .initialize(Some(&workspace), crate::workspace::WorkspaceMode::NonGit)
            .unwrap();
            let operator = private.join("operator.json");
            OperatorStore::new(home.operator_root())
                .initialize(&operator)
                .unwrap();
            let controller = private.join("controller.json");
            crate::controller::create_controller_credential(
                &controller,
                ControllerKind::Automation,
                "host-fixture".to_owned(),
                None,
                None,
            )
            .unwrap();
            let (version, sha256) = match route {
                HostRoute::Codex => (
                    "0.164.0",
                    "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf",
                ),
                HostRoute::Claude => (
                    "2.1.310",
                    "ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd",
                ),
            };
            let draft = OwnerDraft {
                route,
                executable: HostExecutable {
                    path: private.join("native-host"),
                    version: version.to_owned(),
                    sha256: sha256.to_owned(),
                },
                owner: OwnerKind::External,
                controller_generation: 1,
                controller_file: controller.clone(),
                native_host_root: private.join("native"),
                allowed_scopes: vec![
                    AllowedScope {
                        kind: "workspace".to_owned(),
                        reference: Some(crate::workspace::workspace_id(&workspace)),
                    },
                    AllowedScope {
                        kind: "external_engagement".to_owned(),
                        reference: None,
                    },
                ],
                tools: vec!["specialist".to_owned()],
                caller_scope: CallerScope {
                    binding_ref: "binding".to_owned(),
                    podway_workspace_ref: "pw-workspace".to_owned(),
                    podway_session_ref: "pw-session".to_owned(),
                    podway_attempt_ref: "pw-attempt".to_owned(),
                    delegated_placement_ref: "placement".to_owned(),
                    max_rounds: 4,
                },
                adapter_id: "dolgorae.external/1".to_owned(),
                adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
            };
            Self {
                root,
                home,
                workspace,
                private,
                operator,
                controller,
                draft,
            }
        }
        pub(crate) fn for_recovery(
            home: DolgoraeHome,
            workspace: PathBuf,
            carrier: &CredentialCarrier,
            owner: crate::delegation_service::SettlementOwner,
        ) -> Self {
            let root = home.root().parent().unwrap().to_path_buf();
            let private = root.join("private");
            for path in [&private, &private.join("native")] {
                fs::create_dir(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let operator = private.join("operator.json");
            OperatorStore::new(home.operator_root())
                .initialize(&operator)
                .unwrap();
            let controller = private.join("controller.json");
            let fd = DarwinSystem.duplicate_fd_cloexec(carrier.raw_fd()).unwrap();
            let mut file = File::from(fd);
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            fs::write(&controller, bytes).unwrap();
            fs::set_permissions(&controller, fs::Permissions::from_mode(0o600)).unwrap();
            let binding = crate::controller::binding_from_carrier(carrier, 1).unwrap();
            let managed = matches!(
                owner,
                crate::delegation_service::SettlementOwner::Managed { .. }
            );
            let f = Self {
                root,
                home,
                workspace,
                private: private.clone(),
                operator,
                controller: controller.clone(),
                draft: OwnerDraft {
                    route: HostRoute::Codex,
                    executable: HostExecutable {
                        path: private.join("not-launched"),
                        version: "0.164.0".into(),
                        sha256: "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf"
                            .into(),
                    },
                    owner: if managed {
                        OwnerKind::Managed {
                            session_ref: owner.scope_ref(),
                        }
                    } else {
                        OwnerKind::External
                    },
                    controller_generation: binding.identity.generation,
                    controller_file: controller,
                    native_host_root: private.join("native"),
                    allowed_scopes: vec![AllowedScope {
                        kind: if managed {
                            "orchestrated_session"
                        } else {
                            "external_engagement"
                        }
                        .into(),
                        reference: Some(owner.scope_ref().to_string()),
                    }],
                    tools: vec!["specialist".into()],
                    caller_scope: crate::caller_scope::fixture(3).accepted().clone(),
                    adapter_id: if managed {
                        "dolgorae.managed/1"
                    } else {
                        "dolgorae.external/1"
                    }
                    .into(),
                    adapter_version: env!("CARGO_PKG_VERSION").into(),
                },
            };
            f.create().unwrap();
            f
        }

        pub(crate) fn recover(
            &self,
            scope: Uuid,
            member: Uuid,
            task: Uuid,
        ) -> Result<(), MachineError> {
            let operator = File::open(&self.operator).unwrap();
            let controller = File::open(&self.controller).unwrap();
            let args: Vec<OsString> = vec![
                "member-recover".into(),
                "--owner-config".into(),
                self.path().into_os_string(),
                "--scope-ref".into(),
                scope.to_string().into(),
                "--member-ref".into(),
                member.to_string().into(),
                "--task-ref".into(),
                task.to_string().into(),
                "--operator-fd".into(),
                operator.as_raw_fd().to_string().into(),
                "--controller-fd".into(),
                controller.as_raw_fd().to_string().into(),
            ];
            if let Some(binary) = std::env::var_os("DOLGORAE_MEMBER_RECOVERY_TEST_BINARY") {
                let output=std::process::Command::new("python3").args(["-c","import subprocess,sys; o=open(sys.argv[2],'rb');c=open(sys.argv[3],'rb');a=sys.argv[4:];a[a.index('--operator-fd')+1]=str(o.fileno());a[a.index('--controller-fd')+1]=str(c.fileno());p=subprocess.run([sys.argv[1],'__trusted-specialist-host',*a],pass_fds=(o.fileno(),c.fileno()),capture_output=True);sys.stdout.buffer.write(p.stdout);sys.stderr.buffer.write(p.stderr);sys.exit(p.returncode)"]).arg(binary).arg(&self.operator).arg(&self.controller).args(&args).env("HOME",self.home.root().parent().unwrap()).env_remove("CODEX_HOME").output().unwrap();
                assert!(output.stdout.is_empty(), "private recovery leaked stdout");
                if output.status.success() {
                    assert!(output.stderr.is_empty());
                    Ok(())
                } else {
                    let code = String::from_utf8(output.stderr).unwrap();
                    assert!(
                        code.len() < 66
                            && code
                                .trim()
                                .bytes()
                                .all(|c| c.is_ascii_uppercase() || c == b'_')
                    );
                    Err(MachineError::new(
                        code.trim(),
                        "cold recovery refused",
                        false,
                        serde_json::json!({}),
                    ))
                }
            } else {
                run_admin_at(&args, Some(&self.home))
            }
        }

        pub fn path(&self) -> PathBuf {
            self.private.join("owner.json")
        }
        pub fn owner(&self) -> VerifiedOwner {
            VerifiedOwner::load(&self.home, &self.path()).unwrap()
        }
        pub fn create(&self) -> Result<(), MachineError> {
            let operator = CredentialCarrier::open_path(&self.operator)?;
            let hold = OperatorStore::new(self.home.operator_root()).authorize(&operator)?;
            let result = create_owner(
                &self.home,
                &self.workspace,
                &self.path(),
                self.draft.clone(),
                &CredentialCarrier::open_path(&self.controller)?,
            );
            assert!(
                OperatorStore::new(self.home.operator_root())
                    .try_lock_exclusive()?
                    .is_none()
            );
            hold.release();
            result
        }
        pub fn namespace(&self) -> Uuid {
            crate::engagement::ensure_delegation_namespace(&self.owner().state_root).unwrap()
        }
        pub fn initialize(&self) -> Result<(), MachineError> {
            let owner = self.owner();
            let _lock = owner.root.lock()?;
            crate::host_invocation::initialize(&owner)
        }
        pub fn ready(&self) -> crate::host_invocation::HostStore {
            self.owner().ready().unwrap()
        }
        pub fn complete(&self) {
            self.create().unwrap();
            self.namespace();
            self.initialize().unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn administrative_recovery_reference_corpus_uses_the_frozen_uuid_contract() {
        let canonical = "01234567-89ab-7cde-8abc-0123456789ab";
        assert_eq!(
            recovery_reference(OsStr::new(canonical))
                .unwrap()
                .to_string(),
            canonical
        );
        for value in [
            "",
            "not-a-uuid",
            "0123456789ab7cde8abc0123456789ab",
            "01234567-89AB-7CDE-8ABC-0123456789AB",
            "{01234567-89ab-7cde-8abc-0123456789ab}",
            "urn:uuid:01234567-89ab-7cde-8abc-0123456789ab",
        ] {
            assert!(recovery_reference(OsStr::new(value)).is_err(), "{value}");
        }
        for version in b"0123456789abcdef" {
            let mut value = canonical.as_bytes().to_vec();
            value[14] = *version;
            assert_eq!(
                recovery_reference(OsStr::new(std::str::from_utf8(&value).unwrap())).is_ok(),
                *version == b'7'
            );
        }
        for variant in b"0123456789abcdef" {
            let mut value = canonical.as_bytes().to_vec();
            value[19] = *variant;
            assert_eq!(
                recovery_reference(OsStr::new(std::str::from_utf8(&value).unwrap())).is_ok(),
                b"89ab".contains(variant)
            );
        }
        use std::os::unix::ffi::OsStrExt as _;
        assert!(recovery_reference(OsStr::from_bytes(&[0xff])).is_err());
    }

    #[test]
    fn managed_owner_configuration_follows_current_root_authority_for_recovery() {
        let mut f = managed_fixture(HostRoute::Codex);
        f.create().unwrap();
        let OwnerKind::Managed { session_ref } = f.draft.owner else {
            panic!("managed")
        };
        let original = fs::read(f.path()).unwrap();
        let replacement = f.private.join("current-root.json");
        crate::controller::create_controller_credential(
            &replacement,
            ControllerKind::HumanCli,
            "new-root-owner".into(),
            None,
            None,
        )
        .unwrap();
        let binding = crate::controller::binding_from_carrier(
            &CredentialCarrier::open_path(&replacement).unwrap(),
            2,
        )
        .unwrap();
        let state = f.owner().state_root;
        fs::write(
            crate::run::run_root(&state, session_ref).join("controller.json"),
            canonical(&binding).unwrap(),
        )
        .unwrap();
        assert!(
            VerifiedOwner::load(&f.home, &f.path()).is_err(),
            "old protected owner cannot retain authority after Root reset"
        );
        let retained = f.private.join("original-owner.json");
        fs::rename(f.path(), &retained).unwrap();
        f.draft.controller_generation = 2;
        f.draft.controller_file = replacement.clone();
        f.controller = replacement;
        f.create().unwrap();
        assert_eq!(f.owner().controller, binding);
        assert_eq!(fs::read(retained).unwrap(), original);
    }

    #[test]
    fn member_recover_authenticates_and_checks_raw_arguments_before_effect() {
        for managed in [false, true] {
            let mut scene = crate::semantic::successor_review_tests::Scene::new(managed);
            let admin = scene.administrative_fixture();
            scene.prepare_worker_ledger();
            let task = scene.accept_administrative_task("unrecorded-original", false);
            let operator = File::open(&admin.operator).unwrap();
            let controller = File::open(&admin.controller).unwrap();
            let valid: Vec<OsString> = vec![
                "member-recover".into(),
                "--owner-config".into(),
                admin.path().into_os_string(),
                "--scope-ref".into(),
                scene.owner().scope_ref().to_string().into(),
                "--member-ref".into(),
                scene.manifest().run_id.to_string().into(),
                "--task-ref".into(),
                task.task_ref.to_string().into(),
                "--operator-fd".into(),
                operator.as_raw_fd().to_string().into(),
                "--controller-fd".into(),
                controller.as_raw_fd().to_string().into(),
            ];
            assert_eq!(
                run_admin_at(&valid, Some(&admin.home)).unwrap_err().details["reason"],
                "task_turn_unrecorded"
            );
            let before = scene.stability_facts();
            let other = Fixture::new(HostRoute::Codex);
            let foreign_operator = File::open(&other.operator).unwrap();
            let foreign_controller = File::open(&other.controller).unwrap();
            let original_config = fs::read(admin.path()).unwrap();
            for case in 0..18 {
                let mut args = valid.clone();
                match case {
                    0 => args[10] = foreign_operator.as_raw_fd().to_string().into(),
                    1 => args[12] = foreign_controller.as_raw_fd().to_string().into(),
                    2 => args[10] = "-1".into(),
                    3 => args[12] = "-1".into(),
                    4 => {
                        args.pop();
                    }
                    5 => args.extend(["--task-ref".into(), task.task_ref.to_string().into()]),
                    6 => args.extend(["--unknown".into(), "value".into()]),
                    7 => {
                        args.drain(7..9);
                    }
                    8 => args[4] = scene.owner().scope_ref().simple().to_string().into(),
                    9 => args[6] = scene.manifest().run_id.to_string().to_uppercase().into(),
                    10 => args[8] = task.task_ref.urn().to_string().into(),
                    11 => args[8] = Uuid::nil().to_string().into(),
                    12 => args[8] = "not-a-uuid".into(),
                    13 => fs::set_permissions(admin.path(), fs::Permissions::from_mode(0o644))
                        .unwrap(),
                    14 => {
                        let path = admin.private.join("alias.json");
                        symlink(admin.path(), &path).unwrap();
                        args[2] = path.into_os_string();
                    }
                    15 => args[2] = other.path().into_os_string(),
                    16 => args.extend([
                        "--output".into(),
                        admin.private.join("unused").into_os_string(),
                    ]),
                    _ => {
                        let mut record: OwnerRecord =
                            serde_json::from_slice(&fs::read(admin.path()).unwrap()).unwrap();
                        record.configuration.allowed_scopes[0].reference =
                            Some(Uuid::now_v7().to_string());
                        fs::write(admin.path(), canonical(&record).unwrap()).unwrap();
                    }
                }
                let error = run_admin_at(&args, Some(&admin.home)).unwrap_err();
                assert_ne!(
                    error.details["reason"], "task_turn_unrecorded",
                    "case {case}: authority/grammar must refuse before the member port"
                );
                assert_eq!(scene.stability_facts(), before, "case {case}");
                if case == 17 {
                    fs::write(admin.path(), &original_config).unwrap();
                }
                if case == 13 {
                    fs::set_permissions(admin.path(), fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            let owner = admin.owner();
            let mut changed: OwnerRecord = serde_json::from_slice(&original_config).unwrap();
            changed.configuration.caller_scope.podway_attempt_ref =
                "changed-after-original-capture".into();
            fs::write(admin.path(), canonical(&changed).unwrap()).unwrap();
            let error = administrative_member_recover(
                &owner,
                &CredentialCarrier::open_path(&admin.controller).unwrap(),
                scene.owner().scope_ref(),
                scene.manifest().run_id,
                task.task_ref,
            )
            .unwrap_err();
            assert_ne!(
                error.details["reason"], "task_turn_unrecorded",
                "owner config must be revalidated before effect"
            );
            assert_eq!(scene.stability_facts(), before);
            fs::write(admin.path(), &original_config).unwrap();
            assert_eq!(
                run_admin_at(&valid, Some(&admin.home)).unwrap_err().details["reason"],
                "task_turn_unrecorded"
            );
            assert_eq!(scene.stability_facts(), before);
        }
    }

    #[test]
    fn bounded_private_read_refuses_mutation_after_bytes_before_identity_check() {
        let f = Fixture::new(HostRoute::Codex);
        let directory = PrivateDirectory::open(&f.private).unwrap();
        let path = f.private.join("bounded.json");
        fs::write(&path, b"{\"proof\":1}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let changed = path.clone();
        let _fault = StorageFault::at(move || {
            STORAGE_FAULT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    fs::write(changed, b"{\"proof\":2}").unwrap();
                }))
            });
        });
        assert!(
            directory
                .read_bounded::<serde_json::Value>(&path, 100)
                .is_err(),
            "proof file changed after its bytes were read"
        );
        assert_eq!(fs::read(path).unwrap(), b"{\"proof\":2}");
    }

    #[test]
    fn kernel_identity_corpus_refuses_leaf_and_journal_replacement_between_observations() {
        for journal in [false, true] {
            let f = Fixture::new(HostRoute::Claude);
            let directory = PrivateDirectory::open(&f.private).unwrap();
            let path = f
                .private
                .join(if journal { "native/journal" } else { "leaf" });
            fs::write(&path, b"original private bytes").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let swapped = path.clone();
            let _fault = StorageFault::at(move || {
                fs::rename(&swapped, swapped.with_extension("retained")).unwrap();
                fs::write(&swapped, b"replacement private bytes").unwrap();
                fs::set_permissions(&swapped, fs::Permissions::from_mode(0o600)).unwrap();
            });
            if journal {
                assert!(
                    directory.verify_journal(&f.private.join("native")).is_err(),
                    "journal inode changed after stat"
                );
            } else {
                assert!(
                    directory.open_file(&path).is_err(),
                    "file inode changed after open"
                );
            }
            assert_eq!(fs::read(&path).unwrap(), b"replacement private bytes");
        }
    }

    #[test]
    fn administrative_argument_grammar_corpus_rejects_ambiguous_authority() {
        use std::os::fd::AsRawFd as _;
        for text in ["-1", "-2147483648", "2147483648", "", "x", "1.0"] {
            assert!(
                inherited_fd(OsStr::new(text)).is_err(),
                "invalid inherited fd {text:?}"
            );
        }
        for value in [0, 1, i32::MAX] {
            assert_eq!(inherited_fd(OsStr::new(&value.to_string())).unwrap(), value);
        }
        for malformed in ["duplicate", "trailing", "unknown"] {
            let f = Fixture::new(HostRoute::Codex);
            let request = f.private.join("request.json");
            fs::write(&request, canonical(&f.draft).unwrap()).unwrap();
            fs::set_permissions(&request, fs::Permissions::from_mode(0o600)).unwrap();
            let operator = File::open(&f.operator).unwrap();
            let controller = File::open(&f.controller).unwrap();
            let mut args = vec![
                OsString::from("owner-config-create"),
                "--owner-config".into(),
                f.path().into_os_string(),
                "--workspace".into(),
                f.workspace.clone().into_os_string(),
                "--request".into(),
                request.into_os_string(),
                "--operator-fd".into(),
                operator.as_raw_fd().to_string().into(),
                "--controller-fd".into(),
                controller.as_raw_fd().to_string().into(),
            ];
            match malformed {
                "duplicate" => {
                    args.extend(["--workspace".into(), f.workspace.clone().into_os_string()])
                }
                "trailing" => args.push("--foreign".into()),
                "unknown" => args.extend(["--foreign".into(), "value".into()]),
                _ => unreachable!(),
            }
            assert!(
                run_admin_at(&args, Some(&f.home)).is_err(),
                "argument grammar {malformed}"
            );
            assert!(
                !f.path().exists(),
                "ambiguous arguments published owner configuration"
            );
        }
    }

    #[test]
    fn journal_entry_count_corpus_accepts_the_bound_and_refuses_one_more() {
        let f = Fixture::new(HostRoute::Claude);
        let directory = PrivateDirectory::open(&f.private).unwrap();
        let journal = f.private.join("native");
        for number in 0..10_000 {
            let path = journal.join(format!("entry-{number}"));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            drop(file);
        }
        assert!(
            directory.verify_journal(&journal).is_ok(),
            "exact journal entry bound"
        );
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(journal.join("one-more"))
            .unwrap();
        drop(file);
        assert!(
            directory.verify_journal(&journal).is_err(),
            "journal entry bound exceeded"
        );
    }

    #[test]
    fn private_storage_bounds_and_path_grammar_corpus() {
        let f = Fixture::new(HostRoute::Codex);
        let directory = PrivateDirectory::open(&f.private).unwrap();
        assert!(PrivateDirectory::open(Path::new("relative")).is_err());
        assert!(
            PrivateDirectory::open(f.private.strip_prefix("/").unwrap()).is_err(),
            "physical root still requires absolute input"
        );
        assert!(directory.parent(&f.private).is_err(), "root is not a leaf");
        assert!(
            directory
                .parent(&f.private.join("../controller.json"))
                .is_err(),
            "parent component"
        );
        for name in ["", "a/b", "/absolute", "bad\nname"] {
            assert!(
                directory.create_file(name).is_err(),
                "invalid leaf {name:?}"
            );
        }
        for (text, maximum, expected) in [
            ("a", 1, true),
            ("", 1, false),
            ("ab", 1, false),
            ("\n", 1, false),
            ("\u{7f}", 1, false),
        ] {
            assert_eq!(opaque(text, maximum), expected);
        }
        let path = f.private.join("bounded.json");
        for size in [MAX_CONFIG - 1, MAX_CONFIG, MAX_CONFIG + 1] {
            let draft = String::from_utf8(canonical(&f.draft).unwrap()).unwrap();
            let text = format!("{draft}{}", " ".repeat(size as usize - draft.len()));
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(
                directory.read::<OwnerDraft>(&path).is_ok(),
                size <= MAX_CONFIG,
                "private config size {size}"
            );
        }
        let outside = f.root.join("outside-journal");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            directory.verify_journal(&f.private).is_err(),
            "journal cannot be the private root"
        );
        assert!(
            directory
                .verify_journal(&f.private.join("../outside-journal"))
                .is_err(),
            "journal parent traversal"
        );
        let mut deep = f.private.join("native");
        for _ in 0..64 {
            deep = deep.join("nested");
            fs::create_dir(&deep).unwrap();
            fs::set_permissions(&deep, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(directory.verify_journal(&f.private.join("native")).is_ok());
        let path = deep.join("one-too-deep");
        fs::write(&path, b"private").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            directory.verify_journal(&f.private.join("native")).is_err(),
            "journal depth bound"
        );
    }

    #[test]
    fn administrative_error_code_corpus_never_discloses_input_or_private_paths() {
        for (code, expected) in [
            ("RECOVERY_REQUIRED".to_owned(), "RECOVERY_REQUIRED"),
            ("ERROR_2".to_owned(), "ERROR_2"),
            (String::new(), "INTERNAL_ERROR"),
            ("private/credential.json".to_owned(), "INTERNAL_ERROR"),
            ("TOKEN\nsecret".to_owned(), "INTERNAL_ERROR"),
            ("é".to_owned(), "INTERNAL_ERROR"),
            ("X".repeat(65), "INTERNAL_ERROR"),
        ] {
            let error = MachineError::new(
                code,
                "secret credential path",
                false,
                serde_json::json!({"secret":"private"}),
            );
            assert_eq!(administrative_error_code(&error), expected);
        }
    }

    #[test]
    fn admin_authority_and_interrupted_setup_corpus_uses_real_fd_carriers() {
        use std::os::fd::AsRawFd as _;
        for stage in ["owner-config-create", "namespace-setup", "host-state-init"] {
            for authority in [
                "current",
                "foreign-operator",
                "rotated-operator",
                "foreign-controller",
            ] {
                let f = Fixture::new(HostRoute::Codex);
                if stage != "owner-config-create" {
                    f.create().unwrap();
                }
                if stage == "host-state-init" {
                    f.namespace();
                }
                let request = f.private.join("request.json");
                fs::write(&request, canonical(&f.draft).unwrap()).unwrap();
                fs::set_permissions(&request, fs::Permissions::from_mode(0o600)).unwrap();
                let other = Fixture::new(HostRoute::Claude);
                if authority == "rotated-operator" {
                    OperatorStore::new(f.home.operator_root())
                        .rotate(
                            &CredentialCarrier::open_path(&f.operator).unwrap(),
                            &f.private.join("rotated.json"),
                        )
                        .unwrap();
                }
                let operator = File::open(if authority == "foreign-operator" {
                    &other.operator
                } else {
                    &f.operator
                })
                .unwrap();
                let controller = File::open(if authority == "foreign-controller" {
                    &other.controller
                } else {
                    &f.controller
                })
                .unwrap();
                let mut args = vec![
                    OsString::from(stage),
                    "--owner-config".into(),
                    f.path().into_os_string(),
                    "--operator-fd".into(),
                    operator.as_raw_fd().to_string().into(),
                    "--controller-fd".into(),
                    controller.as_raw_fd().to_string().into(),
                ];
                if stage == "owner-config-create" {
                    args.extend([
                        "--workspace".into(),
                        f.workspace.clone().into_os_string(),
                        "--request".into(),
                        request.into_os_string(),
                    ]);
                }
                let result = run_admin_at(&args, Some(&f.home));
                assert_eq!(
                    result.is_ok(),
                    authority == "current",
                    "{stage}/{authority}: {result:?}"
                );
                if authority != "current" {
                    if stage == "owner-config-create" {
                        assert!(!f.path().exists());
                        assert!(!f.private.join(LOCK_NAME).exists());
                    }
                    if stage == "namespace-setup" {
                        assert!(
                            !crate::engagement::namespace_birth_path(&f.owner().state_root)
                                .exists()
                        );
                    }
                    assert!(!f.private.join("invocations.sqlite").exists());
                } else {
                    run_admin_at(&args, Some(&f.home)).unwrap();
                }
            }
        }
    }

    pub(crate) fn managed_fixture(route: HostRoute) -> Fixture {
        configure_managed_fixture(Fixture::new(route))
    }
    pub(crate) fn configure_managed_fixture(mut f: Fixture) -> Fixture {
        fs::remove_file(&f.controller).unwrap();
        crate::controller::create_controller_credential(
            &f.controller,
            ControllerKind::HumanCli,
            "root-owner".into(),
            None,
            None,
        )
        .unwrap();
        let binding = crate::controller::binding_from_carrier(
            &CredentialCarrier::open_path(&f.controller).unwrap(),
            1,
        )
        .unwrap();
        let template = crate::semantic::successor_fixture::ManagedMember::new();
        let session = crate::orchestration::OrchestrationStore::open_observer(&template.state_root)
            .unwrap()
            .session(template.session_id)
            .unwrap();
        let crate::session_policy::SessionPolicySnapshot::Legacy(policy) =
            session.specialist_policy
        else {
            panic!("legacy fixture")
        };
        let workspace = crate::workspace::workspace_id(&f.workspace);
        let state = f.home.workspace_root(&workspace);
        let session_ref = Uuid::now_v7();
        let mut sessions = crate::orchestration::OrchestrationStore::open(&state).unwrap();
        let session = sessions
            .prepare_session(
                &workspace,
                session_ref,
                "host-root",
                &"b".repeat(64),
                &policy,
            )
            .unwrap();
        let mut manifest = template.manifest.clone();
        manifest.schema_version = 2;
        manifest.run_id = session_ref;
        manifest.workspace_id = workspace.clone();
        manifest.canonical_workspace = crate::workspace::LosslessPath::from_path(&f.workspace);
        manifest.controller = binding.clone();
        manifest.control_mode = crate::domain::ControlMode::DirectInteractive;
        manifest.parent_ref = None;
        manifest.start_contract = None;
        manifest.requested_access = None;
        manifest.selected_role = None;
        manifest.creation_mode = None;
        manifest.admitted_configuration_digest = None;
        manifest.start_operation_receipt = None;
        let text = "Root fixture instructions";
        let instructions = crate::run::InstructionSnapshot {
            schema: "dolgorae.instructions/v1".into(),
            common_prefix_version: 1,
            mode_prefix_version: 1,
            purpose_prefix_version: 1,
            normalized_byte_length: text.len() as u64,
            normalized_sha256: crate::jcs::sha256_hex(text.as_bytes()),
        };
        manifest.instructions = instructions.clone().into();
        manifest.agent_configuration.schema_version = 2;
        manifest.agent_configuration.instructions = instructions.into();
        manifest.agent_configuration.normalized_instructions = text.into();
        manifest.agent_configuration.role_reference = None;
        manifest.agent_configuration.admitted_configuration = None;
        manifest.agent_configuration.native_subagent_policy = "enabled".into();
        manifest.aggregate_binding = Some(crate::run::AggregateBinding {
            aggregate_kind: crate::domain::AggregateKind::OrchestratedSession,
            aggregate_id: session_ref,
            operation_id: session.bootstrap_operation_id,
            member_kind: crate::run::AggregateMemberKind::Primary,
            policy_sha256: Some(session.specialist_policy_sha256),
            role_reference: None,
            role_snapshot_sha256: None,
            agent_configuration_sha256: None,
        });
        crate::run::RunStore::new(SystemWorkspacePlatform, &state)
            .publish(&manifest)
            .unwrap();
        f.draft.owner = OwnerKind::Managed { session_ref };
        f.draft.adapter_id = "dolgorae.managed/1".into();
        f.draft.allowed_scopes = vec![AllowedScope {
            kind: "orchestrated_session".into(),
            reference: Some(session_ref.to_string()),
        }];
        f.complete();
        f
    }

    #[test]
    fn managed_configuration_uses_the_existing_root_controller_and_session_binding() {
        let f = managed_fixture(HostRoute::Codex);
        let OwnerKind::Managed { session_ref } = f.draft.owner else {
            unreachable!()
        };
        let workspace = crate::workspace::workspace_id(&f.workspace);
        let state = f.home.workspace_root(&workspace);
        let manifest = crate::run::RunStore::new(SystemWorkspacePlatform, &state)
            .load_manifest(session_ref)
            .unwrap();
        let owner = f.owner();
        owner
            .permit(Some(("orchestrated_session", &session_ref.to_string())))
            .unwrap();
        assert!(
            owner
                .permit(Some(("external_engagement", "external")))
                .is_err()
        );
        let record = fs::read(f.path()).unwrap();
        for case in 0..5 {
            let mut changed = manifest.clone();
            match case {
                0 => changed.controller.capability_sha256 = "0".repeat(64),
                1 => changed.workspace_id = "foreign".into(),
                2 => changed.aggregate_binding = None,
                3 => changed.aggregate_binding.as_mut().unwrap().operation_id = Uuid::now_v7(),
                4 => {
                    changed.aggregate_binding.as_mut().unwrap().policy_sha256 = Some("f".repeat(64))
                }
                _ => unreachable!(),
            }
            let path = state
                .join("runs")
                .join(session_ref.to_string())
                .join("manifest.json");
            fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(
                VerifiedOwner::load(&f.home, &f.path()).is_err(),
                "managed binding mutation {case}"
            );
            fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        }
        let store = f.ready();
        let request = serde_json::json!({"schema":"dolgorae.external-specialist-request/v5","operation":"open_external_engagement",
            "workspace_ref":workspace,"idempotency_key":"foreign-native","expected_revision":null});
        assert!(
            store
                .bind_native(
                    crate::host_invocation::NativeEvent {
                        session: "native".into(),
                        turn: None,
                        tool_use: "call".into(),
                        server: "dolgorae".into(),
                        server_source: None,
                        tool: "specialist".into()
                    },
                    request
                )
                .is_err()
        );
        assert_eq!(fs::read(f.path()).unwrap(), record);
    }

    #[test]
    fn explicit_setup_stages_resume_without_delivery_initialization() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let fixture = Fixture::new(route);
            assert!(VerifiedOwner::load(&fixture.home, &fixture.path()).is_err());
            assert!(!fixture.private.join(LOCK_NAME).exists());
            fixture.create().unwrap();
            let owner = fixture.owner();
            let namespace = crate::engagement::namespace_birth_path(&owner.state_root);
            assert!(!namespace.exists());
            assert!(owner.ready().is_err());
            assert!(!namespace.exists(), "incomplete delivery created namespace");
            assert!(!fixture.private.join("invocations.sqlite").exists());
            assert!(fixture.initialize().is_err());
            assert!(!fixture.private.join("invocations.pending").exists());
            fixture.create().unwrap();
            let birth = fixture.namespace();
            assert!(fixture.owner().ready().is_err());
            fixture.initialize().unwrap();
            fixture.initialize().unwrap();
            assert_eq!(fixture.namespace(), birth);
            let store = fixture.ready();
            assert_eq!(
                store.caller_scope().unwrap().accepted(),
                &fixture.draft.caller_scope
            );
            assert!(store.launch_recipe().is_err()); // No native executable is launched by setup.
            drop(store);
            fs::remove_file(&namespace).unwrap();
            // DB-committed/birth-unpublished setup resumes without changing birth.
            assert_eq!(fixture.namespace(), birth);
            assert!(namespace.exists());
        }
    }

    #[test]
    fn owner_record_and_revalidation_corpus_pins_original_configuration() {
        let f = Fixture::new(HostRoute::Codex);
        f.create().unwrap();
        let bytes = fs::read(f.path()).unwrap();
        let alias = f.private.join("foreign-name.json");
        fs::write(&alias, &bytes).unwrap();
        fs::set_permissions(&alias, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            VerifiedOwner::load(&f.home, &alias).is_err(),
            "owner filename"
        );
        let mut missing_scope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        missing_scope["configuration"]["allowed_scopes"][1]
            .as_object_mut()
            .unwrap()
            .remove("reference");
        fs::write(f.path(), canonical(&missing_scope).unwrap()).unwrap();
        assert!(
            VerifiedOwner::load(&f.home, &f.path()).is_err(),
            "missing wildcard authorization must not default to null"
        );
        fs::write(f.path(), &bytes).unwrap();
        for pointer in [
            "",
            "/configuration",
            "/configuration/executable",
            "/configuration/allowed_scopes/0",
        ] {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            value
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("unexpected".into(), true.into());
            fs::write(f.path(), canonical(&value).unwrap()).unwrap();
            assert!(
                VerifiedOwner::load(&f.home, &f.path()).is_err(),
                "unknown owner field {pointer}"
            );
        }
        fs::write(f.path(), &bytes).unwrap();
        let owner = f.owner();
        for field in [
            "schema_version",
            "workspace_ref",
            "controller_generation",
            "caller_scope",
        ] {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            match field {
                "schema_version" => value[field] = 2.into(),
                "workspace_ref" => value[field] = "foreign".into(),
                "controller_generation" => value["configuration"][field] = 2.into(),
                "caller_scope" => value["configuration"][field]["max_rounds"] = 5.into(),
                _ => unreachable!(),
            }
            fs::write(f.path(), canonical(&value).unwrap()).unwrap();
            assert!(owner.revalidate().is_err(), "changed owner record {field}");
            if field != "caller_scope" {
                assert!(
                    VerifiedOwner::load(&f.home, &f.path()).is_err(),
                    "owner metadata {field}"
                );
            }
            fs::write(f.path(), &bytes).unwrap();
        }
        let other = Fixture::new(HostRoute::Codex);
        fs::remove_file(&other.controller).unwrap();
        crate::controller::create_controller_credential(
            &other.controller,
            ControllerKind::Other,
            "unpermitted".into(),
            None,
            None,
        )
        .unwrap();
        assert!(
            other.create().is_err(),
            "Controller kind is not owner authority"
        );
        assert!(!other.path().exists());
        // The immutable owner pathname is a publication precondition, not just
        // a readback check after a different pathname was published.
        let f = Fixture::new(HostRoute::Codex);
        let result = create_owner(
            &f.home,
            &f.workspace,
            &f.private.join("alias.json"),
            f.draft.clone(),
            &CredentialCarrier::open_path(&f.controller).unwrap(),
        );
        assert!(result.is_err());
        assert!(
            !f.path().exists(),
            "invalid owner filename published configuration"
        );
    }

    #[test]
    fn owner_configuration_mutation_corpus_refuses_before_publication() {
        for case in 0..26 {
            let mut f = Fixture::new(HostRoute::Codex);
            match case {
                0 => f.draft.executable.version = "0.160.0".to_owned(),
                1 => f.draft.executable.sha256 = "0".repeat(64),
                2 => f.draft.executable.path = PathBuf::from("relative"),
                3 => f.draft.controller_generation = 0,
                4 => f.draft.caller_scope.max_rounds = 0,
                5 => f.draft.adapter_id = "dolgorae.managed/1".to_owned(),
                6 => f.draft.adapter_version = "foreign".to_owned(),
                7 => f.draft.tools.clear(),
                8 => f.draft.tools.push("specialist".to_owned()),
                9 => f.draft.allowed_scopes.clear(),
                10 => f.draft.allowed_scopes[1].kind = "orchestrated_session".to_owned(),
                11 => f.draft.allowed_scopes[0].reference = Some("foreign".to_owned()),
                12 => f.draft.native_host_root = f.private.clone(),
                13 => {
                    f.draft.owner = OwnerKind::Managed {
                        session_ref: Uuid::nil(),
                    }
                }
                14 => f.draft.controller_file = f.workspace.join("missing"),
                15 => f.draft.controller_generation = 9_007_199_254_740_992,
                16 => f.draft.tools = (0..33).map(|n| format!("tool-{n}")).collect(),
                17 => f.draft.tools[0] = "".into(),
                18 => f.draft.tools[0] = "x".repeat(129),
                19 => f.draft.allowed_scopes = vec![f.draft.allowed_scopes[0].clone(); 65],
                20 => f.draft.allowed_scopes[1].reference = Some("".into()),
                21 => f.draft.allowed_scopes[1].reference = Some("x".repeat(513)),
                22 => f.draft.caller_scope.binding_ref.clear(),
                23 => f.draft.tools[0] = "bad\nname".into(),
                24 => f.draft.native_host_root = f.root.join("outside"),
                25 => f.draft.allowed_scopes[0].reference = None,
                _ => unreachable!(),
            }
            assert!(f.create().is_err(), "owner mutation {case}");
            assert!(
                !f.path().exists(),
                "owner mutation {case} published configuration"
            );
            assert!(
                !f.private.join(LOCK_NAME).exists(),
                "owner mutation {case} created lock"
            );
        }
    }

    #[test]
    fn protected_path_permission_and_alias_corpus_is_effect_free() {
        for case in 0..8 {
            let f = Fixture::new(HostRoute::Claude);
            let original = fs::read(&f.controller).unwrap();
            match case {
                0 => fs::set_permissions(&f.private, fs::Permissions::from_mode(0o755)).unwrap(),
                1 => fs::set_permissions(&f.controller, fs::Permissions::from_mode(0o644)).unwrap(),
                2 => {
                    fs::hard_link(&f.controller, f.private.join("alias")).unwrap();
                }
                3 => {
                    fs::rename(&f.controller, f.private.join("real")).unwrap();
                    symlink(f.private.join("real"), &f.controller).unwrap();
                }
                4 => {
                    fs::set_permissions(f.private.join("native"), fs::Permissions::from_mode(0o755))
                        .unwrap()
                }
                5 => {
                    fs::write(f.private.join("native/journal"), b"private native history").unwrap();
                    fs::set_permissions(
                        f.private.join("native/journal"),
                        fs::Permissions::from_mode(0o644),
                    )
                    .unwrap();
                }
                6 => {
                    symlink(&f.workspace, f.private.join("native/linked")).unwrap();
                }
                7 => {
                    let outside = f.private.join("outside");
                    fs::write(&outside, b"secret").unwrap();
                    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
                    fs::hard_link(outside, f.private.join("native/hardlink")).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(f.create().is_err(), "path mutation {case}");
            assert!(!f.path().exists());
            assert_eq!(fs::read(&f.controller).unwrap(), original);
        }
        let f = Fixture::new(HostRoute::Codex);
        let directory = PrivateDirectory::open(&f.private).unwrap();
        assert!(
            directory
                .open_file(&f.private.join("../private/controller.json"))
                .is_err()
        );
        let file = File::open(&f.controller).unwrap();
        assert!(
            private_file_metadata(&file.metadata().unwrap(), DarwinSystem.current_uid() + 1)
                .is_err()
        );
        assert!(
            private_directory_metadata(
                &directory.file.metadata().unwrap(),
                DarwinSystem.current_uid() + 1
            )
            .is_err()
        );
        assert!(
            private_directory_metadata(&file.metadata().unwrap(), DarwinSystem.current_uid())
                .is_err()
        );
        let alias = f.root.join("private-alias");
        symlink(&f.private, &alias).unwrap();
        assert!(PrivateDirectory::open(&alias).is_err());
        assert!(directory.open_file(&f.private.join("native")).is_err());
        fs::rename(&f.private, f.root.join("retained-private")).unwrap();
        fs::create_dir(&f.private).unwrap();
        fs::set_permissions(&f.private, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(directory.verify().is_err());
    }

    #[test]
    fn setup_refuses_changed_owner_birth_and_partial_state() {
        for case in 0..7 {
            let mut f = Fixture::new(HostRoute::Codex);
            f.complete();
            let state_root = f.owner().state_root;
            match case {
                0 => {
                    f.draft.caller_scope.max_rounds += 1;
                    assert_eq!(f.create().unwrap_err().code, "IDEMPOTENCY_CONFLICT");
                }
                1 => {
                    fs::remove_file(f.private.join(LOCK_NAME)).unwrap();
                    assert!(f.create().is_err());
                }
                2 => {
                    fs::write(f.private.join("invocations.pending"), b"interrupted").unwrap();
                    assert!(f.initialize().is_err());
                }
                3 => {
                    fs::write(f.private.join("invocations.sqlite"), b"corrupt").unwrap();
                    assert!(f.initialize().is_err());
                }
                4 => {
                    let birth = crate::engagement::namespace_birth_path(&state_root);
                    fs::write(&birth, format!("{{\"schema\":\"dolgorae.namespace-birth/v1\",\"namespace_birth_id\":\"{}\"}}", Uuid::now_v7())).unwrap();
                    assert!(f.initialize().is_err());
                }
                5 => {
                    fs::remove_file(&f.controller).unwrap();
                    crate::controller::create_controller_credential(
                        &f.controller,
                        ControllerKind::Automation,
                        "foreign".to_owned(),
                        None,
                        None,
                    )
                    .unwrap();
                    assert!(VerifiedOwner::load(&f.home, &f.path()).is_err());
                }
                6 => {
                    let c =
                        rusqlite::Connection::open(f.private.join("invocations.sqlite")).unwrap();
                    c.execute("UPDATE metadata SET generation=0", [])
                        .unwrap_err();
                    c.execute("UPDATE metadata SET owner_digest='foreign'", [])
                        .unwrap();
                    assert!(f.initialize().is_err());
                }
                _ => unreachable!(),
            }
            if case != 0 {
                assert!(
                    VerifiedOwner::load(&f.home, &f.path())
                        .and_then(VerifiedOwner::ready)
                        .is_err(),
                    "partial case {case}"
                );
            }
        }
    }
}
