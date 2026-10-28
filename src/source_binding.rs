//! Private administrative source preparation; product receipts remain authoritative.

use crate::delegation_identity::{AcceptedTask, LookupNamespace};
use crate::delegation_service::{
    self as services, AssignmentSource, DelegationScope, SettlementOwner,
};
use crate::instructions::{InstructionSourceStore, SelectedInstructionSource, SelectedNamedSource};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::provisioning::{
    ProvisioningAdmission, ProvisioningSources, ProvisioningSourcesIdentity, RetainedSnapshot,
};
use crate::trusted_host::{
    OwnerKind, PrivateDirectory, VerifiedOwner, canonical, conflict, refusal,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[cfg(test)]
thread_local! {static AFTER_CAPTURE:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=std::cell::RefCell::new(None);}
#[cfg(test)]
struct CaptureFault;
#[cfg(test)]
impl CaptureFault {
    fn at(action: impl FnOnce() + 'static) -> Self {
        AFTER_CAPTURE.with(|s| {
            assert!(s.borrow().is_none());
            *s.borrow_mut() = Some(Box::new(action));
        });
        Self
    }
}
#[cfg(test)]
impl Drop for CaptureFault {
    fn drop(&mut self) {
        AFTER_CAPTURE.with(|s| {
            s.borrow_mut().take();
        });
    }
}
fn after_capture() {
    #[cfg(test)]
    {
        let action = AFTER_CAPTURE.with(|s| s.borrow_mut().take());
        if let Some(action) = action {
            action();
        }
    }
}

const DIRECTORY: &str = "preparations";
const VERSION: u32 = 1;
const PUBLIC_BYTES: u64 = 16 * 1024 * 1024;
// At most129 sources, each either65536 inline bytes or64 components of255
// bytes. JSON escapes can expand a byte sixfold. Profile authority already has
// a64MiB producer ceiling. Reserve one MiB for aliases and policy metadata.
const SELECTION_BYTES: u64 = 64 * 1024 * 1024 + 129 * 6 * (65_536 + 64 * 255) + 1024 * 1024;
const ASSIGNMENT_BYTES: u64 = PUBLIC_BYTES + 64 * 1024;
const MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const RECORD_BYTES: u64 = PUBLIC_BYTES + 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProvisioningSelection {
    pub role_name: String,
    pub role: SelectedInstructionSource,
    pub dependencies: Vec<SelectedNamedSource>,
    pub selected_guidance: Vec<SelectedNamedSource>,
    pub profile_binding: crate::global_runtime::GlobalProfileBinding,
    pub admission: ProvisioningAdmission,
}
impl ProvisioningSelection {
    fn capture(&self, home: &DolgoraeHome) -> Result<RetainedSnapshot, MachineError> {
        let instruct = InstructionSourceStore::new(home);
        crate::provisioning::capture_provisioning(&ProvisioningSources {
            instruct: &instruct,
            role_name: &self.role_name,
            role: self.role.clone(),
            dependencies: self.dependencies.clone(),
            selected_guidance: self.selected_guidance.clone(),
            profile_binding: self.profile_binding.clone(),
            admission: self.admission.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssignmentSelection {
    pub namespace: LookupNamespace,
    #[serde(deserialize_with = "crate::delegation_identity::deserialize_invocation_ref")]
    pub member_ref: Uuid,
    pub task: crate::task_request::SpecialistTaskRequest,
    pub execution_intent: String,
    pub deadline_seconds: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub target: Option<crate::review::ReviewTargetRequest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Partition {
    schema_version: u32,
    owner_digest: String,
    workspace: String,
    canonical_workspace: PathBuf,
    #[serde(deserialize_with = "crate::delegation_identity::deserialize_invocation_ref")]
    birth: Uuid,
    generation: u64,
    principal: String,
    controller_generation: u64,
    caller: crate::delegation_identity::CallerScope,
    owner: OwnerKind,
}
impl Partition {
    fn current(owner: &VerifiedOwner) -> Result<Self, MachineError> {
        owner.revalidate()?;
        let ns = namespace(owner, None, "install_provisioning", "partition")?;
        Ok(Self {
            schema_version: VERSION,
            owner_digest: owner.configuration_digest.clone(),
            workspace: owner.view.workspace_id.clone(),
            canonical_workspace: owner.view.canonical_path.to_path_buf()?,
            birth: crate::engagement::require_delegation_namespace(&owner.state_root)?,
            generation: crate::host_invocation::preparation_generation(owner)?,
            principal: ns.controller_principal,
            controller_generation: ns.controller_generation,
            caller: owner.configuration.caller_scope.clone(),
            owner: owner.configuration.owner.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    digest: String,
    bytes: u64,
}
impl Component {
    fn of(bytes: &[u8]) -> Self {
        Self {
            digest: crate::jcs::sha256_hex(bytes),
            bytes: bytes.len() as u64,
        }
    }
    fn name(&self) -> Result<String, MachineError> {
        if self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(refusal());
        }
        Ok(format!("c-{}.json", self.digest))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Prepared {
    Provisioning {
        selection: Component,
        snapshot: Component,
        projection: Box<ProvisioningSourcesIdentity>,
    },
    Assignment {
        selection: Component,
        configuration: Component,
        projection: Box<AcceptedTask>,
        #[serde(deserialize_with = "Option::deserialize")]
        manifest: Option<Component>,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    partition: Partition,
    prepared: Prepared,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    record: Component,
}

fn no_pending(directory: &PrivateDirectory, name: &str) -> Result<(), MachineError> {
    match std::fs::symlink_metadata(directory.path.join(format!("{name}.pending"))) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(refusal()),
    }
}

struct Registry {
    directory: PrivateDirectory,
    partition: Partition,
}
impl Registry {
    fn open(owner: &VerifiedOwner) -> Result<Self, MachineError> {
        let partition = Partition::current(owner)?;
        let directory = PrivateDirectory::open(&owner.root.path.join(DIRECTORY))?;
        no_pending(&directory, "metadata.json")?;
        let existing: Partition = directory.read(&directory.path.join("metadata.json"))?;
        if existing != partition {
            return Err(refusal());
        }
        // Check the lock even for read-only projection; a partial init is not ready.
        directory.open_file(&directory.path.join(crate::trusted_host::LOCK_NAME))?;
        Ok(Self {
            directory,
            partition,
        })
    }
    fn component<T: serde::de::DeserializeOwned>(
        &self,
        reference: &Component,
        maximum: u64,
    ) -> Result<T, MachineError> {
        if reference.bytes > maximum {
            return Err(refusal());
        }
        no_pending(&self.directory, &reference.name()?)?;
        let value: Value = self
            .directory
            .read_bounded(&self.directory.path.join(reference.name()?), maximum)?;
        let bytes = canonical(&value)?;
        if Component::of(&bytes) != *reference {
            return Err(refusal());
        }
        serde_json::from_value(value).map_err(|_| refusal())
    }
    fn record(&self, name: &str) -> Result<Record, MachineError> {
        no_pending(&self.directory, name)?;
        let index: Index = self.directory.read(&self.directory.path.join(name))?;
        let record: Record = self.component(&index.record, RECORD_BYTES)?;
        if record.partition != self.partition {
            return Err(refusal());
        }
        Ok(record)
    }
    fn publish(
        &self,
        name: &str,
        prepared: Prepared,
        components: Vec<Vec<u8>>,
    ) -> Result<(), MachineError> {
        let record = Record {
            partition: self.partition.clone(),
            prepared,
        };
        let bytes = canonical(&record)?;
        if bytes.len() as u64 > RECORD_BYTES {
            return Err(refusal());
        }
        let reference = Component::of(&bytes);
        let _lock = self.directory.lock()?;
        let index_path = self.directory.path.join(name);
        match std::fs::symlink_metadata(&index_path) {
            Ok(_) => {
                if self.record(name)? != record {
                    return Err(conflict());
                }
                // Verify every complete component on idempotent registration too.
                self.verify_record(&record)?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(refusal()),
        }
        let mut pieces = components;
        pieces.push(bytes);
        for bytes in pieces {
            let component = Component::of(&bytes);
            self.publish_bytes(&component.name()?, &bytes)?;
        }
        self.publish_bytes(name, &canonical(&Index { record: reference })?)?;
        Ok(())
    }
    fn publish_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), MachineError> {
        no_pending(&self.directory, name)?;
        match std::fs::symlink_metadata(self.directory.path.join(name)) {
            Ok(_) => {
                let existing: Value = self
                    .directory
                    .read_bounded(&self.directory.path.join(name), bytes.len() as u64)?;
                if canonical(&existing)? != bytes {
                    return Err(refusal());
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.directory.publish(name, bytes)
            }
            Err(_) => Err(refusal()),
        }
    }
    fn verify_record(&self, record: &Record) -> Result<(), MachineError> {
        match &record.prepared {
            Prepared::Provisioning {
                selection,
                snapshot,
                projection,
            } => {
                let selected: ProvisioningSelection = self.component(selection, SELECTION_BYTES)?;
                let snapshot: RetainedSnapshot =
                    self.component(snapshot, crate::provisioning::MAX_SNAPSHOT_BYTES as u64)?;
                snapshot.verify()?;
                snapshot.admitted_configuration()?;
                if selected.role_name != snapshot.role.name
                    || selected.dependencies.len() != snapshot.dependencies.len()
                    || selected.selected_guidance.len() != snapshot.selected_guidance.len()
                {
                    return Err(refusal());
                }
                let pairs = std::iter::once((&selected.role, &snapshot.role.body)).chain(
                    selected
                        .dependencies
                        .iter()
                        .zip(&snapshot.dependencies)
                        .chain(
                            selected
                                .selected_guidance
                                .iter()
                                .zip(&snapshot.selected_guidance),
                        )
                        .map(|(s, r)| (&s.source, &r.body)),
                );
                for (source, body) in pairs {
                    if let SelectedInstructionSource::Inline(inline) = source {
                        if inline != body {
                            return Err(refusal());
                        }
                    } else if let SelectedInstructionSource::File(parts) = source {
                        crate::instructions::validate_directory_components(parts)?;
                    }
                }
                if selected
                    .dependencies
                    .iter()
                    .map(|s| &s.name)
                    .ne(snapshot.dependencies.iter().map(|s| &s.name))
                    || selected
                        .selected_guidance
                        .iter()
                        .map(|s| &s.name)
                        .ne(snapshot.selected_guidance.iter().map(|s| &s.name))
                    || RetainedSnapshot::from_capture(
                        snapshot.capture(),
                        selected.profile_binding,
                        selected.admission,
                    )? != snapshot
                {
                    return Err(refusal());
                }
                if snapshot.sources_identity()? != **projection {
                    return Err(refusal());
                }
            }
            Prepared::Assignment {
                selection,
                projection,
                manifest,
                configuration,
            } => {
                let selected: AssignmentSelection = self.component(selection, ASSIGNMENT_BYTES)?;
                projection.digest()?;
                let bytes = manifest
                    .as_ref()
                    .map(|r| {
                        self.component::<Value>(r, MANIFEST_BYTES)
                            .and_then(|v| canonical(&v))
                    })
                    .transpose()?;
                verify_assignment_bytes(&selected, projection, bytes.as_deref())?;
                let configuration: crate::run::AgentConfigurationSnapshot = self.component(
                    configuration,
                    crate::provisioning::MAX_SNAPSHOT_BYTES as u64,
                )?;
                let manifest = bytes
                    .as_ref()
                    .map(|bytes| {
                        serde_json::from_slice::<crate::direct_review_target::DirectTarget>(bytes)
                            .map_err(|_| refusal())
                            .and_then(|target| target.metadata_bytes())
                    })
                    .transpose()?;
                let identity = manifest
                    .as_ref()
                    .map(|bytes| {
                        serde_json::from_slice::<crate::direct_review_target::DirectTarget>(bytes)
                            .map(|target| target.target_sha256)
                            .map_err(|_| refusal())
                    })
                    .transpose()?;
                let (actual, _) = crate::successor_capture::capture_facade_task(
                    &configuration,
                    &selected.task,
                    &selected.execution_intent,
                    identity
                        .as_ref()
                        .zip(manifest.as_ref())
                        .map(|(id, b)| (id.as_str(), b.as_slice())),
                )?;
                if actual != **projection {
                    return Err(refusal());
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn initialize(owner: &VerifiedOwner) -> Result<(), MachineError> {
    let partition = Partition::current(owner)?;
    let _lock = owner.root.lock()?;
    owner.revalidate()?;
    let path = owner.root.path.join(DIRECTORY);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let directory = PrivateDirectory::open(&path)?;
            no_pending(&directory, "metadata.json")?;
            let existing: Partition = directory.read(&path.join("metadata.json"))?;
            directory.open_file(&path.join(crate::trusted_host::LOCK_NAME))?;
            if existing != partition {
                return Err(refusal());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            crate::darwin::DarwinSystem
                .mkdirat_private(&owner.root.file, OsStr::new(DIRECTORY))
                .map_err(|_| refusal())?;
            owner.root.file.sync_all().map_err(|_| refusal())?;
            let directory = PrivateDirectory::open(&path)?;
            directory.create_file(crate::trusted_host::LOCK_NAME)?;
            directory.publish("metadata.json", &canonical(&partition)?)?;
        }
        Err(_) => return Err(refusal()),
    }
    Ok(())
}

fn namespace(
    owner: &VerifiedOwner,
    scope: Option<(&str, &str)>,
    operation: &str,
    key: &str,
) -> Result<LookupNamespace, MachineError> {
    Ok((&crate::operation::Namespace::delegation(
        &owner.controller.identity,
        &owner.view.workspace_id,
        scope,
        operation,
        key,
    )
    .map_err(|_| refusal())?)
        .into())
}
fn assignment_owner(
    owner: &VerifiedOwner,
    selected: &AssignmentSelection,
) -> Result<SettlementOwner, MachineError> {
    let ns = &selected.namespace;
    let reference = ns.scope_ref.as_deref().ok_or_else(refusal)?;
    let scope_kind = ns.scope_kind.as_deref().ok_or_else(refusal)?;
    owner.permit(Some((scope_kind, reference)))?;
    if namespace(
        owner,
        Some((scope_kind, reference)),
        &ns.operation_kind,
        &ns.idempotency_key,
    )? != *ns
    {
        return Err(refusal());
    }
    let uuid = Uuid::try_parse(reference).map_err(|_| refusal())?;
    if uuid.to_string() != reference
        || uuid.get_version_num() != 7
        || uuid.get_variant() != uuid::Variant::RFC4122
    {
        return Err(refusal());
    }
    if !(1..=3600).contains(&selected.deadline_seconds) {
        return Err(refusal());
    }
    if let Some(target) = &selected.target {
        target.validate()?;
    }
    match (
        &owner.configuration.owner,
        scope_kind,
        ns.operation_kind.as_str(),
    ) {
        (OwnerKind::External, "external_engagement", "assign_external_specialist_task") => {
            Ok(SettlementOwner::External {
                engagement_id: uuid,
            })
        }
        (OwnerKind::Managed { session_ref }, "orchestrated_session", "assign_specialist_task")
            if *session_ref == uuid =>
        {
            Ok(SettlementOwner::Managed { session_id: uuid })
        }
        _ => Err(refusal()),
    }
}
fn permit_provisioning(owner: &VerifiedOwner) -> Result<(), MachineError> {
    match owner.configuration.owner {
        OwnerKind::External => owner.permit(None),
        OwnerKind::Managed { session_ref } => {
            owner.permit(Some(("orchestrated_session", &session_ref.to_string())))
        }
    }
}
fn scope(owner: &VerifiedOwner) -> DelegationScope<'_> {
    DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    }
}
fn index_name(prefix: &str, value: &impl Serialize) -> Result<String, MachineError> {
    Ok(format!(
        "{prefix}-{}.json",
        crate::jcs::sha256_hex(&canonical(value)?)
    ))
}

pub(crate) fn prepare_provisioning(
    home: &DolgoraeHome,
    owner: &VerifiedOwner,
    selected: ProvisioningSelection,
) -> Result<ProvisioningSourcesIdentity, MachineError> {
    permit_provisioning(owner)?;
    let registry = Registry::open(owner)?;
    let selection = canonical(&selected)?;
    if selection.len() as u64 > SELECTION_BYTES {
        return Err(refusal());
    }
    let snapshot = selected.capture(home)?;
    let snapshot_bytes = canonical(&snapshot)?;
    if snapshot_bytes.len() > crate::provisioning::MAX_SNAPSHOT_BYTES {
        return Err(refusal());
    }
    let projection = snapshot.sources_identity()?;
    let name = index_name("p", &projection)?;
    registry.publish(
        &name,
        Prepared::Provisioning {
            selection: Component::of(&selection),
            snapshot: Component::of(&snapshot_bytes),
            projection: Box::new(projection.clone()),
        },
        vec![selection, snapshot_bytes],
    )?;
    Ok(projection)
}

pub(crate) fn prepare_assignment(
    owner: &VerifiedOwner,
    selected: AssignmentSelection,
) -> Result<AcceptedTask, MachineError> {
    let registry = Registry::open(owner)?;
    let target_owner = assignment_owner(owner, &selected)?;
    let selection = canonical(&selected)?;
    if selection.len() as u64 > ASSIGNMENT_BYTES {
        return Err(refusal());
    }
    let configuration =
        services::preparation_configuration(&scope(owner), target_owner, selected.member_ref)?;
    let (accepted, contents) = services::prepare_assignment_capture(
        &scope(owner),
        target_owner,
        selected.member_ref,
        &selected.task,
        &selected.execution_intent,
        selected.target.as_ref(),
    )?;
    let manifest = contents
        .target
        .as_ref()
        .map(|b| {
            let v: Value = serde_json::from_slice(b).map_err(|_| refusal())?;
            canonical(&v)
        })
        .transpose()?;
    if manifest
        .as_ref()
        .is_some_and(|b| b.len() as u64 > MANIFEST_BYTES)
    {
        return Err(refusal());
    }
    let configuration_bytes = canonical(&configuration)?;
    let mut components = vec![selection.clone(), configuration_bytes.clone()];
    if let Some(bytes) = &manifest {
        components.push(bytes.clone());
    }
    registry.publish(
        &index_name("a", &selected.namespace)?,
        Prepared::Assignment {
            selection: Component::of(&selection),
            configuration: Component::of(&configuration_bytes),
            projection: Box::new(accepted.clone()),
            manifest: manifest.as_ref().map(|b| Component::of(b)),
        },
        components,
    )?;
    Ok(accepted)
}

fn verify_assignment_bytes(
    selected: &AssignmentSelection,
    accepted: &AcceptedTask,
    manifest: Option<&[u8]>,
) -> Result<(), MachineError> {
    let task = canonical(&selected.task)?;
    if accepted.task_body != crate::delegation_identity::BytesRef::from_bytes(&task)
        || accepted.execution_intent != selected.execution_intent
    {
        return Err(refusal());
    }
    match (&selected.target, manifest) {
        (Some(selection), Some(bytes)) => {
            let target: crate::direct_review_target::DirectTarget =
                serde_json::from_slice(bytes).map_err(|_| refusal())?;
            if target.kind != selection.kind || target.requested_revision != selection.revision {
                return Err(refusal());
            }
            let reference = accepted
                .contexts
                .iter()
                .find(|c| c.kind == "review_target")
                .ok_or_else(refusal)?;
            // The service producer hashes exact metadata_bytes(), not re-encoded JCS.
            let original = target.metadata_bytes()?;
            if reference.identity != target.target_sha256
                || reference.content != crate::delegation_identity::BytesRef::from_bytes(&original)
                || reference.media_type != "application/json"
            {
                return Err(refusal());
            }
            crate::successor_capture::verify_target_manifest(&reference.identity, &original)?;
        }
        (None, None) if !accepted.contexts.iter().any(|c| c.kind == "review_target") => {}
        _ => return Err(refusal()),
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ProjectionLookup {
    Provisioning {
        sources: Box<ProvisioningSourcesIdentity>,
    },
    Assignment {
        namespace: LookupNamespace,
    },
}
pub(crate) fn project(
    owner: &VerifiedOwner,
    lookup: ProjectionLookup,
) -> Result<Value, MachineError> {
    let registry = Registry::open(owner)?;
    let name = match &lookup {
        ProjectionLookup::Provisioning { sources } => {
            permit_provisioning(owner)?;
            index_name("p", sources)?
        }
        ProjectionLookup::Assignment { namespace: ns } => index_name("a", ns)?,
    };
    let record = registry.record(&name)?;
    registry.verify_record(&record)?;
    match (lookup, record.prepared) {
        (ProjectionLookup::Provisioning { sources }, Prepared::Provisioning { projection, .. })
            if sources == projection =>
        {
            serde_json::to_value(projection).map_err(|_| refusal())
        }
        (
            ProjectionLookup::Assignment { namespace },
            Prepared::Assignment {
                selection,
                projection,
                ..
            },
        ) => {
            let selection: AssignmentSelection =
                registry.component(&selection, ASSIGNMENT_BYTES)?;
            assignment_owner(owner, &selection)?;
            if namespace != selection.namespace {
                return Err(refusal());
            }
            serde_json::to_value(projection).map_err(|_| refusal())
        }
        _ => Err(refusal()),
    }
}

pub(crate) fn fresh_provisioning(
    home: &DolgoraeHome,
    owner: &VerifiedOwner,
    expected: &ProvisioningSourcesIdentity,
) -> Result<RetainedSnapshot, MachineError> {
    permit_provisioning(owner)?;
    let registry = Registry::open(owner)?;
    let record = registry.record(&index_name("p", expected)?)?;
    registry.verify_record(&record)?;
    let Prepared::Provisioning {
        selection,
        snapshot,
        projection,
    } = record.prepared
    else {
        return Err(refusal());
    };
    if *projection != *expected {
        return Err(conflict());
    }
    let selected: ProvisioningSelection = registry.component(&selection, SELECTION_BYTES)?;
    let retained: RetainedSnapshot =
        registry.component(&snapshot, crate::provisioning::MAX_SNAPSHOT_BYTES as u64)?;
    let actual = selected.capture(home)?;
    after_capture();
    if Partition::current(owner)? != registry.partition {
        return Err(refusal());
    }
    if actual != retained || actual.sources_identity()? != *expected {
        return Err(conflict());
    }
    Ok(actual)
}

/// Internal bridge input; authenticated native/Worker dispatch is a later boundary.
pub(crate) struct RegisteredAssignment<'a> {
    pub owner: &'a VerifiedOwner,
    pub supplied: AssignmentSelection,
    pub expected: AcceptedTask,
    pub admission: Option<&'a dyn Fn() -> Result<(), MachineError>>,
}
impl AssignmentSource for RegisteredAssignment<'_> {
    fn expected(&self) -> &AcceptedTask {
        &self.expected
    }
    fn selection(&self) -> Result<Option<crate::review::ReviewTargetRequest>, MachineError> {
        assignment_owner(self.owner, &self.supplied)?;
        if let Some(admission) = self.admission {
            admission()?;
        }
        let registry = Registry::open(self.owner)?;
        let record = registry.record(&index_name("a", &self.supplied.namespace)?)?;
        let Prepared::Assignment {
            selection,
            projection,
            ..
        } = record.prepared
        else {
            return Err(refusal());
        };
        let selected: AssignmentSelection = registry.component(&selection, ASSIGNMENT_BYTES)?;
        let target = selected.target.clone();
        let mut public = selected;
        public.target = None;
        if self.supplied.target.is_some() || public != self.supplied || *projection != self.expected
        {
            return Err(conflict());
        }
        Ok(target)
    }
    fn verify_capture(
        &self,
        accepted: &AcceptedTask,
        contents: &crate::successor_capture::AcceptedTaskContents,
    ) -> Result<(), MachineError> {
        after_capture();
        let registry = Registry::open(self.owner)?;
        let record = registry.record(&index_name("a", &self.supplied.namespace)?)?;
        registry.verify_record(&record)?;
        let Prepared::Assignment {
            projection,
            manifest,
            configuration,
            ..
        } = record.prepared
        else {
            return Err(refusal());
        };
        let owner = assignment_owner(self.owner, &self.supplied)?;
        if services::preparation_configuration(&scope(self.owner), owner, self.supplied.member_ref)?
            != registry.component::<crate::run::AgentConfigurationSnapshot>(
                &configuration,
                crate::provisioning::MAX_SNAPSHOT_BYTES as u64,
            )?
        {
            return Err(conflict());
        }
        let retained = manifest
            .as_ref()
            .map(|r| {
                registry
                    .component::<crate::direct_review_target::DirectTarget>(r, MANIFEST_BYTES)
                    .and_then(|t| t.metadata_bytes())
            })
            .transpose()?;
        if *accepted != self.expected || *accepted != *projection || contents.target != retained {
            return Err(conflict());
        }
        Ok(())
    }
}

pub(crate) fn projection_destination(
    owner: &VerifiedOwner,
    path: &Path,
) -> Result<(), MachineError> {
    let directory = PrivateDirectory::open(path.parent().ok_or_else(refusal)?)?;
    if directory.path.starts_with(&owner.root.path) || !path.is_absolute() {
        return Err(refusal());
    }
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(refusal)?;
    if name.is_empty() || name.len() > 121 || name.chars().any(char::is_control) {
        return Err(refusal());
    }
    no_pending(&directory, name)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            directory.open_file(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(refusal()),
    }
    Ok(())
}
pub(crate) fn write_projection(
    owner: &VerifiedOwner,
    path: &Path,
    value: &Value,
) -> Result<(), MachineError> {
    projection_destination(owner, path)?;
    let directory = PrivateDirectory::open(path.parent().ok_or_else(refusal)?)?;
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(refusal)?;
    let bytes = canonical(value)?;
    if bytes.len() as u64 > PUBLIC_BYTES {
        return Err(refusal());
    }
    no_pending(&directory, name)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let existing: Value = directory.read_bounded(path, PUBLIC_BYTES)?;
            if canonical(&existing)? != bytes {
                return Err(conflict());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => directory.publish(name, &bytes)?,
        Err(_) => return Err(refusal()),
    }
    Ok(())
}

pub(crate) fn selection_limit() -> u64 {
    SELECTION_BYTES
}
pub(crate) fn assignment_limit() -> u64 {
    ASSIGNMENT_BYTES
}
pub(crate) fn public_limit() -> u64 {
    PUBLIC_BYTES
}

fn check_caller(
    owner: &VerifiedOwner,
    caller: &crate::caller_scope::TrustedCallerScope,
) -> Result<(), MachineError> {
    owner.revalidate()?;
    if *caller.accepted() != owner.configuration.caller_scope {
        return Err(refusal());
    }
    Ok(())
}

pub(crate) fn install(
    home: &DolgoraeHome,
    owner: &VerifiedOwner,
    expected: &ProvisioningSourcesIdentity,
    key: &str,
    revision: Option<u64>,
) -> Result<
    (
        crate::provisioning::ProvisioningCandidate,
        crate::delegation_identity::OperationReceipt,
    ),
    MachineError,
> {
    owner.revalidate()?;
    permit_provisioning(owner)?;
    services::install_bound_provisioning(&scope(owner), expected, key, revision, || {
        fresh_provisioning(home, owner, expected)
    })
}

pub(crate) fn assign_external(
    owner: &VerifiedOwner,
    caller: &crate::caller_scope::TrustedCallerScope,
    port: &dyn services::RunSettlementPort,
    request: services::AssignExternalSpecialistTask<'_>,
    expected: AcceptedTask,
) -> Result<
    (
        services::DelegationTaskObservation,
        crate::delegation_identity::OperationReceipt,
    ),
    MachineError,
> {
    check_caller(owner, caller)?;
    if owner.configuration.owner != OwnerKind::External || request.target.is_some() {
        return Err(refusal());
    }
    let reference = request.engagement_id.to_string();
    owner.permit(Some(("external_engagement", &reference)))?;
    let supplied = AssignmentSelection {
        namespace: namespace(
            owner,
            Some(("external_engagement", &reference)),
            "assign_external_specialist_task",
            request.idempotency_key,
        )?,
        member_ref: request.member_ref,
        task: request.task.clone(),
        execution_intent: request.execution_intent.to_owned(),
        deadline_seconds: request.deadline_seconds,
        target: None,
    };
    let source = RegisteredAssignment {
        owner,
        supplied,
        expected,
        admission: None,
    };
    services::assign_external_with_source(&scope(owner), caller, port, request, &source)
}

pub(crate) fn assign_managed(
    owner: &VerifiedOwner,
    context: &services::ManagedCallContext<'_>,
    caller: &crate::caller_scope::TrustedCallerScope,
    port: &dyn services::RunSettlementPort,
    request: services::AssignSpecialistTask<'_>,
    expected: AcceptedTask,
) -> Result<
    (
        services::DelegationTaskObservation,
        crate::delegation_identity::OperationReceipt,
    ),
    MachineError,
> {
    check_caller(owner, caller)?;
    if owner.configuration.owner
        != (OwnerKind::Managed {
            session_ref: context.session_id,
        })
        || request.target.is_some()
    {
        return Err(refusal());
    }
    let reference = context.session_id.to_string();
    owner.permit(Some(("orchestrated_session", &reference)))?;
    let supplied = AssignmentSelection {
        namespace: namespace(
            owner,
            Some(("orchestrated_session", &reference)),
            "assign_specialist_task",
            context.idempotency_key,
        )?,
        member_ref: request.member_ref,
        task: request.task.clone(),
        execution_intent: request.execution_intent.to_owned(),
        deadline_seconds: request.deadline_seconds,
        target: None,
    };
    let source = RegisteredAssignment {
        owner,
        supplied,
        expected,
        admission: None,
    };
    services::assign_managed_with_source(&scope(owner), context, caller, port, request, &source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusted_host::{HostRoute, tests::Fixture};
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn mkdir(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    struct Scene {
        f: Fixture,
        selected: ProvisioningSelection,
        aggregate: SettlementOwner,
        member: Uuid,
        port: services::tests::FakePort,
    }
    impl Scene {
        fn new(managed: bool, route: HostRoute, git: bool) -> Self {
            Self::new_with_access(managed, route, git, false)
        }
        fn new_with_access(managed: bool, route: HostRoute, git: bool, write: bool) -> Self {
            Self::new_with_caller(managed, route, git, write, None)
        }
        fn new_with_caller(
            managed: bool,
            route: HostRoute,
            git: bool,
            write: bool,
            caller: Option<crate::delegation_identity::CallerScope>,
        ) -> Self {
            Self::new_with_registration(managed, route, git, write, caller, false)
        }
        fn new_with_registration(
            managed: bool,
            route: HostRoute,
            git: bool,
            write: bool,
            caller: Option<crate::delegation_identity::CallerScope>,
            native: bool,
        ) -> Self {
            let mut f = Fixture::new(route);
            if native {
                f.draft.tools = vec![
                    crate::native_consumer::EXTERNAL_NATIVE.into(),
                    crate::native_consumer::EXTERNAL_CONTROL.into(),
                ];
            }
            if let Some(caller) = caller {
                f.draft.caller_scope = caller;
            }

            if git {
                fs::remove_dir_all(
                    f.home
                        .workspace_root(&crate::workspace::workspace_id(&f.workspace)),
                )
                .unwrap();
                fs::remove_dir_all(&f.workspace).unwrap();
                mkdir(&f.workspace);
                let run = |args: &[&str]| {
                    let result = std::process::Command::new("/usr/bin/git")
                        .arg("-C")
                        .arg(&f.workspace)
                        .args([
                            "-c",
                            "user.email=fixture@example.invalid",
                            "-c",
                            "user.name=Fixture",
                        ])
                        .args(args)
                        .output()
                        .unwrap();
                    assert!(
                        result.status.success(),
                        "{}",
                        String::from_utf8_lossy(&result.stderr)
                    );
                };
                fs::write(f.workspace.join("file"), b"base\n").unwrap();
                run(&["init", "-q"]);
                run(&["add", "file"]);
                run(&["commit", "-q", "-m", "base"]);
                crate::workspace::WorkspaceService::new(
                    crate::workspace::SystemWorkspacePlatform,
                    crate::workspace::SystemGitRunner,
                    f.home.root().to_path_buf(),
                )
                .initialize(Some(&f.workspace), crate::workspace::WorkspaceMode::Git)
                .unwrap();
            }
            if managed {
                f = crate::trusted_host::tests::configure_managed_fixture(f);
                let OwnerKind::Managed { session_ref } = f.draft.owner else {
                    unreachable!()
                };
                let mut sessions =
                    crate::orchestration::OrchestrationStore::open(&f.owner().state_root).unwrap();
                sessions.mark_primary_intent(session_ref).unwrap();
                sessions
                    .finish_primary_publication(session_ref, Ok(()))
                    .unwrap();
            } else {
                f.complete();
            }
            InstructionSourceStore::new(&f.home)
                .initialize_templates()
                .unwrap();
            let mut binding: crate::global_runtime::GlobalProfileBinding =
                serde_json::from_str::<crate::run::CallerStartReservation>(include_str!(
                    "../docs/protocol/examples/workspace-execution-start-reservation.valid.json"
                ))
                .unwrap()
                .accepted_intent
                .global_profile_binding;
            let codex = f.root.join("profile-home");
            mkdir(&codex);
            binding.launch_snapshot.canonical_codex_home = codex.to_str().unwrap().to_owned();
            binding.launch_snapshot.codex_version = "0.164.0".to_owned();
            binding.launch_snapshot.executable_identity.sha256 =
                "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf".to_owned();
            binding.launch_snapshot_sha256 =
                crate::jcs::sha256_hex(&canonical(&binding.launch_snapshot).unwrap());
            let binding = crate::profile::qualified_test_binding(&binding);
            let selected = ProvisioningSelection {
                role_name: "reviewer".to_owned(),
                role: SelectedInstructionSource::File(vec!["roles".into(), "reviewer".into()]),
                dependencies: vec![SelectedNamedSource {
                    name: "contract".into(),
                    source: SelectedInstructionSource::Inline("complete contract\n".into()),
                }],
                selected_guidance: vec![],
                profile_binding: binding,
                admission: ProvisioningAdmission {
                    model: "gpt-5.6".into(),
                    effort: "high".into(),
                    purpose_kind: "review".into(),
                    purpose_label: None,
                    required_capabilities: vec![],
                    execution_lane: "dedicated".into(),
                    requested_access: if write {
                        "canonical_workspace_write"
                    } else {
                        "read_only"
                    }
                    .into(),
                    required_assurance: "best_effort_personal_alpha".into(),
                    native_subagent_policy: "disabled".into(),
                },
            };
            let owner = f.owner();
            initialize(&owner).unwrap();
            let projection = prepare_provisioning(&f.home, &owner, selected.clone()).unwrap();
            let (candidate, _) = install(&f.home, &owner, &projection, "install", None).unwrap();
            let port = services::tests::FakePort::new();
            let aggregate = match f.draft.owner {
                OwnerKind::External => SettlementOwner::External {
                    engagement_id: Uuid::try_parse(
                        &services::open_external_engagement(&scope(&owner), "open", None)
                            .unwrap()
                            .0
                            .engagement_ref,
                    )
                    .unwrap(),
                },
                OwnerKind::Managed { session_ref } => SettlementOwner::Managed {
                    session_id: session_ref,
                },
            };
            let member = match aggregate {
                SettlementOwner::External { engagement_id } => {
                    services::hire_external_specialist(
                        &scope(&owner),
                        &port,
                        services::HireExternalSpecialist {
                            engagement_id,
                            provisioning_ref: &candidate.provisioning_ref,
                            role_ref: "reviewer",
                            agent_configuration_digest: &candidate.agent_configuration_digest,
                            idempotency_key: "hire",
                            expected_revision: None,
                        },
                    )
                    .unwrap()
                    .member
                    .member_ref
                }
                SettlementOwner::Managed { session_id } => {
                    services::spawn_specialist(
                        &scope(&owner),
                        &services::ManagedCallContext {
                            session_id,
                            source_run_id: session_id,
                            source_turn_id: "root-turn",
                            source_tool_call_id: "root-call",
                            idempotency_key: "spawn",
                        },
                        &port,
                        services::SpawnSpecialist {
                            provisioning_ref: &candidate.provisioning_ref,
                            role_ref: "reviewer",
                            agent_configuration_digest: &candidate.agent_configuration_digest,
                            expected_revision: None,
                        },
                    )
                    .unwrap()
                    .member
                    .member_ref
                }
            };
            let member = Uuid::try_parse(&member).unwrap();
            let snapshot = crate::provisioning::retained_snapshot(
                &owner.state_root,
                &candidate.provisioning_ref,
            )
            .unwrap();
            let operation=Uuid::try_parse(&crate::engagement::EngagementStore::open_read_only(&crate::engagement::EngagementStore::workspace_database_path(&owner.state_root)).unwrap().connection().query_row("SELECT operation_ref FROM delegation_operations WHERE operation_kind IN ('hire_external_specialist','spawn_specialist') ORDER BY sequence DESC LIMIT 1",[],|r|r.get::<_,String>(0)).unwrap()).unwrap();
            let controller = if managed {
                crate::orchestration::OrchestrationStore::open(&owner.state_root)
                    .unwrap()
                    .member_credential(member)
                    .unwrap()
                    .binding
                    .clone()
            } else {
                owner.controller.clone()
            };
            let mut manifest = services::tests::member_manifest(
                &owner.view,
                &controller,
                member,
                aggregate.scope_ref(),
                operation,
                &snapshot,
            );
            if write {
                manifest.requested_access = Some(crate::domain::CallerWorkspaceAccess::Writable);
                manifest.initial_access = crate::domain::Access::Write;
                manifest.creation_mode = Some("writable".into());
            }
            manifest.aggregate_binding.as_mut().unwrap().aggregate_kind = if managed {
                crate::domain::AggregateKind::OrchestratedSession
            } else {
                crate::domain::AggregateKind::ExternalSpecialistEngagement
            };
            if !managed {
                manifest.parent_ref.as_mut().unwrap().namespace =
                    "dolgorae.external-specialist-engagement.v1".into();
            }
            crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &owner.state_root)
                .publish(&manifest)
                .unwrap();
            if !write {
                assert!(services::qualified_successor_review(&manifest));
            }
            drop(crate::orchestration::OrchestrationStore::open(&owner.state_root).unwrap());
            Self {
                f,
                selected,
                aggregate,
                member,
                port,
            }
        }
        fn selection(
            &self,
            key: &str,
            kind: Option<(&str, Option<&str>)>,
            output: &str,
        ) -> AssignmentSelection {
            let owner = self.f.owner();
            let mut task = services::fixture_task();
            task.expected_output = output.into();
            AssignmentSelection {
                namespace: namespace(
                    &owner,
                    Some((
                        self.aggregate.scope_kind(),
                        &self.aggregate.scope_ref().to_string(),
                    )),
                    if matches!(self.aggregate, SettlementOwner::External { .. }) {
                        "assign_external_specialist_task"
                    } else {
                        "assign_specialist_task"
                    },
                    key,
                )
                .unwrap(),
                member_ref: self.member,
                task,
                execution_intent: "read_only".into(),
                deadline_seconds: 600,
                target: kind.map(|(kind, revision)| crate::review::ReviewTargetRequest {
                    kind: kind.into(),
                    revision: revision.map(str::to_owned),
                }),
            }
        }
        fn assign(
            &self,
            selected: &AssignmentSelection,
            expected: AcceptedTask,
        ) -> Result<
            (
                services::DelegationTaskObservation,
                crate::delegation_identity::OperationReceipt,
            ),
            MachineError,
        > {
            let owner = self.f.owner();
            let caller = crate::caller_scope::TrustedCallerScope::from_port(
                &crate::trusted_host::ConfiguredCallerPort(&owner),
            )
            .unwrap();
            match self.aggregate {
                SettlementOwner::External { engagement_id } => assign_external(
                    &owner,
                    &caller,
                    &self.port,
                    services::AssignExternalSpecialistTask {
                        engagement_id,
                        member_ref: selected.member_ref,
                        task: selected.task.clone(),
                        execution_intent: &selected.execution_intent,
                        target: None,
                        deadline_seconds: selected.deadline_seconds,
                        idempotency_key: &selected.namespace.idempotency_key,
                        expected_revision: None,
                    },
                    expected,
                ),
                SettlementOwner::Managed { session_id } => assign_managed(
                    &owner,
                    &services::ManagedCallContext {
                        session_id,
                        source_run_id: session_id,
                        source_turn_id: "root-turn",
                        source_tool_call_id: "root-call",
                        idempotency_key: &selected.namespace.idempotency_key,
                    },
                    &caller,
                    &self.port,
                    services::AssignSpecialistTask {
                        member_ref: selected.member_ref,
                        task: selected.task.clone(),
                        execution_intent: &selected.execution_intent,
                        target: None,
                        deadline_seconds: selected.deadline_seconds,
                        expected_revision: None,
                    },
                    expected,
                ),
            }
        }
        fn effects(&self) -> Vec<i64> {
            let owner = self.f.owner();
            let store = crate::engagement::EngagementStore::open_read_only(
                &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
            )
            .unwrap();
            [
                "delegation_operations",
                "tasks",
                "brokered_tasks",
                "caller_scope_assignments",
            ]
            .iter()
            .map(|table| {
                store
                    .connection()
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap()
            })
            .collect()
        }
        fn git(&self, args: &[&str]) -> String {
            let output = std::process::Command::new("/usr/bin/git")
                .arg("-C")
                .arg(&self.f.workspace)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }
    }

    #[test]
    fn native_managed_refusal_completion_preserves_failure_body_and_false_flag() {
        let scene = Scene::new(true, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let root = scene.aggregate.scope_ref();
        let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"release_specialist","scope_kind":"orchestrated_session","scope_ref":root,"member_ref":scene.member,"expected_revision":1});
        let (body, native, _) = crate::turn::tests::native_worker_completion(
            &owner.state_root,
            root,
            &owner.view.workspace_id,
            &scene.f.workspace,
            crate::specialist_facade::MANAGED_TOOL,
            request,
        )
        .unwrap();
        assert_eq!(body["schema"], "dolgorae.external-specialist-failure/v5");
        assert_eq!(body["error"]["effect"], "none");
        assert_eq!(body["error"]["classification"], "RecoveryNeeded");
        assert_eq!(body["error"]["code"], "RECOVERY_REQUIRED");
        assert_eq!(
            native["success"], false,
            "managed pre-admission failure reported success"
        );
        assert_eq!(
            native["contentItems"][0]["text"],
            serde_json::to_string(&body).unwrap()
        );
    }
    #[test]
    fn native_managed_unknown_completion_preserves_original_operation_and_false_flag() {
        let scene = Scene::new(true, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let root = scene.aggregate.scope_ref();
        crate::managed_specialist::connect(&owner).unwrap();
        let revision = services::transport_member(
            &scope(&owner),
            scene.aggregate,
            scene.member,
            Some(&services::ManagedCallContext {
                session_id: root,
                source_run_id: root,
                source_turn_id: "turn",
                source_tool_call_id: "call",
                idempotency_key: "unused",
            }),
        )
        .unwrap()
        .revision;
        let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"release_specialist","scope_kind":"orchestrated_session","scope_ref":root,"member_ref":scene.member,"expected_revision":revision});
        let (body, native, _) = crate::turn::tests::native_worker_completion(
            &owner.state_root,
            root,
            &owner.view.workspace_id,
            &scene.f.workspace,
            crate::specialist_facade::MANAGED_TOOL,
            request,
        )
        .unwrap();
        assert_eq!(body["schema"], "dolgorae.external-specialist-failure/v5");
        assert_eq!(body["error"]["effect"], "unresolved", "{body}");
        assert_eq!(body["error"]["classification"], "Unknown", "{body}");
        let db = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
        )
        .unwrap();
        let original = crate::engagement::member_release_operation(
            db.connection(),
            &owner.view.workspace_id,
            "orchestrated_session",
            &root.to_string(),
            &scene.member.to_string(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(body["error"]["operation_ref"], original.operation_ref);
        assert_eq!(
            native["success"], false,
            "managed post-admission Unknown reported success"
        );
        assert_eq!(
            native["contentItems"][0]["text"],
            serde_json::to_string(&body).unwrap()
        );
    }
    #[test]
    fn native_managed_success_and_legacy_completion_controls_keep_their_contracts() {
        let scene = Scene::new(true, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let root = scene.aggregate.scope_ref();
        crate::managed_specialist::connect(&owner).unwrap();
        let ns = namespace(&owner, None, "install_provisioning", "install").unwrap();
        let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"lookup_operation","namespace":ns});
        let (body, native, _) = crate::turn::tests::native_worker_completion(
            &owner.state_root,
            root,
            &owner.view.workspace_id,
            &scene.f.workspace,
            crate::specialist_facade::MANAGED_TOOL,
            request,
        )
        .unwrap();
        assert_eq!(body["schema"], "dolgorae.external-specialist-response/v5");
        assert_eq!(native["success"], true);
        assert_eq!(
            native["contentItems"][0]["text"],
            serde_json::to_string(&body).unwrap()
        );
        for operation in ["list_specialists", "unsupported_operation"] {
            let (body, native, _) = crate::turn::tests::native_worker_completion(
                &owner.state_root,
                root,
                &owner.view.workspace_id,
                &scene.f.workspace,
                crate::primary_tool::PRIMARY_TOOL_NAME,
                json!({"operation":operation}),
            )
            .unwrap();
            assert_eq!(native["success"], operation == "list_specialists", "{body}");
            assert_eq!(
                native["contentItems"][0]["text"],
                serde_json::to_string(&body).unwrap()
            );
        }
    }

    #[test]
    fn native_managed_rejected_completion_preserves_stale_admission_failure() {
        let scene = Scene::new(true, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let root = scene.aggregate.scope_ref();
        crate::managed_specialist::connect(&owner).unwrap();
        let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"release_specialist","scope_kind":"orchestrated_session","scope_ref":root,"member_ref":scene.member,"expected_revision":u32::MAX});
        let (body, native, _) = crate::turn::tests::native_worker_completion(
            &owner.state_root,
            root,
            &owner.view.workspace_id,
            &scene.f.workspace,
            crate::specialist_facade::MANAGED_TOOL,
            request,
        )
        .unwrap();
        assert_eq!(body["error"]["classification"], "Rejected", "{body}");
        assert_eq!(body["error"]["effect"], "none");
        assert_eq!(body["error"]["code"], "REVISION_CONFLICT");
        assert_eq!(native["success"], false);
        assert_eq!(
            native["contentItems"][0]["text"],
            serde_json::to_string(&body).unwrap()
        );
        assert_eq!(
            crate::semantic::release_probe::release_row_count(&owner.state_root),
            0
        );
    }

    fn produced_collect_native_fixture(quote_heavy: bool) -> usize {
        let scene = Scene::new_with_access(true, HostRoute::Codex, true, true);
        let owner = scene.f.owner();
        let root = scene.aggregate.scope_ref();
        crate::managed_specialist::connect(&owner).unwrap();
        let mut selected = scene.selection("full-collect", None, "complete_result");
        selected.execution_intent = "canonical_workspace_write".into();
        let accepted = prepare_assignment(&owner, selected.clone()).unwrap();
        let (observed, receipt) = scene.assign(&selected, accepted.clone()).unwrap();
        let task = Uuid::parse_str(&observed.task_ref).unwrap();
        assert_eq!(
            receipt.accepted_subjects.task_ref.as_deref(),
            Some(observed.task_ref.as_str())
        );
        let exclude = scene.f.workspace.join(".git/info/exclude");
        let mut ignored = fs::read_to_string(&exclude).unwrap();
        ignored.push_str("\n/.dolgorae/\n");
        fs::write(&exclude, ignored).unwrap();
        let initial =
            crate::successor_capture::capture_dispatch_index(&scene.f.workspace, task).unwrap();
        assert!(initial.file_count <= 8);
        let initial_facts =
            crate::successor_capture::parse_dispatch_index(&initial.canonical, task).unwrap();
        for entry in &initial_facts.entries {
            fs::write(scene.f.workspace.join(&entry.path), b"").unwrap();
        }
        let added_before = 10000 - initial.file_count as usize;
        let segment = if quote_heavy {
            "\"".repeat(210)
        } else {
            format!("{}\"界", "q".repeat(175))
        };
        let mut directory = scene.f.workspace.clone();
        for _ in 0..4 {
            directory.push(&segment);
        }
        fs::create_dir_all(&directory).unwrap();
        for index in 0..added_before {
            fs::write(directory.join(format!("before-{index:05}")), b"").unwrap();
        }
        let before =
            crate::successor_capture::capture_dispatch_index(&scene.f.workspace, task).unwrap();
        assert_eq!(before.file_count, 10000);
        assert!(before.canonical.len() <= 64 * 1024 * 1024);
        let facts =
            crate::successor_capture::parse_dispatch_index(&before.canonical, task).unwrap();
        for entry in &initial_facts.entries {
            fs::remove_file(scene.f.workspace.join(&entry.path)).unwrap();
        }
        for index in 0..added_before {
            fs::remove_file(directory.join(format!("before-{index:05}"))).unwrap();
        }
        for index in 0..10000 {
            fs::write(directory.join(format!("after-{index:05}")), b"").unwrap();
        }
        scene.git(&["add", "-A"]);
        let after =
            crate::successor_capture::capture_dispatch_index(&scene.f.workspace, task).unwrap();
        assert_eq!(after.file_count, 10000);
        assert!(after.canonical.len() <= 64 * 1024 * 1024);
        let produced = crate::successor_capture::complete_result_changes(
            &scene.f.workspace,
            &facts,
            &before.baseline,
        )
        .unwrap();
        assert_eq!(produced.changes.len(), 20000);
        assert_eq!(
            produced.changes.iter().filter(|c| c.kind == "add").count(),
            10000
        );
        assert_eq!(
            produced
                .changes
                .iter()
                .filter(|c| c.kind == "delete")
                .count(),
            10000
        );
        assert!(
            produced
                .retained
                .iter()
                .map(|side| side.bytes.len())
                .sum::<usize>()
                <= 8 * 1024 * 1024
        );
        assert!(produced.retained.iter().all(|side| side.bytes.is_empty()));
        let complete = crate::delegation_identity::CompleteResult {
            schema: crate::delegation_identity::COMPLETE_RESULT_SCHEMA.into(),
            source: crate::delegation_identity::ResultSource {
                scope_kind: "orchestrated_session".into(),
                scope_ref: root.to_string(),
                worker_ref: scene.member.to_string(),
                task_ref: task.to_string(),
                role_snapshot_digest: accepted.role_snapshot_digest.clone(),
                agent_configuration_digest: accepted.agent_configuration_digest.clone(),
                accepted_task_digest: accepted.digest().unwrap(),
            },
            terminal_outcome: "completed".into(),
            final_response: None,
            structured_output_schema: None,
            structured_output: None,
            changes: produced.changes,
            attachments: vec![],
            safe_failure_code: None,
        };
        let _digest = complete.digest().unwrap();
        let bytes = canonical(&complete).unwrap();
        let mut db = crate::engagement::EngagementStore::open(
            &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
        )
        .unwrap();
        db.transact(|tx| {
            crate::orchestration::retain_successor_task_result(tx, task, &bytes)?;
            assert!(crate::orchestration::settle_successor_task_result(
                tx,
                root,
                task,
                "completed_not_delivered",
                None,
                Some(&crate::jcs::sha256_hex(&bytes)),
                2
            )?);
            Ok(())
        })
        .unwrap();
        drop(db);
        let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"collect_specialist_task","scope_kind":"orchestrated_session","scope_ref":root,"member_ref":scene.member,"task_ref":task});
        let (body, native, wire) = crate::turn::tests::native_worker_completion(
            &owner.state_root,
            root,
            &owner.view.workspace_id,
            &scene.f.workspace,
            crate::specialist_facade::MANAGED_TOOL,
            request,
        )
        .unwrap();
        assert_eq!(
            body["schema"], "dolgorae.external-specialist-response/v5",
            "{body}"
        );
        assert_eq!(native["success"], true);
        assert_eq!(
            body["task"]["complete_result"]["changes"]
                .as_array()
                .unwrap()
                .len(),
            20000
        );
        assert_eq!(
            canonical(&body["task"]["complete_result"]).unwrap(),
            bytes,
            "collect changed the retained complete result"
        );
        let text = serde_json::to_string(&body).unwrap();
        assert_eq!(native["contentItems"][0]["text"], text);
        let messages = crate::app_server::tests::client_messages(&wire);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].len() > crate::app_server::MAX_WEBSOCKET_FRAME_BYTES);
        let expected = serde_json::to_vec(&json!({"id":73,"result":native})).unwrap();
        assert_eq!(
            messages[0], expected,
            "native JSON-string wrapper lost bytes"
        );
        assert!(crate::app_server::tests::client_frames(&wire).len() > 1);
        println!(
            "full-response producer before_index={} after_index={} refs=20000 complete_bytes={} native_bytes={} frames={}",
            before.canonical.len(),
            after.canonical.len(),
            bytes.len(),
            expected.len(),
            crate::app_server::tests::client_frames(&wire).len()
        );
        expected.len()
    }

    #[test]
    fn native_collect_full_produced_change_references_survive_serialization_and_send() {
        let bytes = produced_collect_native_fixture(false);
        assert!(
            bytes <= 64 * 1024 * 1024,
            "ordinary long-path example exceeds the selected native receiver message cap"
        );
    }
    #[test]
    fn native_collect_quote_heavy_producer_exceeds_selected_receiver_message_cap() {
        let bytes = produced_collect_native_fixture(true);
        assert!(
            bytes > 64 * 1024 * 1024,
            "the source-bound receiver-cap counterexample was not established"
        );
        println!(
            "receiver compatibility gap: valid native JSON message={} bytes > selected source max_message_size={} bytes; offline sender exactness is not native acceptance",
            bytes,
            64 * 1024 * 1024
        );
    }

    #[test]
    fn native_facade_source_pipeline_uses_sealed_context_and_retained_product_receipts() {
        use crate::specialist_facade as facade;
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let scene = Scene::new(false, route, true);
            let owner = scene.f.owner();
            let projection =
                prepare_provisioning(&scene.f.home, &owner, scene.selected.clone()).unwrap();
            let selected = scene.selection(
                "native-assign",
                Some(("head", None)),
                "structured_review_v4",
            );
            let expected = prepare_assignment(&owner, selected.clone()).unwrap();
            let installed=crate::provisioning::retained_snapshot(&owner.state_root,&crate::engagement::EngagementStore::open_read_only(&crate::engagement::EngagementStore::workspace_database_path(&owner.state_root)).unwrap().connection().query_row("SELECT json_extract(receipt_json,'$.accepted_subjects.provisioning_ref') FROM delegation_operations WHERE operation_kind='install_provisioning' LIMIT 1",[],|r|r.get::<_,String>(0)).unwrap()).unwrap().candidate(true).unwrap();
            let requests = [
                json!({"schema":"dolgorae.external-specialist-request/v5","operation":"install_provisioning","sources":projection,"idempotency_key":"native-install","expected_revision":null}),
                json!({"schema":"dolgorae.external-specialist-request/v5","operation":"hire_external_specialist","scope_kind":"external_engagement","scope_ref":scene.aggregate.scope_ref(),"provisioning_ref":installed.provisioning_ref,"role_ref":"reviewer","agent_configuration_digest":installed.agent_configuration_digest,"idempotency_key":"native-hire","expected_revision":null}),
                json!({"schema":"dolgorae.external-specialist-request/v5","operation":"assign_external_specialist_task","scope_kind":"external_engagement","scope_ref":scene.aggregate.scope_ref(),"member_ref":scene.member,"assignment":{"task_body":selected.task,"accepted_task_identity":expected,"deadline_seconds":selected.deadline_seconds},"idempotency_key":"native-assign","expected_revision":null}),
            ];
            let mut receipts = Vec::new();
            for (index, request) in requests.iter().enumerate() {
                let call = format!("native-{index}");
                let native = crate::host_invocation::NativeEvent {
                    session: "native-thread".into(),
                    turn: Some("native-turn".into()),
                    tool_use: call.clone(),
                    server: "dolgorae".into(),
                    server_source: if route == HostRoute::Claude {
                        Some("dynamic".into())
                    } else {
                        None
                    },
                    tool: "specialist".into(),
                };
                let (binding, cap) = {
                    let store = scene.f.ready();
                    store.bind_native(native, request.clone()).unwrap()
                };
                let deliver = || {
                    facade::external_with_port(
                        &scene.f.home,
                        &scene.f.path(),
                        "specialist",
                        if route == HostRoute::Codex {
                            facade::NativeCarrier::Codex {
                                session: "native-thread",
                                call: &call,
                            }
                        } else {
                            facade::NativeCarrier::Claude(&cap)
                        },
                        request,
                        &scene.port,
                    )
                };
                let first = deliver();
                assert_eq!(
                    first["schema"], "dolgorae.external-specialist-response/v5",
                    "{first}"
                );
                let effects = scene.effects();
                let calls = scene.port.calls();
                let repeat = deliver();
                assert_eq!(repeat["schema"], first["schema"], "{repeat}");
                assert_eq!(repeat["receipt"], first["receipt"]);
                assert_eq!(scene.effects(), effects);
                assert_eq!(scene.port.calls(), calls, "repeat redispatched");
                let store = crate::engagement::EngagementStore::open_read_only(
                    &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
                )
                .unwrap();
                let context:String=store.connection().query_row("SELECT trusted_invocation_json FROM delegation_operations WHERE operation_ref=?1",[first["receipt"]["operation_ref"].as_str().unwrap()],|r|r.get(0)).unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&context).unwrap(),
                    serde_json::to_value(binding.context()).unwrap()
                );
                receipts.push(first);
            }
            fs::remove_dir_all(owner.root.path.join(DIRECTORY)).unwrap();
            fs::remove_dir_all(scene.f.home.root().join("roles")).unwrap();
            let request = &requests[0];
            let reply = facade::external_with_port(
                &scene.f.home,
                &scene.f.path(),
                "specialist",
                facade::NativeCarrier::Codex {
                    session: "native-thread",
                    call: "native-0",
                },
                request,
                &scene.port,
            );
            if route == HostRoute::Codex {
                assert_eq!(
                    reply["receipt"], receipts[0]["receipt"],
                    "accepted replay read live preparation"
                );
            }
        }
    }

    #[test]
    fn native_cold_stdio_await_releases_host_lock_and_eof_preserves_accepted_work() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};
        let scene = Scene::new(false, HostRoute::Codex, true);
        let owner = scene.f.owner();
        let selected =
            scene.selection("overlap-task", Some(("head", None)), "structured_review_v4");
        let expected = prepare_assignment(&owner, selected.clone()).unwrap();
        let (observed, _) = scene.assign(&selected, expected).unwrap();
        let task_ref = observed.task_ref.clone();
        let before = scene.effects();
        let await_request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"await_external_specialist_task","scope_kind":"external_engagement","scope_ref":scene.aggregate.scope_ref(),"member_ref":scene.member,"task_ref":task_ref,"timeout_seconds":3});
        let open_request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"open_external_engagement","workspace_ref":owner.view.workspace_id,"idempotency_key":"parallel-open","expected_revision":null});
        let hook = |request: &Value, call: &str| json!({"hook_event_name":"PreToolUse","session_id":"cold-thread","tool_use_id":call,"tool_name":"mcp__dolgorae__specialist","tool_input":request});
        crate::specialist_stdio::hook(
            &scene.f.home,
            &scene.f.path(),
            &serde_json::to_vec(&hook(&await_request, "waiting")).unwrap(),
        )
        .unwrap();
        let frames = |request: &Value, call: &str, id: u64| {
            format!(
                "{}\n{}\n{}\n",
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cold","version":"1"}}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"specialist","arguments":request,"_meta":{"threadId":"cold-thread","callId":call}}})
            )
        };
        let command = |mode: &str| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "specialist_stdio::tests::cold_stdio_child",
                    "--nocapture",
                ])
                .env("DOLGORAE_SPECIALIST_STDIO_OWNER", scene.f.path())
                .env("DOLGORAE_SPECIALIST_CHILD_MODE", mode)
                .env("HOME", scene.f.home.root().parent().unwrap())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command
        };
        let extract = |bytes: &[u8]| {
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .lines()
                .filter_map(|line| {
                    line.find('{')
                        .and_then(|at| serde_json::from_str::<Value>(&line[at..]).ok())
                })
                .collect::<Vec<_>>()
        };
        let entered = scene.f.root.join("await-entered");
        let completed = scene.f.root.join("await-completed");
        let mut waiting = command("host-mcp")
            .env("DOLGORAE_SPECIALIST_AWAIT_ENTERED", &entered)
            .env("DOLGORAE_SPECIALIST_AWAIT_COMPLETED", &completed)
            .spawn()
            .unwrap();
        waiting
            .stdin
            .take()
            .unwrap()
            .write_all(frames(&await_request, "waiting", 22).as_bytes())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !entered.exists() {
            assert!(
                waiting.try_wait().unwrap().is_none(),
                "stdio exited before product wait"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "product wait not entered"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut publisher = command("host-hook").spawn().unwrap();
        publisher
            .stdin
            .take()
            .unwrap()
            .write_all(
                serde_json::to_string(&hook(&open_request, "parallel"))
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        let published = publisher.wait_with_output().unwrap();
        assert!(
            published.status.success(),
            "{}",
            String::from_utf8_lossy(&published.stderr)
        );
        assert_eq!(
            extract(&published.stdout)[0]["hookSpecificOutput"]["permissionDecision"],
            "allow"
        );
        let mut parallel = command("host-mcp").spawn().unwrap();
        parallel
            .stdin
            .take()
            .unwrap()
            .write_all(frames(&open_request, "parallel", 33).as_bytes())
            .unwrap();
        let accepted = parallel.wait_with_output().unwrap();
        assert!(
            accepted.status.success(),
            "{}",
            String::from_utf8_lossy(&accepted.stderr)
        );
        let accepted = extract(&accepted.stdout)
            .into_iter()
            .find(|v| v["id"] == 33)
            .unwrap()["result"]["structuredContent"]
            .clone();
        assert!(accepted.get("receipt").is_some(), "{accepted}");
        assert!(
            waiting.try_wait().unwrap().is_none(),
            "other delivery did not overlap the real wait"
        );
        assert!(
            !completed.exists(),
            "independent delivery completed only after the actual product await returned; stdout backpressure is not overlap"
        );
        let waited = waiting.wait_with_output().unwrap();
        assert!(
            waited.status.success(),
            "{}",
            String::from_utf8_lossy(&waited.stderr)
        );
        let waited = extract(&waited.stdout)
            .into_iter()
            .find(|v| v["id"] == 22)
            .unwrap();
        assert_eq!(
            waited["result"]["structuredContent"]["wait_timed_out"], true,
            "{waited}"
        );
        let mut reconnected = command("host-mcp").spawn().unwrap();
        reconnected
            .stdin
            .take()
            .unwrap()
            .write_all(frames(&open_request, "parallel", 44).as_bytes())
            .unwrap();
        let replayed = reconnected.wait_with_output().unwrap();
        assert!(replayed.status.success());
        let replayed = extract(&replayed.stdout)
            .into_iter()
            .find(|v| v["id"] == 44)
            .unwrap();
        assert_eq!(
            replayed["result"]["structuredContent"]["receipt"],
            accepted["receipt"]
        );
        let get = services::await_external_specialist_tasks(
            &scope(&owner),
            services::AwaitExternalSpecialistTasks {
                engagement_id: scene.aggregate.scope_ref(),
                member_ref: scene.member,
                tasks: &[Uuid::parse_str(&task_ref).unwrap()],
                predicate: services::AwaitPredicate::All,
                timeout_seconds: 0,
            },
        )
        .unwrap();
        assert_eq!(get.0[0], observed);
        assert_eq!(scene.effects()[1..], before[1..]);
    }

    #[test]
    fn managed_facade_k_joins_keep_native_provenance_and_null_external_context() {
        use crate::specialist_facade as facade;
        let scene = Scene::new(true, HostRoute::Codex, true);
        let owner = scene.f.owner();
        let root = scene.aggregate.scope_ref();
        let projection =
            prepare_provisioning(&scene.f.home, &owner, scene.selected.clone()).unwrap();
        let installed=crate::provisioning::retained_snapshot(&owner.state_root,&crate::engagement::EngagementStore::open_read_only(&crate::engagement::EngagementStore::workspace_database_path(&owner.state_root)).unwrap().connection().query_row("SELECT json_extract(receipt_json,'$.accepted_subjects.provisioning_ref') FROM delegation_operations WHERE operation_kind='install_provisioning' LIMIT 1",[],|r|r.get::<_,String>(0)).unwrap()).unwrap().candidate(true).unwrap();
        let selected = scene.selection(
            "managed-facade-assign",
            Some(("head", None)),
            "structured_review_v4",
        );
        let expected = prepare_assignment(&owner, selected.clone()).unwrap();
        let requests = [
            json!({"schema":"dolgorae.external-specialist-request/v5","operation":"install_provisioning","sources":projection,"idempotency_key":"managed-facade-install","expected_revision":null}),
            json!({"schema":"dolgorae.external-specialist-request/v5","operation":"spawn_specialist","scope_kind":"orchestrated_session","scope_ref":root,"provisioning_ref":installed.provisioning_ref,"role_ref":"reviewer","agent_configuration_digest":installed.agent_configuration_digest,"idempotency_key":"managed-facade-spawn","expected_revision":null}),
            json!({"schema":"dolgorae.external-specialist-request/v5","operation":"assign_specialist_task","scope_kind":"orchestrated_session","scope_ref":root,"member_ref":scene.member,"assignment":{"task_body":selected.task,"accepted_task_identity":expected,"deadline_seconds":selected.deadline_seconds},"idempotency_key":"managed-facade-assign","expected_revision":null}),
        ];
        for request in requests {
            let key = request["idempotency_key"].as_str().unwrap();
            let context = services::ManagedCallContext {
                session_id: root,
                source_run_id: root,
                source_turn_id: "authenticated-turn",
                source_tool_call_id: "authenticated-call",
                idempotency_key: key,
            };
            let response = facade::dispatch(
                &scene.f.home,
                &owner,
                &scope(&owner),
                Some(&context),
                &scene.port,
                &request,
                None,
            )
            .unwrap();
            let operation = request["operation"].as_str().unwrap();
            assert!(
                facade::validates(&response, &format!("{operation}_response")),
                "{response}"
            );
            let before = scene.effects();
            let calls = scene.port.calls();
            let repeat = facade::dispatch(
                &scene.f.home,
                &owner,
                &scope(&owner),
                Some(&context),
                &scene.port,
                &request,
                None,
            )
            .unwrap();
            assert_eq!(repeat["receipt"], response["receipt"]);
            assert_eq!(scene.effects(), before);
            assert_eq!(scene.port.calls(), calls);
            let store = crate::engagement::EngagementStore::open_read_only(
                &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
            )
            .unwrap();
            let(envelope,external):(String,Option<String>)=store.connection().query_row("SELECT mutation_envelope_json,trusted_invocation_json FROM delegation_operations WHERE operation_ref=?1",[response["receipt"]["operation_ref"].as_str().unwrap()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert!(external.is_none());
            assert!(
                serde_json::from_str::<Value>(&envelope).unwrap()["trusted_invocation_ref"]
                    .is_null()
            );
        }
    }

    #[test]
    fn state_owner_cancellation_and_close_are_durable_before_effect_and_repeats_only_observe() {
        struct Probe<'a> {
            base: &'a services::tests::FakePort,
            cancelled: std::cell::Cell<usize>,
            closed: std::cell::Cell<usize>,
            fault: u8,
            original: std::cell::RefCell<Option<Value>>,
        }
        impl services::RunSettlementPort for Probe<'_> {
            fn member_run_observation(
                &self,
                r: services::MemberCleanupObservation<'_>,
                budget: &mut crate::ledger::ObservationBudget,
            ) -> Result<crate::snapshot::MemberRunObservation, MachineError> {
                services::RunSettlementPort::member_run_observation(self.base, r, budget)
            }
            fn member_interaction_observation(
                &self,
                r: services::MemberCleanupObservation<'_>,
                source: &crate::snapshot::MemberRunObservation,
                budget: &mut crate::ledger::ObservationBudget,
            ) -> Result<(bool, crate::domain::ProjectionStamp), MachineError> {
                services::RunSettlementPort::member_interaction_observation(
                    self.base, r, source, budget,
                )
            }
            fn start_member_run(
                &self,
                r: services::AdmittedMemberStart<'_>,
            ) -> Result<services::MemberRunStart, MachineError> {
                services::RunSettlementPort::start_member_run(self.base, r)
            }
            fn dispatch_member_task(
                &self,
                r: services::MemberTaskDispatch<'_>,
            ) -> Result<services::MemberDispatchOutcome, MachineError> {
                services::RunSettlementPort::dispatch_member_task(self.base, r)
            }
            fn dispatch_managed_task(
                &self,
                r: services::ManagedTaskDispatch<'_>,
            ) -> Result<services::MemberDispatchOutcome, MachineError> {
                services::RunSettlementPort::dispatch_managed_task(self.base, r)
            }
            fn settle_member_task(
                &self,
                r: services::MemberTaskSettlement<'_>,
            ) -> Result<services::MemberSettlement, MachineError> {
                services::RunSettlementPort::settle_member_task(self.base, r)
            }
            fn member_writer_observation(
                &self,
                r: services::MemberCleanupObservation<'_>,
            ) -> Result<services::MemberWriterObservation, MachineError> {
                services::RunSettlementPort::member_writer_observation(self.base, r)
            }
            fn cancel_member_task(
                &self,
                r: services::MemberTaskSettlement<'_>,
            ) -> Result<(), MachineError> {
                let db = crate::engagement::EngagementStore::open_read_only(
                    &crate::engagement::EngagementStore::workspace_database_path(r.state_root),
                )
                .unwrap();
                let count: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM metadata WHERE key LIKE 'successor-state:cancel_%'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 1, "interrupt preceded durable owner");
                let (key, text): (String, String) = db
                    .connection()
                    .query_row(
                        "SELECT key,value FROM metadata WHERE key LIKE 'successor-state:cancel_%'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                let record: Value = serde_json::from_str(&text).unwrap();
                *self.original.borrow_mut() = Some(record["operation_ref"].clone());
                self.cancelled.set(self.cancelled.get() + 1);
                if self.fault != 0 {
                    drop(db);
                    let mutation = rusqlite::Connection::open(
                        crate::engagement::EngagementStore::workspace_database_path(r.state_root),
                    )
                    .unwrap();
                    if self.fault == 1 {
                        mutation.execute("UPDATE metadata SET value='corrupted after interrupt' WHERE key=?1",[key]).unwrap();
                        return Ok(());
                    }
                    mutation
                        .execute("DELETE FROM metadata WHERE key=?1", [key])
                        .unwrap();
                }
                Err(MachineError::new(
                    "TRANSPORT_FAILURE",
                    "lost interrupt response",
                    false,
                    json!({}),
                ))
            }
            fn settle_member_release(
                &self,
                r: services::MemberReleaseRequest<'_>,
            ) -> Result<services::MemberReleaseOutcome, services::MemberReleaseFailure>
            {
                let db = crate::engagement::EngagementStore::open_read_only(
                    &crate::engagement::EngagementStore::workspace_database_path(r.state_root),
                )
                .unwrap();
                let state: String = db
                    .connection()
                    .query_row(
                        "SELECT state FROM engagements WHERE engagement_id=?1",
                        [r.owner.scope_ref().to_string()],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(state, "closing", "release preceded durable close");
                self.closed.set(self.closed.get() + 1);
                Err(services::MemberReleaseFailure::BeforeAdmission(
                    MachineError::new(
                        "CAPABILITY_UNSUPPORTED",
                        "explicit fixture release refusal",
                        false,
                        json!({}),
                    ),
                ))
            }
        }
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Codex, true);
            let owner = scene.f.owner();
            let selected =
                scene.selection("state-task", Some(("head", None)), "structured_review_v4");
            let expected = prepare_assignment(&owner, selected.clone()).unwrap();
            let (observed, _) = scene.assign(&selected, expected).unwrap();
            let task = Uuid::parse_str(&observed.task_ref).unwrap();
            let root = scene.aggregate.scope_ref();
            let context = services::ManagedCallContext {
                session_id: root,
                source_run_id: root,
                source_turn_id: "root-turn",
                source_tool_call_id: "root-call",
                idempotency_key: "unused-for-S",
            };
            let context = managed.then_some(&context);
            let port = Probe {
                base: &scene.port,
                cancelled: std::cell::Cell::new(0),
                closed: std::cell::Cell::new(0),
                fault: 0,
                original: std::cell::RefCell::new(None),
            };
            let member =
                services::transport_member(&scope(&owner), scene.aggregate, scene.member, context)
                    .unwrap();
            let before = scene.effects();
            for revision in [member.revision + 1, u64::MAX] {
                assert!(
                    services::cancel_specialist_task(
                        &scope(&owner),
                        scene.aggregate,
                        context,
                        &port,
                        scene.member,
                        task,
                        revision,
                        "stop original"
                    )
                    .is_err()
                );
                assert_eq!(port.cancelled.get(), 0);
            }
            assert!(
                services::cancel_specialist_task(
                    &scope(&owner),
                    scene.aggregate,
                    context,
                    &port,
                    scene.member,
                    Uuid::now_v7(),
                    member.revision,
                    "stop original"
                )
                .is_err()
            );
            let uncertain = services::cancel_specialist_task(
                &scope(&owner),
                scene.aggregate,
                context,
                &port,
                scene.member,
                task,
                member.revision,
                "stop original",
            )
            .unwrap_err();
            assert_eq!(uncertain.details["classification"], "Unknown");
            let operation = uncertain.details["operation_ref"].clone();
            assert!(operation.is_string());
            assert_eq!(port.cancelled.get(), 1);
            let repeat = services::cancel_specialist_task(
                &scope(&owner),
                scene.aggregate,
                context,
                &port,
                scene.member,
                task,
                member.revision,
                "stop original",
            )
            .unwrap();
            assert_eq!(repeat["operation_ref"], operation);
            assert_eq!(repeat["task"]["task_ref"], observed.task_ref);
            assert_eq!(port.cancelled.get(), 1);
            assert_eq!(scene.effects(), before);
            let database =
                crate::engagement::EngagementStore::workspace_database_path(&owner.state_root);
            let db = rusqlite::Connection::open(&database).unwrap();
            let (key, original): (String, String) = db
                .query_row(
                    "SELECT key,value FROM metadata WHERE key LIKE 'successor-state:cancel_%'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let body = json!({"member_ref":scene.member,"task_ref":task,"reason":"stop original"});
            for axis in 0..15 {
                let mut changed: Value = serde_json::from_str(&original).unwrap();
                match axis {
                    0 => changed["schema"] = json!("foreign"),
                    1 => changed["operation_ref"] = json!(Uuid::now_v7().simple().to_string()),
                    2 => changed["workspace"] = json!("foreign"),
                    3 => changed["scope_kind"] = json!("workspace"),
                    4 => changed["scope_ref"] = json!(Uuid::now_v7()),
                    5 => changed["target"] = json!(Uuid::now_v7()),
                    6 => changed["operation"] = json!("foreign"),
                    7 => changed["input_digest"] = json!("bad"),
                    8 => changed["input_digest"] = json!(format!("sha256:{}", "0".repeat(64))),
                    9 => changed["terminal"] = json!("false"),
                    10 => {
                        changed["unknown"] = json!(true);
                    }
                    11 => {
                        changed.as_object_mut().unwrap().remove("operation_ref");
                    }
                    _ => {}
                };
                if axis == 14 {
                    changed["operation_ref"] = json!(Uuid::now_v7());
                } else {
                    let mut payload = changed.clone();
                    payload.as_object_mut().unwrap().remove("record_digest");
                    changed["record_digest"] =
                        json!(crate::trusted_host::digest(&payload).unwrap());
                }
                let bytes = if axis == 12 {
                    format!("{{\"schema\":\"duplicate\",{}", &original[1..])
                } else if axis == 13 {
                    format!("{}{}", changed, " ".repeat(65_536))
                } else {
                    changed.to_string()
                };
                db.execute(
                    "UPDATE metadata SET value=?2 WHERE key=?1",
                    rusqlite::params![key, bytes],
                )
                .unwrap();
                let before = std::fs::read(&database).unwrap();
                let operation = if managed {
                    "cancel_specialist_task"
                } else {
                    "cancel_external_specialist_task"
                };
                assert!(
                    services::observe_state_control(
                        &scope(&owner),
                        scene.aggregate,
                        context,
                        operation,
                        Some(scene.member),
                        Some(task),
                        Some(&body)
                    )
                    .is_err(),
                    "corruption axis {axis}"
                );
                assert_eq!(
                    std::fs::read(&database).unwrap(),
                    before,
                    "getter repaired state"
                );
                assert_eq!(port.cancelled.get(), 1);
            }
            let saved: Value = serde_json::from_str(&original).unwrap();
            let canonical_ref = saved["operation_ref"].as_str().unwrap();
            let uuid = Uuid::parse_str(canonical_ref).unwrap();
            for encoded in [
                uuid.simple().to_string(),
                canonical_ref.to_uppercase(),
                uuid.braced().to_string(),
                uuid.urn().to_string(),
            ] {
                if encoded == canonical_ref {
                    continue;
                }
                let mut changed = saved.clone();
                changed["operation_ref"] = json!(encoded);
                // Keep the original canonical digest so lossy UUID conversion
                // cannot hide equivalent noncanonical stored proof bytes.
                db.execute(
                    "UPDATE metadata SET value=?2 WHERE key=?1",
                    rusqlite::params![key, changed.to_string()],
                )
                .unwrap();
                let before = std::fs::read(&database).unwrap();
                assert!(
                    services::observe_state_control(
                        &scope(&owner),
                        scene.aggregate,
                        context,
                        if managed {
                            "cancel_specialist_task"
                        } else {
                            "cancel_external_specialist_task"
                        },
                        Some(scene.member),
                        Some(task),
                        Some(&body)
                    )
                    .is_err(),
                    "noncanonical original operation proof {encoded}"
                );
                assert_eq!(std::fs::read(&database).unwrap(), before);
                assert_eq!(port.cancelled.get(), 1);
            }
            db.execute(
                "UPDATE metadata SET value=?2 WHERE key=?1",
                rusqlite::params![key, original],
            )
            .unwrap();
            drop(db);

            assert!(
                services::cancel_specialist_task(
                    &scope(&owner),
                    scene.aggregate,
                    context,
                    &port,
                    scene.member,
                    task,
                    member.revision,
                    "changed reason"
                )
                .is_err()
            );
            assert_eq!(port.cancelled.get(), 1);
            if !managed {
                let engagement = services::get_external_engagement(&scope(&owner), root).unwrap();
                let close = services::close_external_engagement(
                    &scope(&owner),
                    &port,
                    root,
                    engagement.revision,
                    true,
                )
                .unwrap_err();
                assert_eq!(close.details["classification"], "Unknown");
                assert_eq!(port.closed.get(), 1);
                let current = services::get_external_engagement(&scope(&owner), root).unwrap();
                assert_eq!(current.state, "closing");
                let repeat = services::observe_state_control(
                    &scope(&owner),
                    scene.aggregate,
                    None,
                    "close_external_engagement",
                    None,
                    None,
                    Some(&json!({"allow_interrupt":true})),
                )
                .unwrap();
                assert_eq!(repeat["operation_ref"], close.details["operation_ref"]);
                assert_eq!(port.closed.get(), 1);
                let repeated = services::close_external_engagement(
                    &scope(&owner),
                    &port,
                    root,
                    engagement.revision,
                    true,
                )
                .unwrap();
                assert_eq!(repeated["operation_ref"], close.details["operation_ref"]);
                assert_eq!(repeated["engagement"]["state"], "closing");
                assert_eq!(
                    port.closed.get(),
                    1,
                    "normal close repeated an ambiguous release"
                );
                assert!(
                    services::close_external_engagement(
                        &scope(&owner),
                        &port,
                        root,
                        engagement.revision,
                        false
                    )
                    .is_err()
                );
                assert_eq!(port.closed.get(), 1);
            }

            for fault in [1, 2, 3] {
                let fault_scene = Scene::new(managed, HostRoute::Codex, true);
                let fault_owner = fault_scene.f.owner();
                let selected = fault_scene.selection(
                    "post-effect-loss",
                    Some(("head", None)),
                    "structured_review_v4",
                );
                let expected = prepare_assignment(&fault_owner, selected.clone()).unwrap();
                let (observation, _) = fault_scene.assign(&selected, expected).unwrap();
                let task = Uuid::parse_str(&observation.task_ref).unwrap();
                let root = fault_scene.aggregate.scope_ref();
                let source = services::ManagedCallContext {
                    session_id: root,
                    source_run_id: root,
                    source_turn_id: "root-turn",
                    source_tool_call_id: "root-call",
                    idempotency_key: "not-a-state-key",
                };
                let source = managed.then_some(&source);
                let port = Probe {
                    base: &fault_scene.port,
                    cancelled: std::cell::Cell::new(0),
                    closed: std::cell::Cell::new(0),
                    fault,
                    original: std::cell::RefCell::new(None),
                };
                let revision = services::transport_member(
                    &scope(&fault_owner),
                    fault_scene.aggregate,
                    fault_scene.member,
                    source,
                )
                .unwrap()
                .revision;
                if fault == 3 {
                    let db = rusqlite::Connection::open(
                        crate::engagement::EngagementStore::workspace_database_path(
                            &fault_owner.state_root,
                        ),
                    )
                    .unwrap();
                    db.execute_batch("CREATE TABLE specialist_commit_failure(owner TEXT REFERENCES engagements(engagement_id) DEFERRABLE INITIALLY DEFERRED) STRICT; CREATE TRIGGER specialist_deferred AFTER INSERT ON metadata WHEN NEW.key LIKE 'successor-state:%' BEGIN INSERT INTO specialist_commit_failure(owner) VALUES('absent-engagement'); END;").unwrap();
                }
                let error = services::cancel_specialist_task(
                    &scope(&fault_owner),
                    fault_scene.aggregate,
                    source,
                    &port,
                    fault_scene.member,
                    task,
                    revision,
                    "stop original",
                )
                .unwrap_err();
                assert_eq!(port.cancelled.get(), if fault == 3 { 0 } else { 1 });
                assert_eq!(error.details["classification"], "Unknown");
                assert_eq!(error.details["effect"], "unresolved");
                if fault == 3 {
                    assert!(
                        Uuid::parse_str(error.details["operation_ref"].as_str().unwrap()).is_ok()
                    );
                    let db = rusqlite::Connection::open(
                        crate::engagement::EngagementStore::workspace_database_path(
                            &fault_owner.state_root,
                        ),
                    )
                    .unwrap();
                    let owners: i64 = db
                        .query_row(
                            "SELECT COUNT(*) FROM metadata WHERE key LIKE 'successor-state:%'",
                            [],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(owners, 0, "failed admission commit left an owner");
                    if !managed {
                        let view =
                            services::get_external_engagement(&scope(&fault_owner), root).unwrap();
                        let failed = services::close_external_engagement(
                            &scope(&fault_owner),
                            &port,
                            root,
                            view.revision,
                            true,
                        )
                        .unwrap_err();
                        assert_eq!(failed.details["effect"], "unresolved");
                        assert_eq!(failed.details["classification"], "Unknown");
                        assert_eq!(port.closed.get(), 0);
                        assert_eq!(
                            services::get_external_engagement(&scope(&fault_owner), root).unwrap(),
                            view
                        );
                    }
                } else {
                    assert_eq!(
                        Some(error.details["operation_ref"].clone()),
                        *port.original.borrow(),
                        "post-effect read loss replaced the original operation reference"
                    );
                }
            }
        }
    }

    #[test]
    fn facade_reads_and_state_repeats_follow_actual_product_owners() {
        use crate::specialist_facade as facade;
        for managed in [false, true] {
            for route in [HostRoute::Codex, HostRoute::Claude] {
                let scene = Scene::new(managed, route, true);
                let owner = scene.f.owner();
                let root = scene.aggregate.scope_ref();
                let selected =
                    scene.selection("facade-RS", Some(("head", None)), "structured_review_v4");
                let expected = prepare_assignment(&owner, selected.clone()).unwrap();
                let (task, _) = scene.assign(&selected, expected).unwrap();
                let context = services::ManagedCallContext {
                    session_id: root,
                    source_run_id: root,
                    source_turn_id: "real-turn",
                    source_tool_call_id: "real-call",
                    idempotency_key: "unused-read-state",
                };
                let deliver = |request: &Value, call: &str| {
                    if managed {
                        return facade::finish(
                            request["operation"].as_str().unwrap(),
                            facade::dispatch(
                                &scene.f.home,
                                &owner,
                                &scope(&owner),
                                Some(&context),
                                &scene.port,
                                request,
                                None,
                            ),
                            true,
                        );
                    }
                    let (_, cap) = {
                        let store = scene.f.ready();
                        store
                            .bind_native(
                                crate::host_invocation::NativeEvent {
                                    session: "facade-RS".into(),
                                    turn: Some("turn".into()),
                                    tool_use: call.into(),
                                    server: "dolgorae".into(),
                                    server_source: (route == HostRoute::Claude)
                                        .then(|| "dynamic".into()),
                                    tool: "specialist".into(),
                                },
                                request.clone(),
                            )
                            .unwrap()
                    };
                    facade::external_with_port(
                        &scene.f.home,
                        &scene.f.path(),
                        "specialist",
                        if route == HostRoute::Codex {
                            facade::NativeCarrier::Codex {
                                session: "facade-RS",
                                call,
                            }
                        } else {
                            facade::NativeCarrier::Claude(&cap)
                        },
                        request,
                        &scene.port,
                    )
                };
                let member = services::transport_member(
                    &scope(&owner),
                    scene.aggregate,
                    scene.member,
                    managed.then_some(&context),
                )
                .unwrap();
                for prefix in ["await", "collect"] {
                    let operation = format!(
                        "{prefix}_{}specialist_task",
                        if managed { "" } else { "external_" }
                    );
                    let mut request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":operation,"scope_kind":scene.aggregate.scope_kind(),"scope_ref":root,"member_ref":scene.member,"task_ref":task.task_ref});
                    if prefix == "await" {
                        request["timeout_seconds"] = json!(0);
                    }
                    let before = scene.effects();
                    for _ in 0..2 {
                        let reply = deliver(&request, prefix);
                        assert_eq!(
                            reply["schema"], "dolgorae.external-specialist-response/v5",
                            "{reply}"
                        );
                        assert_eq!(reply["task"]["task_ref"], task.task_ref);
                        assert!(facade::validates(&reply, &format!("{operation}_response")));
                    }
                    assert_eq!(scene.effects(), before);
                }
                let operation = if managed {
                    "cancel_specialist_task"
                } else {
                    "cancel_external_specialist_task"
                };
                let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":operation,"scope_kind":scene.aggregate.scope_kind(),"scope_ref":root,"member_ref":scene.member,"task_ref":task.task_ref,"expected_revision":member.revision,"reason":"original request"});
                let first = deliver(&request, "cancel");
                assert_eq!(first["error"]["classification"], "Unknown", "{first}");
                let original_ref = first["error"]["operation_ref"].clone();
                assert!(!original_ref.is_null());
                for call in ["cancel", "cancel-new-native"] {
                    let reply = deliver(&request, call);
                    assert_eq!(reply["operation_ref"], original_ref, "{reply}");
                    assert_eq!(reply["task"]["task_ref"], task.task_ref);
                    assert!(facade::validates(&reply, &format!("{operation}_response")));
                }
                if !managed {
                    assert_eq!(
                        facade::lookup_native(
                            &scene.f.home,
                            &scene.f.path(),
                            "facade-RS",
                            "cancel"
                        )
                        .unwrap()["operation_ref"],
                        original_ref
                    );
                    let get = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"get_external_engagement","engagement_ref":root});
                    let initial = deliver(&get, "fresh-get");
                    assert_eq!(initial["engagement"]["state"], "open");
                    let close = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"close_external_engagement","engagement_ref":root,"expected_revision":initial["engagement"]["revision"],"allow_interrupt":true});
                    let lost = deliver(&close, "close");
                    assert_eq!(lost["error"]["classification"], "Unknown", "{lost}");
                    let repeated = deliver(&close, "close");
                    assert_eq!(
                        repeated["operation_ref"], lost["error"]["operation_ref"],
                        "{repeated}"
                    );
                    assert_eq!(repeated["engagement"]["state"], "closing");
                    let fresh = deliver(&get, "fresh-get");
                    assert_eq!(
                        fresh["engagement"]["state"], "closing",
                        "R delivery reused a stale observation"
                    );
                }
            }
        }
    }

    #[test]
    fn close_successive_members_refreshes_real_release_revision_and_observes_prior_owners() {
        use crate::conformance::{
            BootstrapRecordKind, BootstrapRequest, ConformantLedger, IdempotencyIntent,
            IdempotencyOperation,
        };
        for (count, prior) in [
            (2, None),
            (3, None),
            (3, Some((0, "terminal"))),
            (3, Some((1, "terminal"))),
            (3, Some((2, "terminal"))),
            (3, Some((0, "requested"))),
            (3, Some((1, "requested"))),
            (3, Some((2, "requested"))),
            (3, Some((1, "cleanup_failed"))),
        ] {
            let scene = Scene::new(false, HostRoute::Codex, false);
            let owner = scene.f.owner();
            let root = scene.aggregate.scope_ref();
            let member_view =
                services::transport_member(&scope(&owner), scene.aggregate, scene.member, None)
                    .unwrap();
            let db = crate::engagement::EngagementStore::open_read_only(
                &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
            )
            .unwrap();
            let provision:String=db.connection().query_row("SELECT json_extract(receipt_json,'$.accepted_subjects.provisioning_ref') FROM delegation_operations WHERE operation_kind='install_provisioning' LIMIT 1",[],|r|r.get(0)).unwrap();
            drop(db);
            let snapshot =
                crate::provisioning::retained_snapshot(&owner.state_root, &provision).unwrap();
            let mut members = vec![scene.member];
            for index in 1..count {
                let hired = services::hire_external_specialist(
                    &scope(&owner),
                    &scene.port,
                    services::HireExternalSpecialist {
                        engagement_id: root,
                        provisioning_ref: &provision,
                        role_ref: "reviewer",
                        agent_configuration_digest: &member_view.agent_configuration_digest,
                        idempotency_key: &format!("close-member-{index}"),
                        expected_revision: None,
                    },
                )
                .unwrap();
                let member = Uuid::parse_str(&hired.member.member_ref).unwrap();
                let mut manifest = services::tests::member_manifest(
                    &owner.view,
                    &owner.controller,
                    member,
                    root,
                    Uuid::parse_str(&hired.receipt.operation_ref).unwrap(),
                    &snapshot,
                );
                manifest.aggregate_binding.as_mut().unwrap().aggregate_kind =
                    crate::domain::AggregateKind::ExternalSpecialistEngagement;
                manifest.parent_ref.as_mut().unwrap().namespace =
                    "dolgorae.external-specialist-engagement.v1".into();
                crate::run::RunStore::new(
                    crate::workspace::SystemWorkspacePlatform,
                    &owner.state_root,
                )
                .publish(&manifest)
                .unwrap();
                members.push(member);
            }
            members.sort();
            crate::writer::WriterStore::initialize_layout(
                &owner.state_root,
                &owner.view.workspace_id,
                crate::darwin::DarwinSystem.current_uid(),
            )
            .unwrap();
            for member in &members {
                let run_root = crate::run::run_root(&owner.state_root, *member);
                let mut ledger = ConformantLedger::open_for_bootstrap(&run_root, *member).unwrap();
                ledger
                    .bootstrap(&BootstrapRequest {
                        timestamp: "2026-10-22T00:00:00.000000Z".into(),
                        workspace_id: owner.view.workspace_id.clone(),
                        intent: IdempotencyIntent {
                            schema_version: 1,
                            operation: IdempotencyOperation::StartRun,
                            idempotency_key: format!("close-fixture-{member}"),
                            normalized_identity_sha256: "2".repeat(64),
                            run_id: *member,
                        },
                        record_kind: BootstrapRecordKind::RunCreated,
                        initial_access: "unknown".into(),
                        default_effort: Some("low".into()),
                    })
                    .unwrap();
                ledger
                    .mark_threadless_ready("2026-10-22T00:00:01.000000Z")
                    .unwrap();
                let projection = ledger.inner().projection().unwrap();
                assert_eq!(projection.lifecycle, crate::domain::RunLifecycle::Idle);
                private(
                    &run_root.join("state.json"),
                    &canonical(&projection).unwrap(),
                );
                let manifest = crate::run::RunStore::new(
                    crate::workspace::SystemWorkspacePlatform,
                    &owner.state_root,
                )
                .load_manifest(*member)
                .unwrap();
                let binding = manifest.global_profile_binding.unwrap();
                crate::global_runtime::GlobalMembershipStore::from_root(
                    scene.f.home.root(),
                    &binding.selected_name,
                    &binding.server_key,
                )
                .unwrap()
                .record(
                    &owner.view.workspace_id,
                    *member,
                    crate::global_runtime::MembershipDisposition::Active,
                )
                .unwrap();
            }
            let carrier =
                crate::controller::CredentialCarrier::open_path(&scene.f.controller).unwrap();
            let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
            let stale =
                services::transport_member(&scope(&owner), scene.aggregate, members[0], None)
                    .unwrap()
                    .revision
                    - 1;
            let refused = services::release_external_specialist(
                &scope(&owner),
                &port,
                services::ReleaseExternalSpecialist {
                    engagement_id: root,
                    member_ref: members[0],
                    expected_revision: stale,
                    allow_interrupt: false,
                },
            )
            .unwrap_err();
            assert_eq!(refused.code, "REVISION_CONFLICT");
            assert_eq!(
                crate::semantic::release_probe::release_row_count(&owner.state_root),
                0,
                "stale fresh admission created a release owner"
            );
            if let Some((index, progress)) = prior {
                let member = members[index];
                let current =
                    services::transport_member(&scope(&owner), scene.aggregate, member, None)
                        .unwrap()
                        .revision;
                if progress == "terminal" {
                    let released = services::release_external_specialist(
                        &scope(&owner),
                        &port,
                        services::ReleaseExternalSpecialist {
                            engagement_id: root,
                            member_ref: member,
                            expected_revision: current,
                            allow_interrupt: false,
                        },
                    )
                    .unwrap();
                    assert_eq!(released.state, "released");
                } else {
                    let mut db = crate::engagement::EngagementStore::open(
                        &crate::engagement::EngagementStore::workspace_database_path(
                            &owner.state_root,
                        ),
                    )
                    .unwrap();
                    db.transact(|tx| {
                        let operation = Uuid::now_v7();
                        crate::engagement::admit_member_release(
                            tx,
                            crate::engagement::MemberReleaseAdmission {
                                operation_ref: operation,
                                workspace_ref: &owner.view.workspace_id,
                                scope_kind: "external_engagement",
                                scope_ref: &root.to_string(),
                                member_ref: member,
                                run_ref: member,
                                controller_principal: &format!(
                                    "controller:{}",
                                    owner.controller.identity.controller_id
                                ),
                                controller_generation: owner.controller.identity.generation,
                                admitted_membership_revision: current,
                                now_ms: 1,
                            },
                        )?;
                        crate::engagement::mark_external_member_releasing(tx, root, member, 1)?;
                        if progress == "cleanup_failed" {
                            crate::engagement::record_member_release_progress(
                                tx,
                                operation,
                                crate::engagement::MemberReleaseProgress::Failed {
                                    progress: "cleanup_failed",
                                    safe_failure_code: "RECOVERY_NEEDED",
                                    required_action: "recover_run",
                                },
                                2,
                            )?;
                        }
                        Ok(())
                    })
                    .unwrap();
                }
            }
            let prior_row = prior.map(|(index, _)| {
                let db = crate::engagement::EngagementStore::open_read_only(
                    &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
                )
                .unwrap();
                canonical(
                    &crate::engagement::member_release_operation(
                        db.connection(),
                        &owner.view.workspace_id,
                        "external_engagement",
                        &root.to_string(),
                        &members[index].to_string(),
                    )
                    .unwrap()
                    .unwrap(),
                )
                .unwrap()
            });
            let observed = services::get_external_engagement(&scope(&owner), root).unwrap();
            let initial_membership = observed.members[0].revision;
            let closed = services::close_external_engagement(
                &scope(&owner),
                &port,
                root,
                observed.revision,
                false,
            )
            .unwrap();
            let pending = prior.is_some_and(|(_, progress)| progress != "terminal");
            assert_eq!(
                closed["engagement"]["state"],
                if pending { "closing" } else { "closed" },
                "{count}/{prior:?}: {closed}"
            );
            let after = services::get_external_engagement(&scope(&owner), root).unwrap();
            let expected_fresh = count - usize::from(prior.is_some());
            assert_eq!(
                after.members[0].revision,
                initial_membership + expected_fresh as u64,
                "each real terminal release advances the shared counter once"
            );
            for (index, member) in members.iter().enumerate() {
                let is_pending = prior.is_some_and(|(prior_index, progress)| {
                    prior_index == index && progress != "terminal"
                });
                let db = crate::engagement::EngagementStore::open_read_only(
                    &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
                )
                .unwrap();
                let row = crate::engagement::member_release_operation(
                    db.connection(),
                    &owner.view.workspace_id,
                    "external_engagement",
                    &root.to_string(),
                    &member.to_string(),
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    row.progress,
                    if is_pending {
                        prior.unwrap().1
                    } else {
                        "terminal"
                    }
                );
                if prior.is_some_and(|(prior_index, _)| prior_index == index) {
                    assert_eq!(
                        canonical(&row).unwrap(),
                        *prior_row.as_ref().unwrap(),
                        "close rewrote a prior release owner"
                    );
                }
                let state = crate::run::RunStore::new(
                    crate::workspace::SystemWorkspacePlatform,
                    &owner.state_root,
                )
                .load_state_projection(*member)
                .unwrap();
                assert_eq!(
                    state.lifecycle,
                    if is_pending {
                        crate::domain::RunLifecycle::Idle
                    } else {
                        crate::domain::RunLifecycle::Closed
                    },
                    "close resumed a retained release"
                );
            }
            let before = scene.effects();
            let members_before = canonical(&after).unwrap();
            let releases_before = crate::semantic::release_probe::release_rows(&owner.state_root);
            let repeated = services::close_external_engagement(
                &scope(&owner),
                &port,
                root,
                observed.revision,
                false,
            )
            .unwrap();
            assert_eq!(repeated["operation_ref"], closed["operation_ref"]);
            assert_eq!(
                scene.effects(),
                before,
                "ordinary close repeat mutated task or K rows"
            );
            assert_eq!(
                canonical(&services::get_external_engagement(&scope(&owner), root).unwrap())
                    .unwrap(),
                members_before,
                "ordinary close repeat mutated membership"
            );
            assert_eq!(
                crate::semantic::release_probe::release_rows(&owner.state_root),
                releases_before,
                "ordinary close repeat mutated release owners"
            );
        }
    }

    #[test]
    fn state_release_getter_projects_the_original_release_owner_without_settlement() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Codex, false);
            let owner = scene.f.owner();
            let root = scene.aggregate.scope_ref();
            let context = services::ManagedCallContext {
                session_id: root,
                source_run_id: root,
                source_turn_id: "turn",
                source_tool_call_id: "call",
                idempotency_key: "unused-S",
            };
            let context = managed.then_some(&context);
            let operation = if managed {
                "release_specialist"
            } else {
                "release_external_specialist"
            };
            let op_ref = Uuid::now_v7();
            let mut db = crate::engagement::EngagementStore::open(
                &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
            )
            .unwrap();
            db.transact(|tx| {
                crate::engagement::admit_member_release(
                    tx,
                    crate::engagement::MemberReleaseAdmission {
                        operation_ref: op_ref,
                        workspace_ref: &owner.view.workspace_id,
                        scope_kind: scene.aggregate.scope_kind(),
                        scope_ref: &root.to_string(),
                        member_ref: scene.member,
                        run_ref: scene.member,
                        controller_principal: &crate::operation::Namespace::delegation(
                            &owner.controller.identity,
                            &owner.view.workspace_id,
                            Some((scene.aggregate.scope_kind(), &root.to_string())),
                            "assign_specialist_task",
                            "release-fixture",
                        )
                        .unwrap()
                        .controller_principal,
                        controller_generation: owner.controller.identity.generation,
                        admitted_membership_revision: 1,
                        now_ms: 1,
                    },
                )
            })
            .unwrap();
            for (progress, state) in [
                (None, "releasing"),
                (Some("cleanup_failed"), "cleanup_failed"),
                (Some("recovery_needed"), "unknown"),
            ] {
                if let Some(progress) = progress {
                    db.transact(|tx| {
                        crate::engagement::record_member_release_progress(
                            tx,
                            op_ref,
                            crate::engagement::MemberReleaseProgress::Failed {
                                progress,
                                safe_failure_code: "RECOVERY_NEEDED",
                                required_action: "recover_run",
                            },
                            2,
                        )
                    })
                    .unwrap();
                }
                let before = scene.effects();
                let response = services::observe_state_control(
                    &scope(&owner),
                    scene.aggregate,
                    context,
                    operation,
                    Some(scene.member),
                    None,
                    None,
                )
                .unwrap();
                assert_eq!(response["operation_ref"], op_ref.to_string());
                assert_eq!(response["member"]["state"], state);
                assert!(
                    crate::specialist_facade::validates(
                        &response,
                        &format!("{operation}_response")
                    ),
                    "{response}"
                );
                assert_eq!(scene.effects(), before);
            }
        }
    }

    #[test]
    fn engagement_observation_bound_never_truncates_or_admits_unrepresentable_close() {
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let member =
            services::transport_member(&scope(&owner), scene.aggregate, scene.member, None)
                .unwrap();
        let store = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
        )
        .unwrap();
        let provision:String=store.connection().query_row("SELECT json_extract(receipt_json,'$.accepted_subjects.provisioning_ref') FROM delegation_operations WHERE operation_kind='install_provisioning' LIMIT 1",[],|r|r.get(0)).unwrap();
        drop(store);
        for index in 1..65 {
            services::hire_external_specialist(
                &scope(&owner),
                &scene.port,
                services::HireExternalSpecialist {
                    engagement_id: scene.aggregate.scope_ref(),
                    provisioning_ref: &provision,
                    role_ref: "reviewer",
                    agent_configuration_digest: &member.agent_configuration_digest,
                    idempotency_key: &format!("bounded-member-{index}"),
                    expected_revision: None,
                },
            )
            .unwrap();
            if index == 63 {
                let visible =
                    services::get_external_engagement(&scope(&owner), scene.aggregate.scope_ref())
                        .unwrap();
                assert_eq!(visible.members.len(), 64);
                assert!(visible.next_cursor.is_none());
            }
        }
        let error = services::get_external_engagement(&scope(&owner), scene.aggregate.scope_ref())
            .unwrap_err();
        assert_eq!(error.code, "CAPABILITY_UNSUPPORTED");
        assert_eq!(error.details["effect"], "none");
        let before = scene.effects();
        let calls = scene.port.calls();
        let error = services::close_external_engagement(
            &scope(&owner),
            &scene.port,
            scene.aggregate.scope_ref(),
            0,
            true,
        )
        .unwrap_err();
        assert_eq!(error.details["effect"], "none");
        assert_eq!(scene.effects(), before);
        assert_eq!(scene.port.calls(), calls);
        let store = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
        )
        .unwrap();
        let(state,owners):(String,i64)=store.connection().query_row("SELECT state,(SELECT COUNT(*) FROM metadata WHERE key LIKE 'successor-state:close_%') FROM engagements WHERE engagement_id=?1",[scene.aggregate.scope_ref().to_string()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(state, "active");
        assert_eq!(owners, 0);
    }

    #[test]
    fn source_binding_preparation_is_not_acceptance_and_provenance_is_immutable() {
        for managed in [false, true] {
            for route in [HostRoute::Codex, HostRoute::Claude] {
                let scene = Scene::new(managed, route, false);
                let owner = scene.f.owner();
                let before = scene.effects();
                let calls = scene.port.calls();
                let first =
                    prepare_provisioning(&scene.f.home, &owner, scene.selected.clone()).unwrap();
                assert_eq!(
                    prepare_provisioning(&scene.f.home, &owner, scene.selected.clone()).unwrap(),
                    first
                );
                let mut other = scene.selected.clone();
                other.role = SelectedInstructionSource::Inline(
                    scene.selected.capture(&scene.f.home).unwrap().role.body,
                );
                assert_eq!(
                    other
                        .capture(&scene.f.home)
                        .unwrap()
                        .sources_identity()
                        .unwrap(),
                    first
                );
                assert_eq!(
                    prepare_provisioning(&scene.f.home, &owner, other)
                        .unwrap_err()
                        .code,
                    "IDEMPOTENCY_CONFLICT"
                );
                let selection = scene.selection(
                    "preparation-only",
                    Some(("workspace", None)),
                    "structured_review_v4",
                );
                let identity = prepare_assignment(&owner, selection.clone()).unwrap();
                assert_eq!(
                    prepare_assignment(&owner, selection.clone()).unwrap(),
                    identity
                );
                let mut changed = selection.clone();
                changed.task.brief.push('x');
                assert_eq!(
                    prepare_assignment(&owner, changed).unwrap_err().code,
                    "IDEMPOTENCY_CONFLICT"
                );
                assert_eq!(
                    project(
                        &owner,
                        ProjectionLookup::Assignment {
                            namespace: selection.namespace
                        }
                    )
                    .unwrap(),
                    serde_json::to_value(identity).unwrap()
                );
                assert_eq!(
                    scene.effects(),
                    before,
                    "preparation must not create product rows"
                );
                assert_eq!(scene.port.calls(), calls);
            }
        }
    }

    #[test]
    fn source_binding_both_owners_capture_once_and_replay_without_registration_or_live_sources() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Codex, true);
            let owner = scene.f.owner();
            let selection =
                scene.selection("checked", Some(("head", None)), "structured_review_v4");
            let identity = prepare_assignment(&owner, selection.clone()).unwrap();
            let before = scene.effects();
            let (_, receipt) = scene.assign(&selection, identity.clone()).unwrap();
            let after = scene.effects();
            assert_eq!(after[0], before[0] + 1);
            assert_eq!(after[3], before[3] + 1);
            let calls = scene.port.calls();
            let bytes = canonical(&receipt).unwrap();
            fs::remove_dir_all(owner.root.path.join(DIRECTORY)).unwrap();
            fs::remove_dir_all(scene.f.home.root().join("roles")).unwrap();
            fs::write(scene.f.workspace.join("file"), b"moved\n").unwrap();
            scene.git(&["add", "file"]);
            scene.git(&["commit", "-q", "-m", "move"]);
            assert_eq!(
                canonical(&scene.assign(&selection, identity.clone()).unwrap().1).unwrap(),
                bytes
            );
            assert_eq!(scene.port.calls(), calls, "replay must not dispatch");
            assert_eq!(scene.effects(), after);
            for field in [
                "schema",
                "purpose",
                "task_body_schema",
                "expected_output_schema",
                "execution_intent",
                "admitted_model",
                "admitted_effort",
                "role_snapshot_digest",
                "agent_configuration_digest",
            ] {
                let mut value = serde_json::to_value(&identity).unwrap();
                value[field] = json!(format!("changed-{field}"));
                let changed = serde_json::from_value(value).unwrap();
                assert_eq!(
                    scene.assign(&selection, changed).unwrap_err().code,
                    "IDEMPOTENCY_CONFLICT",
                    "{field}"
                );
            }
            for field in [
                "task-length",
                "task-digest",
                "images",
                "context-count",
                "context-kind",
            ] {
                let mut changed = identity.clone();
                match field {
                    "task-length" => changed.task_body.byte_length += 1,
                    "task-digest" => {
                        changed.task_body.sha256 = format!("sha256:{}", "e".repeat(64))
                    }
                    "images" => changed.images.push(crate::delegation_identity::ImageRef {
                        content: crate::delegation_identity::BytesRef::from_bytes(b"image"),
                        media_type: "image/png".into(),
                        detail: "auto".into(),
                    }),
                    "context-count" => {
                        changed.contexts.pop();
                    }
                    _ => changed.contexts[0].kind = "artifact".into(),
                }
                assert_eq!(
                    scene.assign(&selection, changed).unwrap_err().code,
                    "IDEMPOTENCY_CONFLICT",
                    "{field}"
                );
            }
            for path in ["identity", "media_type", "length", "digest", "order"] {
                let mut changed = identity.clone();
                let index = changed.contexts.len() - 1;
                match path {
                    "identity" => changed.contexts[index].identity = "foreign".into(),
                    "media_type" => changed.contexts[index].media_type = "text/plain".into(),
                    "length" => changed.contexts[index].content.byte_length += 1,
                    "digest" => {
                        changed.contexts[index].content.sha256 =
                            format!("sha256:{}", "e".repeat(64))
                    }
                    _ => changed.contexts.swap(0, index),
                }
                assert_eq!(
                    scene.assign(&selection, changed).unwrap_err().code,
                    "IDEMPOTENCY_CONFLICT",
                    "{path}"
                );
            }
            for changed in [0, 1, 2] {
                let mut input = selection.clone();
                match changed {
                    0 => input.task.brief.push('x'),
                    1 => input.deadline_seconds += 1,
                    _ => input.member_ref = Uuid::now_v7(),
                };
                assert_eq!(
                    scene.assign(&input, identity.clone()).unwrap_err().code,
                    "IDEMPOTENCY_CONFLICT"
                );
            }
            assert_eq!(scene.effects(), after);
        }
    }

    // Selected Podway source owns these public/digest representations. The
    // process substitute below deliberately does not call bridge converters.
    fn consumer_evidence() -> Vec<(String, String, Value, Value)> {
        let digest = crate::native_consumer::hash;
        let check = json!({"operation_id":"verify","operation_digest":digest(b"operation"),"input_basis":{"descriptor":"source","digest":digest(b"input")},"executor":{"name":"checker","version":"1"},"outcome":"pass","summary":"Verified retained evidence.","output_digest":digest(b"output")});
        let artifact = json!({"location_type":"reference","location":"urn:source:artifact","sha256_digest":digest(b"artifact"),"size_bytes":8,"media_type":"text/plain"});
        let mut check_digest = check.clone();
        check_digest["kind"] = json!("check_result");
        let list = vec!["한".repeat(8192); 122];
        vec![
            (
                "confirmed".into(),
                "confirm".into(),
                json!(true),
                json!({"kind":"confirm"}),
            ),
            (
                "text".into(),
                "text".into(),
                json!("한".repeat(50000)),
                json!({"kind":"text","value":"한".repeat(50000)}),
            ),
            (
                "choice".into(),
                "choice".into(),
                json!("ready"),
                json!({"kind":"choice","value":"ready"}),
            ),
            (
                "integer".into(),
                "integer".into(),
                json!(i64::MIN),
                json!({"kind":"integer","value":i64::MIN}),
            ),
            (
                "list".into(),
                "list".into(),
                json!(list),
                json!({"kind":"list","value":list}),
            ),
            (
                "artifact".into(),
                "artifact".into(),
                artifact,
                json!({"kind":"artifact","location_kind":"external_reference","location":"urn:source:artifact","digest":digest(b"artifact"),"size_bytes":8,"media_type":"text/plain"}),
            ),
            ("check".into(), "check_result".into(), check, check_digest),
        ]
    }
    #[derive(Clone, Copy)]
    enum ConsumerResult {
        Success,
        Rejected,
        Lost,
        ForeignPlacement,
    }
    #[derive(Clone, Copy)]
    enum EvidenceFault {
        ArtifactShape,
        CheckDiscriminator,
        Unavailable,
        Digest,
        ContinuationSource,
    }
    struct ScriptedPodway<'a> {
        observation: Value,
        package: Value,
        evidence: Vec<(String, String, Value, Value)>,
        response: Option<Value>,
        input: Option<Value>,
        launches: std::rc::Rc<std::cell::Cell<usize>>,
        evidence_pages: usize,
        evidence_fault: Option<EvidenceFault>,
        result: ConsumerResult,
        hook_at: Option<&'static str>,
        hook: Option<Box<dyn FnOnce() + 'a>>,
    }
    impl ScriptedPodway<'_> {
        fn new(scene: &Scene, c: &crate::native_consumer::PodwayConnection) -> Self {
            let owner = scene.f.owner();
            let manifest = crate::run::RunStore::new(
                crate::workspace::SystemWorkspacePlatform,
                &owner.state_root,
            )
            .load_manifest(scene.member)
            .unwrap();
            let role = manifest.instructions.delegation().unwrap();
            let values = consumer_evidence();
            let digest = crate::native_consumer::hash;
            let source = Uuid::now_v7();
            let items=values.iter().map(|(id,kind,_,value)|json!({"item_id":id,"state":"present","item_revision":3,"value_digest":digest(&serde_json::to_vec(value).unwrap()),"value_type":kind,"size_bytes":serde_json::to_vec(value).unwrap().len()})).collect::<Vec<_>>();
            let source_items = values
                .iter()
                .map(|(id, _, _, value)| json!({"id":id,"value":value}))
                .collect::<Vec<_>>();
            let body = json!({"schema":"podway.delegation-work-package/v1","workspace_uuid":c.workspace,"procedure_digest":digest(b"selected Procedure"),"session_id":c.session,"placement_id":c.placement,"attempt_id":c.attempt,
                "delegation":{"binding_ref":owner.configuration.caller_scope.binding_ref,"role_ref":role.role.name,"role_snapshot_digest":role.role_snapshot_digest,"max_rounds":4,"after_accept":"retain"},
                "action":{"title":"Review","intent":"Read retained evidence.","items":[{"id":"summary","type":"text","prompt":"Summary","required":true}]},"goal":null,
                "evidence":[{"selector":{"node":"producer","required":true,"items":values.iter().map(|(id,..)|id).collect::<Vec<_>>()},"resolution":"resolved","source_attempt_id":source,"source_attempt_number":1,"source_items_digest":digest(&serde_json::to_vec(&source_items).unwrap()),"items":items}]});
            let mut domain = b"podway.delegation-work-package/v1\n".to_vec();
            domain.extend(serde_json::to_vec(&body).unwrap());
            let package = json!({"schema":"podway.delegation-package-result/v1","package":body,"package_digest":digest(&domain),"workspace_uuid":c.workspace,"session_id":c.session,"session_revision":7,"attempt_id":c.attempt});
            // Selected observation-result/v5 embeds compact status, not the
            // standard session.status result. The required summary was already
            // recorded at revision 2; Record replaces it and Accept can retain it.
            let current = json!({"node":{"graph_node_id":c.placement,"node_definition_id":"review","node_type":"action"},"attempt":{"attempt_id":c.attempt,"attempt_number":1},"readiness":{"items_satisfied":true,"unblocked":true,"goal_ready":true,"can_advance":false},"missing_required_item_count":0,"blockers_total":0});
            let reference = json!({"source_graph_node_id":"producer","source_title":"Producer","state":"resolved","source_attempt_id":source,"source_attempt_number":1,"items_digest":package["package"]["evidence"][0]["source_items_digest"]});
            let observation = json!({"schema":"podway.observation-result/v5",
                "status":{"procedure":{"schema":"podway.procedure/v2","id":"offline","version":"1","digest":digest(b"selected Procedure")},"session":{"id":c.session,"lifecycle":"running","revision":7},"current":current,"goal_tracking":false,"goal_defined":false,"trace_length":1,"counters":[],"items":[{"item_id":"summary","type":"text","required":true,"satisfied":true,"revision":2}],"items_total":1,"items_truncated":false},
                "guidance":{"schema":"podway.next-result/v5","procedure_schema":"podway.procedure/v2","procedure_digest":digest(b"selected Procedure"),"goal_tracking":false,"goal_defined":false,"node":current["node"],"attempt":current["attempt"],"trace_length":1,"counters":[],"revision":7,"readiness":current["readiness"],"missing_required_item_count":0,"blockers_total":0,"allowed_actions":["item.set","item.clear","session.retry","session.block","session.cancel","session.reset"],"suggestions":[],"suggestions_total":0,"suggestions_truncated":false,"references":[reference],"readback":[],"readback_items_total":0,"readback_items_truncated":false,"allowed_manual_rework_targets":[]},
                "active_items":[{"item_id":"summary","type":"text","prompt":"Summary","required":true,"required_now":true,"satisfied":true,"revision":2,"constraints":{"choices_total":0,"choices_truncated":false},"value":"Previously recorded summary.","value_truncated":false}],"active_items_total":1,"active_items_truncated":false,"mutation_templates":[],"mutation_templates_total":0,"mutation_templates_truncated":false});
            Self {
                observation,
                package,
                evidence: values,
                response: None,
                input: None,
                launches: Default::default(),
                evidence_pages: 0,
                evidence_fault: None,
                result: ConsumerResult::Success,
                hook_at: None,
                hook: None,
            }
        }
        fn copy_reads(&self) -> Self {
            Self {
                observation: self.observation.clone(),
                package: self.package.clone(),
                evidence: self.evidence.clone(),
                response: None,
                input: None,
                launches: self.launches.clone(),
                evidence_pages: 0,
                evidence_fault: None,
                result: ConsumerResult::Success,
                hook_at: None,
                hook: None,
            }
        }
        fn trigger(&mut self, point: &str) {
            if self.hook_at == Some(point)
                && let Some(hook) = self.hook.take()
            {
                hook();
            }
        }
        fn envelope(
            c: &crate::native_consumer::PodwayConnection,
            command: &str,
            result: Value,
        ) -> Value {
            json!({"schema":"podway.output/v4","request_id":Uuid::now_v7(),"command":command,"generated_at":"2026-10-27T00:00:00Z","workspace":{"uuid":c.workspace,"root":c.worktree,"latest_workspace_sequence":9},"result":result,"warnings":[]})
        }
        fn semantic_digest(input: &Value, command: &str) -> String {
            let payload = if command == "item.record_many" {
                json!({"operations":input["operations"]})
            } else {
                input.clone()
            };
            crate::native_consumer::hash(&serde_json::to_vec(&json!({"protocol_major":1,"command":command,"workspace_id":input["workspace_uuid"],"preconditions":{"session_id":input["session_id"],"session_revision":input["session_revision"],"attempt_id":input["attempt_id"]},"payload":payload})).unwrap())
        }
    }
    impl crate::podway_client::Client for ScriptedPodway<'_> {
        fn invoke(
            &mut self,
            c: &crate::native_consumer::PodwayConnection,
            invocation: crate::podway_client::Invocation,
            input: &[u8],
            launched: &mut dyn FnMut(u32) -> Result<(), MachineError>,
        ) -> Result<crate::podway_client::Reply, MachineError> {
            use crate::native_consumer::Mutation;
            use crate::podway_client::{Invocation, ReadCommand};
            let (command, result) = match &invocation {
                Invocation::Read(ReadCommand::Manifest) => {
                    self.trigger("manifest");
                    (
                        "version",
                        json!({"schema":"podway.version-result/v2","product":"podway","version":"v0.2.15","target":"offline-test","build_identity":crate::native_consumer::hash(b"offline child substitute"),"source_commit":null,"contract_manifest_schema":"podway.contract-manifest/v2","contract_manifest_digest":c.manifest_digest}),
                    )
                }
                Invocation::Read(ReadCommand::Observe) => {
                    ("session.observe", self.observation.clone())
                }
                Invocation::Read(ReadCommand::Package) => {
                    ("delegation.package", self.package.clone())
                }
                Invocation::Read(ReadCommand::Evidence { source, item, page }) => {
                    assert_eq!(source, "producer");
                    assert_eq!(&invocation.argv(c)[5..7], ["evidence", "read"]);
                    self.evidence_pages += 1;
                    if matches!(self.evidence_fault, Some(EvidenceFault::Unavailable)) {
                        return Err(crate::native_consumer::error(
                            "source_unavailable",
                            "child",
                            crate::native_consumer::Effect::NotCalled,
                        ));
                    }
                    let (_, kind, data, _) =
                        self.evidence.iter().find(|(id, ..)| id == item).unwrap();
                    let start = page
                        .as_ref()
                        .map(|s| s.strip_prefix("page-").unwrap().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    let (total, end, unit, payload) = match kind.as_str() {
                        "text" => {
                            let chars = data.as_str().unwrap().chars().collect::<Vec<_>>();
                            let end = (start + 262_144 / 6).min(chars.len());
                            (
                                chars.len(),
                                end,
                                "scalars",
                                json!(chars[start..end].iter().collect::<String>()),
                            )
                        }
                        "list" => {
                            let array = data.as_array().unwrap();
                            let end = (start + 5).min(array.len());
                            (array.len(), end, "entries", json!(&array[start..end]))
                        }
                        "check_result" => {
                            let n = serde_json::to_vec(data).unwrap().len();
                            (n, n, "bytes", data.clone())
                        }
                        _ => (1, 1, "value", data.clone()),
                    };
                    let group = &self.package["package"]["evidence"][0];
                    let metadata = group["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|v| v["item_id"] == *item)
                        .unwrap();
                    (
                        "evidence.read",
                        json!({"schema":"podway.evidence-read-result/v1","procedure_schema":"podway.procedure/v2","consumer":{"graph_node_id":c.placement,"attempt_id":c.attempt,"attempt_number":1},"source":{"graph_node_id":"producer","title":"Producer","attempt_id":group["source_attempt_id"],"attempt_number":1,"items_digest":group["source_items_digest"]},"item":{"item_id":item,"type":kind,"revision":3},"value_digest":metadata["value_digest"],"total_size":total,"size_unit":unit,"page":{"offset":start,"size":end-start,"data":payload},"truncated":end<total,"next_page_token":if end<total{Some(format!("page-{end}"))}else{None},"page_token_version":1}),
                    )
                }
                Invocation::Mutate(mutation) => {
                    launched(4242)?;
                    self.launches.set(self.launches.get() + 1);
                    self.trigger("live");
                    let input: Value = serde_json::from_slice(input).unwrap();
                    let command = if *mutation == Mutation::Accept {
                        "delegation.accept"
                    } else {
                        "item.record_many"
                    };
                    let after = input["session_revision"].as_u64().unwrap() + 1;
                    let result = if *mutation == Mutation::Accept {
                        assert_eq!(
                            self.observation["status"]["current"]["readiness"]["items_satisfied"],
                            true
                        );
                        assert_eq!(
                            self.observation["status"]["current"]["missing_required_item_count"],
                            0
                        );
                        assert_eq!(
                            self.observation["active_items"][0]["value"],
                            "Previously recorded summary."
                        );
                        json!({"schema":"podway.delegation-accept-result/v2","receipt_id":Uuid::now_v7(),"workspace_uuid":c.workspace,"session_id":c.session,"session_revision":after,"attempt_id":c.attempt,"package_digest":input["package_digest"],"result_digest":input["result_digest"],"acceptance_state":"accepted"})
                    } else {
                        json!({"schema":"podway.item-record-many-result/v2","changed":true,"graph_node_id":c.placement,"attempt_id":c.attempt,"attempt_number":1,"revision":after,"items":[{"item_id":"summary","expected_item_revision":2,"changed":true,"item_revision":3,"value_digest":crate::native_consumer::hash(b"recorded summary")} ]})
                    };
                    let mut value = Self::envelope(c, command, result);
                    value["session"] = json!({"id":c.session,"title":"Review","lifecycle":"running","revision_before":input["session_revision"],"revision_after":after});
                    if matches!(self.result, ConsumerResult::ForeignPlacement) {
                        value["result"]["graph_node_id"] = json!("foreign");
                    }
                    self.input = Some(input);
                    self.response = Some(value.clone());
                    if !matches!(self.result, ConsumerResult::Rejected) {
                        self.observation["status"]["session"]["revision"] = json!(after);
                    }
                    self.trigger("mutation");
                    if matches!(self.result, ConsumerResult::Lost) {
                        return Err(crate::native_consumer::error(
                            "uncertain_outcome",
                            "child",
                            crate::native_consumer::Effect::Unknown,
                        ));
                    }
                    if matches!(self.result, ConsumerResult::Rejected) {
                        self.response = None;
                        let value = json!({"schema":"podway.error/v2","request_id":Uuid::now_v7(),"command":command,"generated_at":"2026-10-27T00:00:00Z","code":"ITEM_CONSTRAINT_FAILED","message":"Explicit refusal","retryable":false,"exit_code":1,"details":{}});
                        return crate::podway_client::parse_reply(
                            &serde_json::to_vec(&value).unwrap(),
                            Some(1),
                            command,
                            c,
                        );
                    }
                    assert_eq!(invocation.name(), command);
                    return crate::podway_client::parse_reply(
                        &serde_json::to_vec(&value).unwrap(),
                        Some(0),
                        command,
                        c,
                    );
                }
                Invocation::Read(ReadCommand::Outcome(key)) => {
                    let result = if let Some(response) = &self.response {
                        let input = self.input.as_ref().unwrap();
                        assert_eq!(input["idempotency_key"], *key);
                        json!({"schema":"podway.outcome-lookup-result/v1","idempotency_key":key,"found":true,"outcome":{"scope":"session","request_digest":Self::semantic_digest(input,response["command"].as_str().unwrap()),"recorded_at":"2026-10-27T00:00:00Z","response":response}})
                    } else {
                        json!({"schema":"podway.outcome-lookup-result/v1","idempotency_key":key,"found":false,"outcome":null})
                    };
                    self.trigger("lookup");
                    ("outcome.lookup", result)
                }
                Invocation::Read(ReadCommand::Receipt) => {
                    let input = self.input.as_ref().unwrap();
                    let mut receipt = input.clone();
                    let object = receipt.as_object_mut().unwrap();
                    object.remove("idempotency_key");
                    object.remove("session_revision");
                    receipt["schema"] = json!("podway.delegation-receipt/v1");
                    receipt["receipt_id"] =
                        self.response.as_ref().unwrap()["result"]["receipt_id"].clone();
                    receipt["procedure_digest"] =
                        self.package["package"]["procedure_digest"].clone();
                    receipt["placement_id"] = json!(c.placement);
                    receipt["delegation"] = self.package["package"]["delegation"].clone();
                    receipt["accepted_items_digest"] =
                        crate::native_consumer::hash(b"items").into();
                    receipt["accepted_at"] = json!("2026-10-27T00:00:00Z");
                    (
                        "delegation.receipt",
                        json!({"schema":"podway.delegation-receipt-result/v1","session_id":c.session,"attempt_id":c.attempt,"receipt":receipt,"applicability":"accepted","reason":"current_receipt"}),
                    )
                }
            };
            assert_eq!(invocation.name(), command, "selected canonical route");
            let mut value = Self::envelope(c, command, result);
            if let Invocation::Read(ReadCommand::Evidence { item, page, .. }) = &invocation {
                match self.evidence_fault {
                    Some(EvidenceFault::ArtifactShape) if item == "artifact" => {
                        let data = value["result"]["page"]["data"].as_object_mut().unwrap();
                        data.remove("location_type");
                        data.insert("location_kind".into(), json!("external_reference"));
                    }
                    Some(EvidenceFault::CheckDiscriminator) if item == "check" => {
                        value["result"]["page"]["data"]["type"] = json!("check_result");
                        // Keep raw byte metadata consistent: otherwise only the
                        // existing page-size guard would reject this control.
                        let size = serde_json::to_vec(&value["result"]["page"]["data"])
                            .unwrap()
                            .len();
                        value["result"]["page"]["size"] = json!(size);
                        value["result"]["total_size"] = json!(size);
                    }
                    Some(EvidenceFault::Digest) if item == "choice" => {
                        value["result"]["page"]["data"] = json!("changed");
                    }
                    Some(EvidenceFault::ContinuationSource) if item == "text" && page.is_some() => {
                        value["result"]["source"]["attempt_id"] = json!(Uuid::now_v7());
                    }
                    _ => {}
                }
            }
            if command == "version" {
                value.as_object_mut().unwrap().remove("workspace");
            }
            crate::podway_client::parse_reply(
                &serde_json::to_vec(&value).unwrap(),
                Some(0),
                command,
                c,
            )
        }
    }
    fn consumer_scene(managed: bool) -> Scene {
        Scene::new_with_caller(
            managed,
            HostRoute::Codex,
            false,
            false,
            Some(crate::delegation_identity::CallerScope {
                binding_ref: "binding".into(),
                podway_workspace_ref: Uuid::now_v7().to_string(),
                podway_session_ref: Uuid::now_v7().to_string(),
                podway_attempt_ref: Uuid::now_v7().to_string(),
                delegated_placement_ref: "review".into(),
                max_rounds: 4,
            }),
        )
    }
    fn consumer_connection(scene: &Scene) -> crate::native_consumer::PodwayConnection {
        let owner = scene.f.owner();
        let executable = owner.root.path.join("offline-podway");
        private(&executable, b"offline typed child substitute");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o500)).unwrap();
        let caller = &owner.configuration.caller_scope;
        crate::native_consumer::PodwayConnection {
            executable,
            executable_sha256: crate::native_consumer::hash(b"offline typed child substitute"),
            manifest_digest: crate::native_consumer::hash(b"selected manifest"),
            worktree: scene.f.workspace.clone(),
            workspace: Uuid::parse_str(&caller.podway_workspace_ref).unwrap(),
            session: Uuid::parse_str(&caller.podway_session_ref).unwrap(),
            attempt: Uuid::parse_str(&caller.podway_attempt_ref).unwrap(),
            placement: caller.delegated_placement_ref.clone(),
            adapter_id: owner.configuration.adapter_id.clone(),
            adapter_version: owner.configuration.adapter_version.clone(),
            permitted_items: std::collections::BTreeSet::from(["summary".into()]),
        }
    }
    fn consumer_retained_task(scene: &Scene, package: &Value) -> Uuid {
        consumer_retained_task_output(scene, package, false)
    }
    fn consumer_retained_task_output(scene: &Scene, package: &Value, v3: bool) -> Uuid {
        let owner = scene.f.owner();
        let mut selected = scene.selection(
            "delegated-content",
            None,
            if v3 {
                "structured_review_v3"
            } else {
                "complete_result"
            },
        );
        selected
            .task
            .contexts
            .push(crate::task_request::TaskContext {
                id: "work-package".into(),
                content: serde_json::to_string(&package["package"]).unwrap(),
                provenance: "Selected public delegation package".into(),
            });
        let accepted = prepare_assignment(&owner, selected.clone()).unwrap();
        let (observed, _) = scene.assign(&selected, accepted.clone()).unwrap();
        let task = Uuid::parse_str(&observed.task_ref).unwrap();
        let verdict = json!({"summary":"Original review 界","findings":[],"criterion_assessments":[],"evidence_limits":[],"overall_assessment":"requirements_met"});
        let response = if v3 {
            serde_json::to_vec_pretty(&verdict).unwrap()
        } else {
            b"Actual retained result for explicit Primary review.".to_vec()
        };
        let bytes = response.as_slice();
        let complete = crate::delegation_identity::CompleteResult {
            schema: crate::delegation_identity::COMPLETE_RESULT_SCHEMA.into(),
            source: crate::delegation_identity::ResultSource {
                scope_kind: scene.aggregate.scope_kind().into(),
                scope_ref: scene.aggregate.scope_ref().to_string(),
                worker_ref: scene.member.to_string(),
                task_ref: task.to_string(),
                role_snapshot_digest: accepted.role_snapshot_digest.clone(),
                agent_configuration_digest: accepted.agent_configuration_digest.clone(),
                accepted_task_digest: accepted.digest().unwrap(),
            },
            terminal_outcome: "completed".into(),
            final_response: Some(crate::delegation_identity::BytesRef::from_bytes(bytes)),
            structured_output_schema: v3.then(|| "structured_review_v3".into()),
            structured_output: v3.then(|| {
                crate::delegation_identity::BytesRef::from_bytes(&canonical(&verdict).unwrap())
            }),
            changes: vec![],
            attachments: vec![],
            safe_failure_code: None,
        };
        let canonical = canonical(&complete).unwrap();
        let mut db = crate::engagement::EngagementStore::open(
            &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
        )
        .unwrap();
        db.transact(|tx| {
            crate::storage_observation::put_content(
                tx,
                "accepted_task",
                &task.to_string(),
                "final_response",
                bytes,
            )?;
            match scene.aggregate {
                SettlementOwner::External { engagement_id } => {
                    let digest = crate::jcs::sha256_hex(&canonical);
                    let artifact = crate::engagement::retain_task_result(
                        tx,
                        engagement_id,
                        task,
                        &canonical,
                        &digest,
                    )?;
                    crate::engagement::terminalize_external_task_in(
                        tx,
                        crate::engagement::TaskTerminal {
                            engagement_id,
                            task_id: task,
                            state: "completed_not_delivered",
                            safe_error_code: None,
                            result_sha256: Some(&digest),
                            artifact_id: Some(artifact),
                            diagnostic_json: None,
                            now_ms: 1,
                        },
                    )?;
                }
                SettlementOwner::Managed { session_id } => {
                    crate::orchestration::retain_successor_task_result(tx, task, &canonical)?;
                    assert!(crate::orchestration::settle_successor_task_result(
                        tx,
                        session_id,
                        task,
                        "completed_not_delivered",
                        None,
                        Some(&crate::jcs::sha256_hex(&canonical)),
                        1
                    )?);
                }
            }
            crate::caller_scope::accepted(tx, task)?;
            Ok(())
        })
        .unwrap();
        task
    }
    fn consumer_bridge<'a>(
        scene: &'a Scene,
        owner: &'a VerifiedOwner,
    ) -> crate::consumer_bridge::Bridge<'a> {
        let root = scene.aggregate.scope_ref();
        crate::consumer_bridge::Bridge {
            store: crate::native_consumer::Store::open(
                owner,
                crate::native_consumer::tests::witness(owner),
            )
            .unwrap(),
            source: crate::consumer_bridge::ProductSource {
                owner,
                port: &scene.port,
                managed: matches!(scene.aggregate, SettlementOwner::Managed { .. }).then_some(
                    services::ManagedCallContext {
                        session_id: root,
                        source_run_id: root,
                        source_turn_id: "root-turn",
                        source_tool_call_id: "root-call",
                        idempotency_key: "consumer-call",
                    },
                ),
            },
        }
    }
    fn consumer_control(mut value: Value) -> crate::native_consumer::Control {
        value["schema"] = json!("dolgorae.consumer-control/v1");
        crate::native_consumer::Control::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn prepare_consumer_call(
        scene: &Scene,
        bridge: &crate::consumer_bridge::Bridge<'_>,
        client: &mut ScriptedPodway<'_>,
        task: Uuid,
        record: bool,
    ) -> crate::native_consumer::Control {
        let evidence = bridge
            .source
            .select(
                &bridge.store,
                &scene.aggregate.scope_ref().to_string(),
                scene.member,
                task,
            )
            .unwrap();
        if record {
            let context = bridge.handle(
                client,
                &consumer_control(json!({"action":"status","target":{"kind":"context"}})),
            );
            assert_eq!(context["ok"], true, "{context}");
            consumer_control(
                json!({"action":"prepare_record","request_id":"request","context_ref":context["data"]["context_ref"],"evidence_ref":evidence,"operations":[{"item_id":"summary","record":{"type":"text","value":"Inspected original evidence."}}]}),
            )
        } else {
            consumer_control(
                json!({"action":"prepare_accept","request_id":"request","evidence_ref":evidence,"summary":"Accepted after inspecting original evidence."}),
            )
        }
    }
    fn explicit_consumer_call(record: bool) -> crate::native_consumer::Control {
        consumer_control(
            json!({"action":if record{"record"}else{"accept"},"request_id":"request","decision":if record{"record"}else{"accept"}}),
        )
    }
    fn collected_consumer_evidence(
        scene: &Scene,
        bridge: &crate::consumer_bridge::Bridge<'_>,
        client: &mut ScriptedPodway<'_>,
        task: Uuid,
    ) -> String {
        let owner = scene.f.owner();
        let op = if matches!(scene.aggregate, SettlementOwner::Managed { .. }) {
            "collect_specialist_task"
        } else {
            "collect_external_specialist_task"
        };
        let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":op,"scope_kind":scene.aggregate.scope_kind(),"scope_ref":scene.aggregate.scope_ref(),"member_ref":scene.member,"task_ref":task});
        let public = crate::specialist_facade::finish(
            op,
            crate::specialist_facade::dispatch(
                &scene.f.home,
                &owner,
                &scope(&owner),
                bridge.source.managed.as_ref(),
                &scene.port,
                &request,
                None,
            ),
            true,
        );
        assert!(
            crate::specialist_facade::validates(&public, &format!("{op}_response")),
            "{public}"
        );
        let presented = bridge.semantic(client, &request, &public);
        assert_eq!(presented["ok"], true, "{presented}");
        presented["evidence"]["selection_ref"]
            .as_str()
            .unwrap()
            .into()
    }
    #[test]
    fn consumer_bridge_delegated_record_accept_recovery_with_complete_evidence() {
        for managed in [false, true] {
            for record in [false, true] {
                let scene = consumer_scene(managed);
                let owner = scene.f.owner();
                let connection = consumer_connection(&scene);
                let mut client = ScriptedPodway::new(&scene, &connection);
                let task = consumer_retained_task(&scene, &client.package);
                crate::native_consumer::initialize(
                    &owner,
                    crate::native_consumer::Mode::Delegated {
                        connection: Box::new(connection),
                    },
                )
                .unwrap();
                let bridge = consumer_bridge(&scene, &owner);
                let mut control = prepare_consumer_call(&scene, &bridge, &mut client, task, record);
                let evidence = collected_consumer_evidence(&scene, &bridge, &mut client, task);
                match &mut control.action {
                    crate::native_consumer::Action::PrepareRecord { evidence_ref, .. }
                    | crate::native_consumer::Action::PrepareAccept { evidence_ref, .. } => {
                        *evidence_ref = evidence.clone()
                    }
                    _ => unreachable!(),
                }
                let prepared = bridge.handle(&mut client, &control);
                assert_eq!(prepared["ok"], true, "{prepared}");
                assert_eq!(prepared["data"]["state"], "prepared");
                if !record {
                    assert!(
                        client.evidence_pages >= 24,
                        "all seven kinds and complete large list/text pages"
                    );
                }
                let result = bridge.handle(&mut client, &explicit_consumer_call(record));
                assert_eq!(result["ok"], true, "{result}");
                assert_eq!(result["data"]["state"], "committed");
                assert!(client.evidence_pages >= 24);
                let recovered=bridge.handle(&mut client,&consumer_control(json!({"action":"status","target":{"kind":"request","request_id":"request"}})));
                assert_eq!(recovered["ok"], true, "{recovered}");
                assert_eq!(recovered["data"]["effect"], "committed");
                assert_eq!(recovered["data"]["outcome"], result["data"]["outcome"]);
                assert_eq!(recovered["data"]["receipt"], result["data"]["receipt"]);
                let replay = bridge.handle(&mut client, &explicit_consumer_call(record));
                assert_eq!(replay["data"]["effect"], "committed");
                assert_eq!(replay["data"]["outcome"], result["data"]["outcome"]);
                assert_eq!(replay["data"]["receipt"], result["data"]["receipt"]);
                assert_eq!(client.launches.get(), 1);
                if !record {
                    let original = bridge.store.request("request").unwrap().outcome;
                    client.response.as_mut().unwrap()["result"]["receipt_id"] =
                        json!(Uuid::now_v7());
                    let refused=bridge.handle(&mut client,&consumer_control(json!({"action":"status","target":{"kind":"request","request_id":"request"}})));
                    assert_eq!(refused["ok"], false, "{refused}");
                    assert_eq!(refused["error"]["effect"], "committed");
                    assert_eq!(bridge.store.request("request").unwrap().outcome, original);
                    assert_eq!(client.launches.get(), 1);
                }
                if record {
                    let inspection = consumer_control(
                        json!({"action":"inspect","selection_ref":prepared["data"]["input"]["selection_ref"],"view":{"kind":"receipt"},"range":{"kind":"start","offset":0}}),
                    );
                    let failed = bridge.handle(&mut client, &inspection);
                    assert_eq!(failed["ok"], false);
                    assert_eq!(failed["error"]["effect"], "committed");
                }
            }
        }
    }
    #[test]
    fn consumer_collected_content_requires_fresh_idle_proof_before_record_or_accept() {
        use crate::native_consumer::Action;
        for managed in [false, true] {
            let scene = consumer_scene(managed);
            let owner = scene.f.owner();
            let connection = consumer_connection(&scene);
            let mut client = ScriptedPodway::new(&scene, &connection);
            let task = consumer_retained_task(&scene, &client.package);
            crate::native_consumer::initialize(
                &owner,
                crate::native_consumer::Mode::Delegated {
                    connection: Box::new(connection),
                },
            )
            .unwrap();
            let bridge = consumer_bridge(&scene, &owner);
            let evidence = collected_consumer_evidence(&scene, &bridge, &mut client, task);
            let mut controls = Vec::new();
            for record in [false, true] {
                let mut control = prepare_consumer_call(&scene, &bridge, &mut client, task, record);
                match &mut control.action {
                    Action::PrepareRecord {
                        request_id,
                        evidence_ref,
                        ..
                    }
                    | Action::PrepareAccept {
                        request_id,
                        evidence_ref,
                        ..
                    } => {
                        *request_id = format!("idle-{record}");
                        *evidence_ref = evidence.clone();
                    }
                    _ => unreachable!(),
                }
                let prepared = bridge.handle(&mut client, &control);
                assert_eq!(prepared["ok"], true, "{prepared}");
                assert_eq!(prepared["data"]["state"], "prepared");
                controls.push((record, control));
            }
            let next = scene.selection("later-consumer-work", None, "complete_result");
            let expected = prepare_assignment(&owner, next.clone()).unwrap();
            scene.assign(&next, expected).unwrap();
            for (record, mut control) in controls {
                match &mut control.action {
                    Action::PrepareRecord { request_id, .. }
                    | Action::PrepareAccept { request_id, .. } => {
                        *request_id = format!("busy-{record}")
                    }
                    _ => unreachable!(),
                }
                let refused = bridge.handle(&mut client, &control);
                assert_eq!(refused["ok"], false, "{refused}");
                assert_eq!(refused["error"]["code"], "DISPOSITION_NOT_IDLE");
                assert_eq!(refused["error"]["effect"], "not_called");
                assert_eq!(client.launches.get(), 0);
            }
        }
    }

    #[test]
    fn consumer_bridge_equivalent_prepare_interleavings_retain_one_request_selection() {
        for record in [false, true] {
            let scene = consumer_scene(false);
            let owner = scene.f.owner();
            let connection = consumer_connection(&scene);
            let (package, observation) = {
                let template = ScriptedPodway::new(&scene, &connection);
                (template.package, template.observation)
            };
            let task = consumer_retained_task(&scene, &package);
            crate::native_consumer::initialize(
                &owner,
                crate::native_consumer::Mode::Delegated {
                    connection: Box::new(connection.clone()),
                },
            )
            .unwrap();
            let bridge = consumer_bridge(&scene, &owner);
            let mut client = ScriptedPodway::new(&scene, &connection);
            client.package = package;
            client.observation = observation;
            let control = prepare_consumer_call(&scene, &bridge, &mut client, task, record);
            let result = std::rc::Rc::new(std::cell::RefCell::new(None));
            {
                let mut inner = client.copy_reads();
                let result = result.clone();
                let control = control.clone();
                let bridge = &bridge;
                client.hook_at = Some("manifest");
                client.hook = Some(Box::new(move || {
                    *result.borrow_mut() = Some(bridge.handle(&mut inner, &control));
                }));
            }
            let outer = bridge.handle(&mut client, &control);
            let inner = result.borrow().clone().unwrap();
            assert_eq!(inner["ok"], true, "{inner}");
            assert_eq!(outer, inner);
            let database =
                rusqlite::Connection::open(owner.root.path.join("consumer.sqlite")).unwrap();
            let count:u64=database.query_row("SELECT COUNT(*) FROM selections WHERE json_extract(CAST(record AS TEXT),'$.kind')='request'",[],|r|r.get(0)).unwrap();
            assert_eq!(count, 1);
            drop(database);
            let mut changed = control.clone();
            match &mut changed.action {
                crate::native_consumer::Action::PrepareAccept { summary, .. } => summary.push('!'),
                crate::native_consumer::Action::PrepareRecord { operations, .. } => {
                    *operations = vec![crate::native_consumer::RecordOperation::Clear(
                        crate::native_consumer::ClearOperation {
                            item_id: "summary".into(),
                            clear: true,
                        },
                    )]
                }
                _ => unreachable!(),
            }
            let conflict = bridge.handle(&mut client, &changed);
            assert_eq!(conflict["error"]["code"], "request_conflict");
            let committed = bridge.handle(&mut client, &explicit_consumer_call(record));
            assert_eq!(committed["data"]["effect"], "committed", "{committed}");
            assert_eq!(client.launches.get(), 1);
            drop(client);
        }
    }
    #[test]
    fn consumer_bridge_post_dispatch_storage_and_authority_loss_keep_local_effect() {
        for (result, authority, expected) in [
            (ConsumerResult::Success, false, "committed"),
            (ConsumerResult::Rejected, false, "rejected"),
            (ConsumerResult::Lost, false, "unknown"),
            (ConsumerResult::Success, true, "committed"),
        ] {
            let scene = consumer_scene(false);
            let owner = scene.f.owner();
            let connection = consumer_connection(&scene);
            let mut client = ScriptedPodway::new(&scene, &connection);
            let task = consumer_retained_task(&scene, &client.package);
            crate::native_consumer::initialize(
                &owner,
                crate::native_consumer::Mode::Delegated {
                    connection: Box::new(connection),
                },
            )
            .unwrap();
            let bridge = consumer_bridge(&scene, &owner);
            let control = prepare_consumer_call(&scene, &bridge, &mut client, task, true);
            let prepared = bridge.handle(&mut client, &control);
            assert_eq!(prepared["ok"], true, "{prepared}");
            let path = if authority {
                scene.f.controller.clone()
            } else {
                owner.root.path.join("consumer.sqlite")
            };
            let saved = path.with_extension("held-by-correction-test");
            let fault_path = path.clone();
            let fault_saved = saved.clone();
            client.result = result;
            client.hook_at = Some("mutation");
            client.hook = Some(Box::new(move || {
                fs::rename(fault_path, fault_saved).unwrap()
            }));
            let failed = bridge.handle(&mut client, &explicit_consumer_call(true));
            assert_eq!(failed["ok"], false, "{failed}");
            assert_eq!(failed["error"]["effect"], expected, "{failed}");
            assert_eq!(client.launches.get(), 1);
            fs::rename(saved, path).unwrap();
            let recovered = bridge.handle(
                &mut client,
                &consumer_control(
                    json!({"action":"status","target":{"kind":"request","request_id":"request"}}),
                ),
            );
            assert_ne!(recovered["data"]["effect"], "not_called", "{recovered}");
            assert_eq!(client.launches.get(), 1);
        }
        // Initial authority loss has no locally observed child/commit and must
        // not be inflated into an uncertain or committed invocation.
        let scene = consumer_scene(false);
        let owner = scene.f.owner();
        let connection = consumer_connection(&scene);
        let mut client = ScriptedPodway::new(&scene, &connection);
        let task = consumer_retained_task(&scene, &client.package);
        crate::native_consumer::initialize(
            &owner,
            crate::native_consumer::Mode::Delegated {
                connection: Box::new(connection),
            },
        )
        .unwrap();
        let bridge = consumer_bridge(&scene, &owner);
        let control = prepare_consumer_call(&scene, &bridge, &mut client, task, true);
        assert_eq!(bridge.handle(&mut client, &control)["ok"], true);
        let saved = scene.f.controller.with_extension("held");
        fs::rename(&scene.f.controller, &saved).unwrap();
        let refused = bridge.handle(&mut client, &explicit_consumer_call(true));
        assert_eq!(refused["error"]["effect"], "not_called");
        assert_eq!(client.launches.get(), 0);
        fs::rename(saved, &scene.f.controller).unwrap();
    }
    #[test]
    fn consumer_bridge_recovery_write_loss_preserves_observed_original_success() {
        let scene = consumer_scene(false);
        let owner = scene.f.owner();
        let connection = consumer_connection(&scene);
        let mut client = ScriptedPodway::new(&scene, &connection);
        let task = consumer_retained_task(&scene, &client.package);
        crate::native_consumer::initialize(
            &owner,
            crate::native_consumer::Mode::Delegated {
                connection: Box::new(connection),
            },
        )
        .unwrap();
        let bridge = consumer_bridge(&scene, &owner);
        let control = prepare_consumer_call(&scene, &bridge, &mut client, task, false);
        assert_eq!(bridge.handle(&mut client, &control)["ok"], true);
        client.result = ConsumerResult::Lost;
        let lost = bridge.handle(&mut client, &explicit_consumer_call(false));
        assert_eq!(lost["error"]["effect"], "unknown");
        let path = owner.root.path.join("consumer.sqlite");
        let saved = owner.root.path.join("consumer-held");
        let fault_path = path.clone();
        let fault_saved = saved.clone();
        client.hook_at = Some("lookup");
        client.hook = Some(Box::new(move || {
            fs::rename(fault_path, fault_saved).unwrap()
        }));
        let status = consumer_control(
            json!({"action":"status","target":{"kind":"request","request_id":"request"}}),
        );
        let failed = bridge.handle(&mut client, &status);
        assert_eq!(failed["error"]["effect"], "committed", "{failed}");
        fs::rename(saved, path).unwrap();
        assert_eq!(
            bridge.handle(&mut client, &status)["data"]["effect"],
            "committed"
        );
        assert_eq!(client.launches.get(), 1);
    }
    #[test]
    fn consumer_bridge_refuses_actual_preimage_drift_and_foreign_success() {
        let scene = consumer_scene(false);
        let owner = scene.f.owner();
        let connection = consumer_connection(&scene);
        let mut client = ScriptedPodway::new(&scene, &connection);
        let task = consumer_retained_task(&scene, &client.package);
        crate::native_consumer::initialize(
            &owner,
            crate::native_consumer::Mode::Delegated {
                connection: Box::new(connection),
            },
        )
        .unwrap();
        let bridge = consumer_bridge(&scene, &owner);
        let control = prepare_consumer_call(&scene, &bridge, &mut client, task, true);
        assert_eq!(bridge.handle(&mut client, &control)["ok"], true);
        client.observation["active_items"][0]["revision"] = json!(3);
        let stale = bridge.handle(&mut client, &explicit_consumer_call(true));
        assert_eq!(stale["error"]["effect"], "not_called");
        assert_eq!(client.launches.get(), 0);
        client.observation["active_items"][0]["revision"] = json!(2);
        client.result = ConsumerResult::ForeignPlacement;
        let foreign = bridge.handle(&mut client, &explicit_consumer_call(true));
        assert_eq!(foreign["error"]["effect"], "unknown", "{foreign}");
        assert_eq!(client.launches.get(), 1);
        let recovered = bridge.handle(&mut client, &explicit_consumer_call(true));
        assert_eq!(recovered["error"]["effect"], "unknown");
        assert_eq!(client.launches.get(), 1);
    }

    #[test]
    fn consumer_bridge_live_duplicate_and_absent_lookup_preserve_original_owner_success() {
        let scene = consumer_scene(false);
        let owner = scene.f.owner();
        let connection = consumer_connection(&scene);
        let (package, observation) = {
            let template = ScriptedPodway::new(&scene, &connection);
            (template.package, template.observation)
        };
        let task = consumer_retained_task(&scene, &package);
        crate::native_consumer::initialize(
            &owner,
            crate::native_consumer::Mode::Delegated {
                connection: Box::new(connection.clone()),
            },
        )
        .unwrap();
        let bridge = consumer_bridge(&scene, &owner);
        let mut client = ScriptedPodway::new(&scene, &connection);
        client.package = package;
        client.observation = observation;
        let control = prepare_consumer_call(&scene, &bridge, &mut client, task, true);
        assert_eq!(bridge.handle(&mut client, &control)["ok"], true);
        let statuses = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let mut duplicate = client.copy_reads();
            let statuses = statuses.clone();
            let bridge = &bridge;
            client.hook_at = Some("live");
            client.hook = Some(Box::new(move || {
                assert!(
                    duplicate.response.is_none(),
                    "the original child is still held"
                );
                statuses.borrow_mut().push(bridge.handle(&mut duplicate, &consumer_control(json!({"action":"status","target":{"kind":"request","request_id":"request"}}))));
                statuses
                    .borrow_mut()
                    .push(bridge.handle(&mut duplicate, &explicit_consumer_call(true)));
                assert_eq!(duplicate.launches.get(), 1);
            }));
        }
        let result = bridge.handle(&mut client, &explicit_consumer_call(true));
        assert_eq!(result["data"]["effect"], "committed", "{result}");
        for status in statuses.borrow().iter() {
            assert_eq!(status["ok"], true, "{status}");
            assert_eq!(status["data"]["state"], "uncertain");
            assert_eq!(status["data"]["effect"], "unknown");
        }
        assert_eq!(statuses.borrow().len(), 2);
        let retained = bridge.store.request("request").unwrap();
        assert_eq!(retained.effect, crate::native_consumer::Effect::Committed);
        assert_eq!(retained.outcome, client.response);
        assert_eq!(
            bridge.handle(&mut client, &explicit_consumer_call(true))["data"]["effect"],
            "committed"
        );
        assert_eq!(client.launches.get(), 1);
    }
    #[test]
    fn consumer_bridge_refuses_corrupt_complete_evidence_and_continuation_association() {
        for record in [false, true] {
            for fault in [
                EvidenceFault::ArtifactShape,
                EvidenceFault::CheckDiscriminator,
                EvidenceFault::Unavailable,
                EvidenceFault::Digest,
                EvidenceFault::ContinuationSource,
            ] {
                let scene = consumer_scene(false);
                let owner = scene.f.owner();
                let connection = consumer_connection(&scene);
                let mut client = ScriptedPodway::new(&scene, &connection);
                let task = consumer_retained_task(&scene, &client.package);
                crate::native_consumer::initialize(
                    &owner,
                    crate::native_consumer::Mode::Delegated {
                        connection: Box::new(connection),
                    },
                )
                .unwrap();
                let bridge = consumer_bridge(&scene, &owner);
                let control = prepare_consumer_call(&scene, &bridge, &mut client, task, record);
                client.evidence_fault = Some(fault);
                let prepared = bridge.handle(&mut client, &control);
                let result = if record {
                    assert_eq!(prepared["ok"], true, "{prepared}");
                    bridge.handle(&mut client, &explicit_consumer_call(true))
                } else {
                    prepared
                };
                assert_eq!(result["ok"], false, "{result}");
                assert_eq!(result["error"]["effect"], "not_called");
                assert_eq!(client.launches.get(), 0);
            }
        }
    }
    #[test]
    fn consumer_bridge_preserves_resolved_missing_evidence() {
        for required in [false, true] {
            for mixed in [false, true] {
                for record in [false, true] {
                    let scene = consumer_scene(false);
                    let owner = scene.f.owner();
                    let connection = consumer_connection(&scene);
                    let mut client = ScriptedPodway::new(&scene, &connection);
                    let group = &mut client.package["package"]["evidence"][0];
                    group["selector"]["required"] = json!(required);
                    for (index, item) in group["items"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .enumerate()
                    {
                        if !mixed || index == 0 {
                            *item = json!({"item_id":item["item_id"],"state":"missing"});
                        }
                    }
                    let mut domain = b"podway.delegation-work-package/v1\n".to_vec();
                    domain.extend(serde_json::to_vec(&client.package["package"]).unwrap());
                    client.package["package_digest"] = json!(crate::native_consumer::hash(&domain));
                    let task = consumer_retained_task(&scene, &client.package);
                    crate::native_consumer::initialize(
                        &owner,
                        crate::native_consumer::Mode::Delegated {
                            connection: Box::new(connection),
                        },
                    )
                    .unwrap();
                    let bridge = consumer_bridge(&scene, &owner);
                    let context = bridge.handle(
                        &mut client,
                        &consumer_control(json!({"action":"status","target":{"kind":"context"}})),
                    );
                    assert_eq!(context["ok"], true, "{context}");
                    let package = bridge.handle(&mut client, &consumer_control(json!({"action":"inspect","selection_ref":context["data"]["context_ref"],"view":{"kind":"package"},"range":{"kind":"start","offset":0}})));
                    assert_eq!(package["ok"], true, "{package}");
                    assert_eq!(
                        package["data"]["source_digest"],
                        crate::native_consumer::hash(
                            &serde_json::to_vec(&client.package["package"]).unwrap()
                        )
                    );
                    let evidence = bridge.handle(&mut client, &consumer_control(json!({"action":"inspect","selection_ref":context["data"]["context_ref"],"view":{"kind":"package_evidence","index":0},"range":{"kind":"start","offset":0}})));
                    if mixed {
                        assert_eq!(evidence["ok"], true, "{evidence}");
                        // The absent confirm slot has no materialized bytes;
                        // the first available value is the original text.
                        let expected = &client.evidence[1].3;
                        assert_eq!(
                            evidence["data"]["source_digest"],
                            crate::native_consumer::hash(&serde_json::to_vec(expected).unwrap())
                        );
                    } else {
                        assert_eq!(evidence["ok"], false, "{evidence}");
                        assert_eq!(client.evidence_pages, 0);
                    }
                    let control = prepare_consumer_call(&scene, &bridge, &mut client, task, record);
                    let prepared = bridge.handle(&mut client, &control);
                    assert_eq!(prepared["ok"], true, "{prepared}");
                    let result = bridge.handle(&mut client, &explicit_consumer_call(record));
                    assert_eq!(result["data"]["effect"], "committed", "{result}");
                    assert_eq!(client.launches.get(), 1);
                    if !mixed {
                        assert_eq!(client.evidence_pages, 0);
                    }
                }
            }
        }
    }
    #[test]
    fn consumer_bridge_refuses_required_unresolved_and_malformed_missing_evidence() {
        for malformed in [false, true] {
            let scene = consumer_scene(false);
            let owner = scene.f.owner();
            let connection = consumer_connection(&scene);
            let mut client = ScriptedPodway::new(&scene, &connection);
            let group = &mut client.package["package"]["evidence"][0];
            if malformed {
                group["items"][0] = json!({"item_id":"confirmed","state":"missing","value":true});
            } else {
                group["resolution"] = json!("unresolved");
                let object = group.as_object_mut().unwrap();
                object.remove("source_attempt_id");
                object.remove("source_attempt_number");
                object.remove("source_items_digest");
                for item in group["items"].as_array_mut().unwrap() {
                    *item = json!({"item_id":item["item_id"],"state":"missing"});
                }
            }
            let mut domain = b"podway.delegation-work-package/v1\n".to_vec();
            domain.extend(serde_json::to_vec(&client.package["package"]).unwrap());
            client.package["package_digest"] = json!(crate::native_consumer::hash(&domain));
            let task = consumer_retained_task(&scene, &client.package);
            crate::native_consumer::initialize(
                &owner,
                crate::native_consumer::Mode::Delegated {
                    connection: Box::new(connection),
                },
            )
            .unwrap();
            let bridge = consumer_bridge(&scene, &owner);
            let control = prepare_consumer_call(&scene, &bridge, &mut client, task, false);
            let result = bridge.handle(&mut client, &control);
            assert_eq!(result["ok"], false, "{result}");
            assert_eq!(result["error"]["effect"], "not_called");
            assert_eq!(client.launches.get(), 0);
            assert_eq!(client.evidence_pages, 0);
        }
    }
    #[test]
    fn consumer_bridge_materializes_authorized_owner_content_and_refuses_retirement() {
        for (managed, large) in [(false, false), (true, false), (false, true)] {
            let outside: crate::delegation_identity::DelegationIdentity =
                serde_json::from_str(include_str!(
                    "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
                ))
                .unwrap();
            outside.validate().unwrap();
            let scene = Scene::new_with_caller(
                managed,
                HostRoute::Codex,
                false,
                false,
                Some(outside.caller_scope.clone()),
            );
            let owner = scene.f.owner();
            assert_eq!(owner.configuration.caller_scope, outside.caller_scope);
            let selected = scene.selection("consumer-content", None, "complete_result");
            let accepted = prepare_assignment(&owner, selected.clone()).unwrap();
            let (observed, _) = scene.assign(&selected, accepted.clone()).unwrap();
            let task = Uuid::parse_str(&observed.task_ref).unwrap();
            let tail = "original result with Korean 한글\r\n";
            let text = if large {
                format!(
                    "{}{tail}",
                    "x".repeat(crate::native_consumer::MAX_INPUT + 17)
                )
            } else {
                tail.into()
            };
            let bytes = text.as_bytes();
            let complete = crate::delegation_identity::CompleteResult {
                schema: crate::delegation_identity::COMPLETE_RESULT_SCHEMA.into(),
                source: crate::delegation_identity::ResultSource {
                    scope_kind: scene.aggregate.scope_kind().into(),
                    scope_ref: scene.aggregate.scope_ref().to_string(),
                    worker_ref: scene.member.to_string(),
                    task_ref: task.to_string(),
                    role_snapshot_digest: accepted.role_snapshot_digest.clone(),
                    agent_configuration_digest: accepted.agent_configuration_digest.clone(),
                    accepted_task_digest: accepted.digest().unwrap(),
                },
                terminal_outcome: "completed".into(),
                final_response: Some(crate::delegation_identity::BytesRef::from_bytes(bytes)),
                structured_output_schema: None,
                structured_output: None,
                changes: vec![],
                attachments: vec![],
                safe_failure_code: None,
            };
            let canonical = canonical(&complete).unwrap();
            let mut db = crate::engagement::EngagementStore::open(
                &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
            )
            .unwrap();
            db.transact(|tx| {
                crate::storage_observation::put_content(
                    tx,
                    "accepted_task",
                    &task.to_string(),
                    "final_response",
                    bytes,
                )?;
                match scene.aggregate {
                    SettlementOwner::External { engagement_id } => {
                        let digest = crate::jcs::sha256_hex(&canonical);
                        let artifact = crate::engagement::retain_task_result(
                            tx,
                            engagement_id,
                            task,
                            &canonical,
                            &digest,
                        )?;
                        crate::engagement::terminalize_external_task_in(
                            tx,
                            crate::engagement::TaskTerminal {
                                engagement_id,
                                task_id: task,
                                state: "completed_not_delivered",
                                safe_error_code: None,
                                result_sha256: Some(&digest),
                                artifact_id: Some(artifact),
                                diagnostic_json: None,
                                now_ms: 1,
                            },
                        )?;
                    }
                    SettlementOwner::Managed { session_id } => {
                        crate::orchestration::retain_successor_task_result(tx, task, &canonical)?;
                        assert!(crate::orchestration::settle_successor_task_result(
                            tx,
                            session_id,
                            task,
                            "completed_not_delivered",
                            None,
                            Some(&crate::jcs::sha256_hex(&canonical)),
                            1
                        )?);
                    }
                }
                crate::caller_scope::accepted(tx, task)?;
                Ok(())
            })
            .unwrap();
            drop(db);
            crate::native_consumer::initialize(&owner, crate::native_consumer::Mode::NativeOnly {})
                .unwrap();
            let store = crate::native_consumer::Store::open(
                &owner,
                crate::native_consumer::tests::witness(&owner),
            )
            .unwrap();
            let root = scene.aggregate.scope_ref();
            let source = crate::consumer_bridge::ProductSource {
                owner: &owner,
                port: &scene.port,
                managed: managed.then_some(services::ManagedCallContext {
                    session_id: root,
                    source_run_id: root,
                    source_turn_id: "root-turn",
                    source_tool_call_id: "root-call",
                    idempotency_key: "consumer-read",
                }),
            };
            let reference = source
                .select(&store, &root.to_string(), scene.member, task)
                .unwrap();
            let bridge = crate::consumer_bridge::Bridge { store, source };
            let control=crate::native_consumer::Control::parse(&serde_json::to_vec(&json!({"schema":"dolgorae.consumer-control/v1","action":"inspect","selection_ref":reference,"view":{"kind":"final_response"},"range":{"kind":"start","offset":0}})).unwrap()).unwrap();
            struct NoConsumer;
            impl crate::podway_client::Client for NoConsumer {
                fn invoke(
                    &mut self,
                    _: &crate::native_consumer::PodwayConnection,
                    _: crate::podway_client::Invocation,
                    _: &[u8],
                    _: &mut dyn FnMut(u32) -> Result<(), MachineError>,
                ) -> Result<crate::podway_client::Reply, MachineError> {
                    panic!("native-only inspection called Podway")
                }
            }
            let read = bridge.handle(&mut NoConsumer, &control);
            assert_eq!(read["ok"], true, "{read}");
            assert_eq!(read["data"]["source_length"], bytes.len());
            assert_eq!(
                read["data"]["source_digest"],
                crate::native_consumer::hash(bytes)
            );
            if large {
                assert!(bytes.len() > crate::native_consumer::MAX_INPUT);
                assert_eq!(read["data"]["text"], &text[..1024]);
                let mut tail_control = control.clone();
                if let crate::native_consumer::Action::Inspect { range, .. } =
                    &mut tail_control.action
                {
                    *range = crate::native_consumer::Range::Start {
                        offset: (bytes.len() - tail.len()) as u64,
                    };
                }
                let last = bridge.handle(&mut NoConsumer, &tail_control);
                assert_eq!(last["ok"], true, "{last}");
                assert_eq!(last["data"]["text"], tail);
                assert!(last["data"]["continuation"].is_null());
            } else {
                assert_eq!(read["data"]["text"], std::str::from_utf8(bytes).unwrap());
            }
            assert!(
                bridge
                    .source
                    .select(
                        &bridge.store,
                        &root.to_string(),
                        scene.member,
                        Uuid::now_v7()
                    )
                    .is_err()
            );
            let db = rusqlite::Connection::open(
                crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
            )
            .unwrap();
            db.execute("DELETE FROM result_content_refs WHERE owner_kind='accepted_task' AND owner_id=?1 AND representation='final_response'",[task.to_string()]).unwrap();
            let refused = bridge.handle(&mut NoConsumer, &control);
            assert_eq!(refused["ok"], false, "{refused}");
            assert_eq!(refused["error"]["effect"], "not_called");
        }
    }

    #[test]
    fn source_binding_full_fresh_identity_and_source_target_drift_are_effect_free() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Codex, true);
            let owner = scene.f.owner();
            let selection = scene.selection("fresh", Some(("dirty", None)), "structured_review_v4");
            let identity = prepare_assignment(&owner, selection.clone()).unwrap();
            let before = scene.effects();
            let calls = scene.port.calls();
            let mut bad = identity.clone();
            bad.admitted_model.push('x');
            assert!(scene.assign(&selection, bad).is_err());
            assert_eq!(scene.effects(), before);
            fs::write(scene.f.workspace.join("file"), b"drift\n").unwrap();
            assert_eq!(
                scene.assign(&selection, identity.clone()).unwrap_err().code,
                "IDEMPOTENCY_CONFLICT"
            );
            assert_eq!(scene.effects(), before);
            assert_eq!(scene.port.calls(), calls);
            fs::write(scene.f.workspace.join("file"), b"base\n").unwrap();
            assert!(scene.assign(&selection, identity).is_ok());
            let sources = scene
                .selected
                .capture(&scene.f.home)
                .unwrap()
                .sources_identity()
                .unwrap();
            let mut changed = scene.selected.clone();
            changed.role = SelectedInstructionSource::Inline("different role".into());
            let other = changed
                .capture(&scene.f.home)
                .unwrap()
                .sources_identity()
                .unwrap();
            assert!(fresh_provisioning(&scene.f.home, &owner, &other).is_err());
            fs::write(
                scene.f.home.root().join("roles/reviewer/AGENTS.md"),
                b"source drift after preparation\n",
            )
            .unwrap();
            assert!(
                fresh_provisioning(&scene.f.home, &owner, &sources).is_err(),
                "fresh source bytes differ from registration"
            );
            fs::remove_dir_all(scene.f.home.root().join("roles")).unwrap();
            assert!(fresh_provisioning(&scene.f.home, &owner, &sources).is_err());
        }
    }

    #[test]
    fn source_binding_target_selection_and_context_boundary_corpus() {
        for managed in [false, true] {
            for kind in ["workspace", "staged", "dirty", "head", "commit", "range"] {
                let scene = Scene::new(managed, HostRoute::Codex, true);
                let owner = scene.f.owner();
                let head = scene.git(&["rev-parse", "HEAD"]);
                let range = format!("{head}..{head}");
                let revision = match kind {
                    "commit" => Some(head.as_str()),
                    "range" => Some(range.as_str()),
                    _ => None,
                };
                let selection =
                    scene.selection(kind, Some((kind, revision)), "structured_review_v4");
                let identity = prepare_assignment(&owner, selection.clone()).unwrap();
                let expected = crate::direct_review_target::DirectTarget::admit(
                    &scene.f.workspace,
                    owner.view.mode,
                    kind,
                    revision,
                    Uuid::now_v7(),
                )
                .unwrap();
                assert_eq!(
                    identity.contexts.last().unwrap().identity,
                    expected.target_sha256
                );
                assert!(
                    scene.assign(&selection, identity).is_ok(),
                    "{managed}/{kind}"
                );
            }
        }
        for managed in [false, true] {
            for count in [63, 64] {
                let scene = Scene::new(managed, HostRoute::Codex, false);
                let mut input = scene.selection(
                    "contexts",
                    Some(("workspace", None)),
                    "structured_review_v4",
                );
                input.task.contexts = (0..count)
                    .map(|n| crate::task_request::TaskContext {
                        id: format!("c{n}"),
                        content: "body".into(),
                        provenance: "caller".into(),
                    })
                    .collect();
                let before = scene.effects();
                let result = prepare_assignment(&scene.f.owner(), input.clone());
                assert_eq!(result.is_ok(), count == 63);
                if let Ok(expected) = result {
                    assert_eq!(expected.contexts.len(), 64);
                    assert!(scene.assign(&input, expected).is_ok());
                } else {
                    assert_eq!(scene.effects(), before);
                }
            }
        }
    }

    #[test]
    fn source_binding_partition_and_strict_record_grammar_corpus() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Claude, false);
            let owner = scene.f.owner();
            let directory = owner.root.path.join(DIRECTORY);
            let metadata = directory.join("metadata.json");
            let original = fs::read(&metadata).unwrap();
            let before = scene.effects();
            let value: Value = serde_json::from_slice(&original).unwrap();
            let mut cases = Vec::new();
            for field in value.as_object().unwrap().keys() {
                let mut changed = value.clone();
                changed.as_object_mut().unwrap().remove(field);
                cases.push(canonical(&changed).unwrap());
                let mut changed = value.clone();
                changed[field] = Value::Null;
                cases.push(canonical(&changed).unwrap());
            }
            for field in [
                "owner_digest",
                "workspace",
                "canonical_workspace",
                "principal",
            ] {
                let mut changed = value.clone();
                changed[field] = json!("foreign");
                cases.push(canonical(&changed).unwrap());
            }
            for field in ["birth"] {
                for text in [
                    Uuid::now_v7().to_string(),
                    value[field].as_str().unwrap().replace('-', ""),
                    "{00000000-0000-7000-8000-000000000000}".into(),
                ] {
                    let mut changed = value.clone();
                    changed[field] = json!(text);
                    cases.push(canonical(&changed).unwrap());
                }
            }
            for field in ["generation", "controller_generation", "schema_version"] {
                let mut changed = value.clone();
                changed[field] = json!(2);
                cases.push(canonical(&changed).unwrap());
            }
            for field in [
                "binding_ref",
                "podway_workspace_ref",
                "podway_session_ref",
                "podway_attempt_ref",
                "delegated_placement_ref",
                "max_rounds",
            ] {
                let mut changed = value.clone();
                changed["caller"][field] = if field == "max_rounds" {
                    json!(5)
                } else {
                    json!("foreign")
                };
                cases.push(canonical(&changed).unwrap());
            }
            let mut unknown = value.clone();
            unknown["unexpected_private_field"] = json!("reject");
            cases.push(canonical(&unknown).unwrap());
            cases.extend([
                b"{\"schema_version\":1,\"schema_version\":1}".to_vec(),
                b"{\"x\":1,\"\\u0078\":2}".to_vec(),
                b"{} {}".to_vec(),
                b"{\"x\":9007199254740992}".to_vec(),
                vec![0xff],
            ]);
            for (i, bytes) in cases.iter().enumerate() {
                private(&metadata, bytes);
                assert!(Registry::open(&owner).is_err(), "partition case{i}");
                assert_eq!(fs::read(&metadata).unwrap(), *bytes);
                assert_eq!(scene.effects(), before);
            }
            private(&metadata, &original);
            assert!(Registry::open(&owner).is_ok());
            for name in ["metadata.json", crate::trusted_host::LOCK_NAME] {
                let path = directory.join(name);
                let saved = path.with_extension("saved");
                fs::rename(&path, &saved).unwrap();
                assert!(initialize(&owner).is_err());
                assert!(!path.exists());
                fs::rename(saved, path).unwrap();
            }
        }
    }

    #[test]
    fn source_binding_storage_component_and_private_output_corpus() {
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let sources = scene
            .selected
            .capture(&scene.f.home)
            .unwrap()
            .sources_identity()
            .unwrap();
        let reg = Registry::open(&owner).unwrap();
        let name = index_name("p", &sources).unwrap();
        let original = fs::read(reg.directory.path.join(&name)).unwrap();
        for mode in [0o644, 0o400, 0o660] {
            let path = reg.directory.path.join(&name);
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                project(
                    &owner,
                    ProjectionLookup::Provisioning {
                        sources: sources.clone().into()
                    }
                )
                .is_err()
            );
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let saved = reg.directory.path.join("saved.json");
        let path = reg.directory.path.join(&name);
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        assert!(reg.record(&name).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&saved, &path).unwrap();
        assert!(reg.record(&name).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&saved, &path).unwrap();
        let record = reg.record(&name).unwrap();
        let Prepared::Provisioning {
            snapshot,
            selection,
            ..
        } = record.prepared
        else {
            panic!()
        };
        for component in [snapshot, selection] {
            let path = reg.directory.path.join(component.name().unwrap());
            let bytes = fs::read(&path).unwrap();
            private(&path, b"{}");
            assert!(
                project(
                    &owner,
                    ProjectionLookup::Provisioning {
                        sources: sources.clone().into()
                    }
                )
                .is_err()
            );
            assert!(
                prepare_provisioning(&scene.f.home, &owner, scene.selected.clone()).is_err(),
                "repeat registration must verify original components"
            );
            private(&path, &bytes);
        }
        let record = reg.record(&name).unwrap();
        let Prepared::Provisioning { selection, .. } = &record.prepared else {
            panic!()
        };
        let component_path = reg.directory.path.join(selection.name().unwrap());
        let component_bytes = fs::read(&component_path).unwrap();
        let mut alternate = scene.selected.clone();
        alternate.role = SelectedInstructionSource::Inline(
            scene.selected.capture(&scene.f.home).unwrap().role.body,
        );
        assert_eq!(
            alternate
                .capture(&scene.f.home)
                .unwrap()
                .sources_identity()
                .unwrap(),
            sources
        );
        private(&component_path, &canonical(&alternate).unwrap());
        assert!(
            project(
                &owner,
                ProjectionLookup::Provisioning {
                    sources: sources.clone().into()
                }
            )
            .is_err(),
            "immutable selection provenance was substituted"
        );
        private(&component_path, &component_bytes);
        let mut foreign = record.clone();
        foreign.partition.workspace = "foreign-workspace".into();
        let bytes = canonical(&foreign).unwrap();
        let reference = Component::of(&bytes);
        reg.publish_bytes(&reference.name().unwrap(), &bytes)
            .unwrap();
        private(&path, &canonical(&Index { record: reference }).unwrap());
        assert!(
            project(
                &owner,
                ProjectionLookup::Provisioning {
                    sources: sources.clone().into()
                }
            )
            .is_err(),
            "record came from a foreign partition"
        );
        private(&path, &original);
        let pending = reg.directory.path.join(format!("{name}.pending"));
        private(&pending, b"interrupted");
        assert!(
            project(
                &owner,
                ProjectionLookup::Provisioning {
                    sources: sources.clone().into()
                }
            )
            .is_err()
        );
        assert_eq!(fs::read(&pending).unwrap(), b"interrupted");
        fs::remove_file(pending).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        let output = scene.f.root.join("output");
        mkdir(&output);
        let destination = output.join("identity.json");
        let identity = serde_json::to_value(&sources).unwrap();
        write_projection(&owner, &destination, &identity).unwrap();
        write_projection(&owner, &destination, &identity).unwrap();
        assert_eq!(
            fs::read(&destination).unwrap(),
            canonical(&identity).unwrap()
        );
        assert!(
            write_projection(&owner, &destination, &json!({"secret":"do not replace"})).is_err()
        );
        assert!(write_projection(&owner, &owner.root.path.join("output.json"), &identity).is_err());
    }
    #[test]
    fn source_binding_private_components_preserve_large_valid_producers_and_public_carrier() {
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let mut selection = scene.selected.clone();
        selection.role = SelectedInstructionSource::Inline("\u{0001}".repeat(30_000));
        let snapshot = selection.capture(&scene.f.home).unwrap();
        assert!(canonical(&snapshot).unwrap().len() > 131_072);
        let identity = prepare_provisioning(&scene.f.home, &owner, selection).unwrap();
        assert_eq!(
            fresh_provisioning(&scene.f.home, &owner, &identity).unwrap(),
            snapshot
        );
        for n in 0..1000 {
            fs::write(
                scene
                    .f
                    .workspace
                    .join(format!("entry-{n:04}-{}", "x".repeat(160))),
                b"entry\n",
            )
            .unwrap();
        }
        let selected = scene.selection(
            "large-target",
            Some(("workspace", None)),
            "structured_review_v4",
        );
        let identity = prepare_assignment(&owner, selected.clone()).unwrap();
        assert!(identity.contexts.last().unwrap().content.byte_length > 131_072);
        assert!(scene.assign(&selected, identity).is_ok());
        let directory = PrivateDirectory::open(&scene.f.private).unwrap();
        let path = scene.f.private.join("carrier.json");
        for length in [PUBLIC_BYTES - 1, PUBLIC_BYTES, PUBLIC_BYTES + 1] {
            let public = crate::host_invocation::tests::aggregate_request(length as usize);
            let bytes = canonical(&public).unwrap();
            assert_eq!(bytes.len() as u64, length);
            private(&path, &bytes);
            let retained: Value = directory.read_bounded(&path, ASSIGNMENT_BYTES).unwrap();
            assert_eq!(retained, public);
            assert_eq!(
                crate::host_invocation::parse_public_request(std::str::from_utf8(&bytes).unwrap())
                    .is_ok(),
                length <= PUBLIC_BYTES
            );
            if length == PUBLIC_BYTES {
                let wrapped =
                    canonical(&json!({"request":public,"private_partition":"bounded metadata"}))
                        .unwrap();
                assert!(wrapped.len() as u64 > PUBLIC_BYTES);
                private(&path, &wrapped);
                let _: Value = directory.read_bounded(&path, ASSIGNMENT_BYTES).unwrap();
            }
        }
        for length in [99, 100, 101] {
            private(&path, format!("\"{}\"", "x".repeat(length - 2)).as_bytes());
            assert_eq!(
                directory.read_bounded::<Value>(&path, 100).is_ok(),
                length <= 100
            );
        }
    }

    #[test]
    fn source_binding_private_shape_grammar_and_original_uuid_corpus() {
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let selection = scene.selection("shape", Some(("workspace", None)), "structured_review_v4");
        let directory = PrivateDirectory::open(&scene.f.private).unwrap();
        let path = scene.f.private.join("input.json");
        let original = canonical(&selection).unwrap();
        let mut cases = vec![
            b"{} {}".to_vec(),
            vec![0xff],
            b"{\"x\":1,\"\\u0078\":2}".to_vec(),
            b"{\"x\":9007199254740992}".to_vec(),
        ];
        let value: Value = serde_json::from_slice(&original).unwrap();
        for field in value.as_object().unwrap().keys() {
            let mut changed = value.clone();
            changed.as_object_mut().unwrap().remove(field);
            cases.push(canonical(&changed).unwrap());
            if field != "target" {
                let mut changed = value.clone();
                changed[field] = Value::Null;
                cases.push(canonical(&changed).unwrap());
            }
        }
        for text in [
            selection.member_ref.to_string().replace('-', ""),
            selection.member_ref.to_string().to_uppercase(),
            format!("{{{}}}", selection.member_ref),
            format!("urn:uuid:{}", selection.member_ref),
            Uuid::nil().to_string(),
            "00000000-0000-4000-8000-000000000000".into(),
            "00000000-0000-7000-0000-000000000000".into(),
        ] {
            let mut changed = value.clone();
            changed["member_ref"] = json!(text);
            cases.push(canonical(&changed).unwrap());
        }
        let mut changed = value.clone();
        changed["private_authority"] = json!("never accepted");
        cases.push(canonical(&changed).unwrap());
        for bytes in cases {
            private(&path, &bytes);
            assert!(
                directory
                    .read_bounded::<AssignmentSelection>(&path, ASSIGNMENT_BYTES)
                    .is_err(),
                "private grammar case"
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        private(&path, &original);
        let actual: AssignmentSelection = directory.read_bounded(&path, ASSIGNMENT_BYTES).unwrap();
        assert_eq!(actual, selection);
        for (kind, revision) in [
            ("other", None),
            ("commit", None),
            ("range", Some("HEAD")),
            ("head", Some("HEAD")),
            ("commit", Some("")),
        ] {
            let mut selected = selection.clone();
            selected.target = Some(crate::review::ReviewTargetRequest {
                kind: kind.into(),
                revision: revision.map(str::to_owned),
            });
            assert!(prepare_assignment(&owner, selected).is_err());
        }
        let before = scene.effects();
        let mut wrong = selection.clone();
        wrong.namespace.controller_generation += 1;
        assert!(prepare_assignment(&owner, wrong).is_err());
        let mut wrong = selection.clone();
        wrong.namespace.scope_kind = Some("orchestrated_session".into());
        wrong.namespace.operation_kind = "assign_specialist_task".into();
        assert!(prepare_assignment(&owner, wrong).is_err());
        assert_eq!(scene.effects(), before);
    }

    #[test]
    fn source_binding_administrative_authority_precedes_every_publication() {
        use std::ffi::OsString;
        use std::os::fd::AsRawFd as _;
        for managed in [false, true] {
            for stage in [
                "preparation-init",
                "provisioning-prepare",
                "assignment-prepare",
                "preparation-project",
            ] {
                for authority in [
                    "current",
                    "foreign-operator",
                    "foreign-controller",
                    "rotated-operator",
                ] {
                    let scene = Scene::new(managed, HostRoute::Codex, false);
                    let owner = scene.f.owner();
                    let other = Fixture::new(HostRoute::Claude);
                    let request = scene.f.private.join("admin-source.json");
                    let selected = scene.selection(
                        "admin-only",
                        Some(("workspace", None)),
                        "structured_review_v4",
                    );
                    let value = match stage {
                        "assignment-prepare" => serde_json::to_value(selected).unwrap(),
                        "preparation-project" => {
                            json!({"kind":"provisioning","sources":scene.selected.capture(&scene.f.home).unwrap().sources_identity().unwrap()})
                        }
                        _ => serde_json::to_value(&scene.selected).unwrap(),
                    };
                    private(&request, &canonical(&value).unwrap());
                    let output = scene.f.root.join("exports");
                    mkdir(&output);
                    let destination = output.join("projection.json");
                    if authority == "rotated-operator" {
                        crate::controller::OperatorStore::new(scene.f.home.operator_root())
                            .rotate(
                                &crate::controller::CredentialCarrier::open_path(&scene.f.operator)
                                    .unwrap(),
                                &scene.f.private.join("new-operator.json"),
                            )
                            .unwrap();
                    }
                    let operator = fs::File::open(if authority == "foreign-operator" {
                        &other.operator
                    } else {
                        &scene.f.operator
                    })
                    .unwrap();
                    let controller = fs::File::open(if authority == "foreign-controller" {
                        &other.controller
                    } else {
                        &scene.f.controller
                    })
                    .unwrap();
                    let mut args = vec![
                        OsString::from(stage),
                        "--owner-config".into(),
                        scene.f.path().into_os_string(),
                        "--operator-fd".into(),
                        operator.as_raw_fd().to_string().into(),
                        "--controller-fd".into(),
                        controller.as_raw_fd().to_string().into(),
                    ];
                    if stage != "preparation-init" {
                        args.extend([
                            "--request".into(),
                            request.into_os_string(),
                            "--output".into(),
                            destination.clone().into_os_string(),
                        ]);
                    }
                    let before = scene.effects();
                    let records = fs::read_dir(owner.root.path.join(DIRECTORY))
                        .unwrap()
                        .map(|e| {
                            let e = e.unwrap();
                            (e.file_name(), fs::read(e.path()).unwrap())
                        })
                        .collect::<std::collections::BTreeMap<_, _>>();
                    let result = crate::trusted_host::tests::administrative(&args, &scene.f.home);
                    assert_eq!(
                        result.is_ok(),
                        authority == "current",
                        "{managed}/{stage}/{authority}: {result:?}"
                    );
                    assert_eq!(
                        scene.effects(),
                        before,
                        "admin preparation grants no product acceptance"
                    );
                    if authority != "current" {
                        assert!(!destination.exists());
                        assert_eq!(
                            fs::read_dir(owner.root.path.join(DIRECTORY))
                                .unwrap()
                                .map(|e| {
                                    let e = e.unwrap();
                                    (e.file_name(), fs::read(e.path()).unwrap())
                                })
                                .collect::<std::collections::BTreeMap<_, _>>(),
                            records
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn source_binding_capture_handoff_uses_checked_bytes_and_rechecks_partition() {
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let sources = scene
            .selected
            .capture(&scene.f.home)
            .unwrap()
            .sources_identity()
            .unwrap();
        let path = scene.f.home.root().join("roles/reviewer/AGENTS.md");
        let original = fs::read(&path).unwrap();
        {
            let changed = path.clone();
            let _fault = CaptureFault::at(move || {
                fs::write(changed, b"changed after checked capture\n").unwrap()
            });
            let installed = install(&scene.f.home, &owner, &sources, "handoff", None)
                .unwrap()
                .0;
            assert_eq!(
                crate::provisioning::retained_snapshot(
                    &owner.state_root,
                    &installed.provisioning_ref
                )
                .unwrap()
                .role
                .body
                .as_bytes(),
                original
            );
        }
        fs::write(&path, original).unwrap();
        let before = scene.effects();
        {
            let state = owner.root.path.join("invocations.sqlite");
            let _fault = CaptureFault::at(move || {
                let db = rusqlite::Connection::open(state).unwrap();
                db.execute("UPDATE metadata SET generation=generation+1", [])
                    .unwrap();
            });
            assert!(install(&scene.f.home, &owner, &sources, "generation-cut", None).is_err());
        }
        assert_eq!(
            scene.effects(),
            before,
            "partition changed during capture must precede intent"
        );
    }

    #[test]
    fn source_binding_partial_preparation_and_unresolved_product_state_do_not_repair_or_redispatch()
    {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        let path = owner.root.path.join(DIRECTORY);
        assert!(Registry::open(&owner).is_err());
        assert!(!path.exists());
        mkdir(&path);
        assert!(initialize(&owner).is_err());
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        for managed in [false, true] {
            for state in ["prepared", "unresolved", "refused"] {
                let scene = Scene::new(managed, HostRoute::Codex, false);
                let owner = scene.f.owner();
                let selected = scene.selection("floor", None, "structured_review_v3");
                let expected = prepare_assignment(&owner, selected.clone()).unwrap();
                let body = crate::successor_capture::assignment_body(
                    selected.member_ref,
                    &selected.task,
                    &expected,
                    selected.deadline_seconds,
                )
                .unwrap();
                services::tests::source_binding_prepare_fixture_intent(
                    &scope(&owner),
                    &selected.namespace,
                    &body,
                    state,
                );
                fs::remove_dir_all(owner.root.path.join(DIRECTORY)).unwrap();
                let before = scene.effects();
                let calls = scene.port.calls();
                services::source_trace();
                let failure = scene.assign(&selected, expected).unwrap_err();
                assert_eq!(scene.effects(), before);
                assert_eq!(scene.port.calls(), calls);
                if state != "refused" {
                    assert_eq!(failure.details["effect"], "unresolved");
                    assert!(services::source_trace().is_empty());
                } else {
                    assert!(matches!(
                        failure.code.as_str(),
                        "RECOVERY_REQUIRED" | "IDEMPOTENCY_CONFLICT"
                    ));
                }
                let sources = scene
                    .selected
                    .capture(&scene.f.home)
                    .unwrap()
                    .sources_identity()
                    .unwrap();
                let namespace =
                    namespace(&owner, None, "install_provisioning", "floor-install").unwrap();
                services::tests::source_binding_prepare_fixture_intent(
                    &scope(&owner),
                    &namespace,
                    &json!({"sources":sources}),
                    state,
                );
                let before = scene.effects();
                let failure =
                    install(&scene.f.home, &owner, &sources, "floor-install", None).unwrap_err();
                assert_eq!(scene.effects(), before);
                if state != "refused" {
                    assert_eq!(failure.details["effect"], "unresolved");
                }
            }
        }
    }

    #[test]
    fn source_binding_retained_corruption_is_recovery_not_new_work() {
        for managed in [false, true] {
            for representation in ["task_body", "target", "context:ctx-0"] {
                let scene = Scene::new(managed, HostRoute::Codex, false);
                let owner = scene.f.owner();
                let selected =
                    scene.selection("corrupt", Some(("workspace", None)), "structured_review_v4");
                let expected = prepare_assignment(&owner, selected.clone()).unwrap();
                let (observed, receipt) = scene.assign(&selected, expected.clone()).unwrap();
                let db = rusqlite::Connection::open(
                    crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
                )
                .unwrap();
                let hash:String=db.query_row("SELECT sha256 FROM result_content_refs WHERE owner_kind='accepted_task' AND owner_id=?1 AND representation=?2",rusqlite::params![observed.task_ref,representation],|r|r.get(0)).unwrap();
                let before = scene.effects();
                let calls = scene.port.calls();
                db.execute(
                    "UPDATE result_content SET bytes=?1 WHERE sha256=?2",
                    rusqlite::params![b"corrupt bytes".as_slice(), hash],
                )
                .unwrap();
                let failure = scene.assign(&selected, expected).unwrap_err();
                assert_eq!(failure.details["classification"], "RecoveryNeeded");
                assert_ne!(failure.code, "IDEMPOTENCY_CONFLICT");
                assert_eq!(scene.effects(), before);
                assert_eq!(scene.port.calls(), calls);
                let stored: String = db
                    .query_row(
                        "SELECT receipt_json FROM delegation_operations WHERE operation_ref=?1",
                        [&receipt.operation_ref],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&stored).unwrap(),
                    serde_json::to_value(receipt).unwrap()
                );
            }
        }
    }

    #[test]
    fn source_binding_non_v4_targets_and_explicit_source_negative_classes() {
        for managed in [false, true] {
            for output in ["plain_text", "structured_review_v3"] {
                for target in [None, Some(("workspace", None))] {
                    let scene = Scene::new(managed, HostRoute::Codex, false);
                    let selected = scene.selection("non-v4", target, output);
                    let expected = prepare_assignment(&scene.f.owner(), selected.clone()).unwrap();
                    assert!(scene.assign(&selected, expected).is_ok());
                }
            }
        }
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let before = scene.effects();
        for case in 0..12 {
            let mut selected = scene.selected.clone();
            match case {
                0 => selected.role_name = "../role".into(),
                1 => selected.role = SelectedInstructionSource::File(vec!["..".into()]),
                2 => selected.role = SelectedInstructionSource::File(vec!["missing".into()]),
                3 => selected.role = SelectedInstructionSource::Inline("".into()),
                4 => selected.role = SelectedInstructionSource::Inline("\0".into()),
                5 => selected.dependencies.push(selected.dependencies[0].clone()),
                6 => selected.dependencies[0].name = "../alias".into(),
                7 => {
                    selected.selected_guidance = (0..65)
                        .map(|n| SelectedNamedSource {
                            name: format!("g{n}"),
                            source: SelectedInstructionSource::Inline("g".into()),
                        })
                        .collect()
                }
                8 => selected.role = SelectedInstructionSource::File(vec!["roles".into(); 65]),
                9 => selected.profile_binding.definition_sha256 = "e".repeat(64),
                10 => selected.admission.requested_access = "arbitrary".into(),
                _ => selected.role = SelectedInstructionSource::Inline("x".repeat(65_537)),
            }
            assert!(
                prepare_provisioning(&scene.f.home, &owner, selected).is_err(),
                "source case{case}"
            );
            assert_eq!(scene.effects(), before);
        }
        for kind in ["staged", "dirty", "head", "commit", "range"] {
            let revision = match kind {
                "commit" => Some("HEAD"),
                "range" => Some("HEAD..HEAD"),
                _ => None,
            };
            let selected = scene.selection(kind, Some((kind, revision)), "structured_review_v4");
            assert!(prepare_assignment(&owner, selected).is_err());
            assert_eq!(scene.effects(), before);
        }
    }

    #[test]
    fn source_binding_closed_private_shapes_reject_unknown_missing_and_wrong_types() {
        fn check<T: serde::de::DeserializeOwned>(value: Value, required: &[&str]) {
            assert!(serde_json::from_value::<T>(value.clone()).is_ok());
            let mut unknown = value.clone();
            unknown["unexpected_private_field"] = json!("no authority");
            assert!(serde_json::from_value::<T>(unknown).is_err());
            for field in required {
                let mut missing = value.clone();
                missing.as_object_mut().unwrap().remove(*field);
                assert!(
                    serde_json::from_value::<T>(missing).is_err(),
                    "missing {field}"
                );
                let mut wrong = value.clone();
                wrong[*field] = json!(true);
                assert!(serde_json::from_value::<T>(wrong).is_err(), "type {field}");
            }
        }
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        check::<ProvisioningSelection>(
            serde_json::to_value(&scene.selected).unwrap(),
            &[
                "role_name",
                "role",
                "dependencies",
                "selected_guidance",
                "profile_binding",
                "admission",
            ],
        );
        check::<SelectedInstructionSource>(
            json!({"kind":"inline","value":"complete bytes"}),
            &["kind", "value"],
        );
        check::<SelectedInstructionSource>(
            json!({"kind":"file","value":["roles","reviewer"]}),
            &["kind", "value"],
        );
        check::<SelectedNamedSource>(
            json!({"name":"dependency","source":{"kind":"inline","value":"complete bytes"}}),
            &["name", "source"],
        );
        check::<ProvisioningAdmission>(
            serde_json::to_value(&scene.selected.admission).unwrap(),
            &[
                "model",
                "effort",
                "purpose_kind",
                "purpose_label",
                "required_capabilities",
                "execution_lane",
                "requested_access",
                "required_assurance",
                "native_subagent_policy",
            ],
        );
        check::<Component>(
            json!({"digest":"e".repeat(64),"bytes":1}),
            &["digest", "bytes"],
        );
        check::<Index>(
            json!({"record":{"digest":"e".repeat(64),"bytes":1}}),
            &["record"],
        );
        check::<ProjectionLookup>(
            json!({"kind":"provisioning","sources":scene.selected.capture(&scene.f.home).unwrap().sources_identity().unwrap()}),
            &["kind", "sources"],
        );
        let registry = Registry::open(&owner).unwrap();
        let record = registry
            .record(
                &index_name(
                    "p",
                    &scene
                        .selected
                        .capture(&scene.f.home)
                        .unwrap()
                        .sources_identity()
                        .unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        check::<Record>(
            serde_json::to_value(&record).unwrap(),
            &["partition", "prepared"],
        );
        check::<Prepared>(
            serde_json::to_value(&record.prepared).unwrap(),
            &["kind", "selection", "snapshot", "projection"],
        );
        let selected = scene.selection("shape-assignment", None, "structured_review_v3");
        prepare_assignment(&owner, selected.clone()).unwrap();
        let record = registry
            .record(&index_name("a", &selected.namespace).unwrap())
            .unwrap();
        check::<Prepared>(
            serde_json::to_value(record.prepared).unwrap(),
            &[
                "kind",
                "selection",
                "configuration",
                "projection",
                "manifest",
            ],
        );
    }

    #[test]
    fn source_binding_registered_selector_manifest_cannot_be_substituted() {
        let scene = Scene::new(false, HostRoute::Codex, true);
        let owner = scene.f.owner();
        let selected = scene.selection(
            "selector-proof",
            Some(("head", None)),
            "structured_review_v4",
        );
        prepare_assignment(&owner, selected.clone()).unwrap();
        let registry = Registry::open(&owner).unwrap();
        let name = index_name("a", &selected.namespace).unwrap();
        let mut record = registry.record(&name).unwrap();
        let mut forged = selected.clone();
        forged.target = Some(crate::review::ReviewTargetRequest {
            kind: "workspace".into(),
            revision: None,
        });
        let bytes = canonical(&forged).unwrap();
        let reference = Component::of(&bytes);
        registry
            .publish_bytes(&reference.name().unwrap(), &bytes)
            .unwrap();
        let Prepared::Assignment { selection, .. } = &mut record.prepared else {
            panic!()
        };
        *selection = reference;
        let bytes = canonical(&record).unwrap();
        let reference = Component::of(&bytes);
        registry
            .publish_bytes(&reference.name().unwrap(), &bytes)
            .unwrap();
        private(
            &registry.directory.path.join(&name),
            &canonical(&Index { record: reference }).unwrap(),
        );
        assert!(
            project(
                &owner,
                ProjectionLookup::Assignment {
                    namespace: selected.namespace
                }
            )
            .is_err(),
            "registered selector differs from complete retained manifest"
        );
        let sources = scene
            .selected
            .capture(&scene.f.home)
            .unwrap()
            .sources_identity()
            .unwrap();
        let name = index_name("p", &sources).unwrap();
        let mut record = registry.record(&name).unwrap();
        let mut selected = scene.selected.clone();
        selected.role_name = "other-role".into();
        let bytes = canonical(&selected).unwrap();
        let reference = Component::of(&bytes);
        registry
            .publish_bytes(&reference.name().unwrap(), &bytes)
            .unwrap();
        let Prepared::Provisioning { selection, .. } = &mut record.prepared else {
            panic!()
        };
        *selection = reference;
        let bytes = canonical(&record).unwrap();
        let reference = Component::of(&bytes);
        registry
            .publish_bytes(&reference.name().unwrap(), &bytes)
            .unwrap();
        private(
            &registry.directory.path.join(&name),
            &canonical(&Index { record: reference }).unwrap(),
        );
        assert!(
            project(
                &owner,
                ProjectionLookup::Provisioning {
                    sources: sources.into()
                }
            )
            .is_err(),
            "registered role provenance differs from its complete snapshot"
        );
    }

    #[test]
    fn source_binding_caller_and_preparation_scope_are_not_public_authority() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Codex, false);
            let owner = scene.f.owner();
            let selected = scene.selection("caller", None, "structured_review_v3");
            let expected = prepare_assignment(&owner, selected.clone()).unwrap();
            let before = scene.effects();
            let foreign = crate::caller_scope::fixture(4);
            let result = match scene.aggregate {
                SettlementOwner::External { engagement_id } => assign_external(
                    &owner,
                    &foreign,
                    &scene.port,
                    services::AssignExternalSpecialistTask {
                        engagement_id,
                        member_ref: scene.member,
                        task: selected.task.clone(),
                        execution_intent: "read_only",
                        target: None,
                        deadline_seconds: 600,
                        idempotency_key: "caller",
                        expected_revision: None,
                    },
                    expected,
                ),
                SettlementOwner::Managed { session_id } => assign_managed(
                    &owner,
                    &services::ManagedCallContext {
                        session_id,
                        source_run_id: session_id,
                        source_turn_id: "root-turn",
                        source_tool_call_id: "root-call",
                        idempotency_key: "caller",
                    },
                    &foreign,
                    &scene.port,
                    services::AssignSpecialistTask {
                        member_ref: scene.member,
                        task: selected.task.clone(),
                        execution_intent: "read_only",
                        target: None,
                        deadline_seconds: 600,
                        expected_revision: None,
                    },
                    expected,
                ),
            };
            assert!(
                result.is_err(),
                "caller placement differs from protected owner"
            );
            assert_eq!(scene.effects(), before);
        }
        let scene = Scene::new(false, HostRoute::Codex, false);
        let mut denied = Fixture::new(HostRoute::Codex);
        denied
            .draft
            .allowed_scopes
            .retain(|s| s.kind == "external_engagement");
        denied.complete();
        InstructionSourceStore::new(&denied.home)
            .initialize_templates()
            .unwrap();
        let owner = denied.owner();
        initialize(&owner).unwrap();
        assert!(
            prepare_provisioning(&denied.home, &owner, scene.selected.clone()).is_err(),
            "workspace preparation requires configured scope permission"
        );
    }

    #[test]
    fn source_binding_assignment_namespace_deadline_and_owner_fences() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Claude, false);
            let owner = scene.f.owner();
            let selected = scene.selection("namespace", None, "structured_review_v3");
            let before = scene.effects();
            let calls = scene.port.calls();
            for seconds in [0, 3601, u64::MAX] {
                let mut changed = selected.clone();
                changed.deadline_seconds = seconds;
                assert!(
                    prepare_assignment(&owner, changed).is_err(),
                    "deadline {seconds}"
                );
            }
            let namespace = serde_json::to_value(&selected.namespace).unwrap();
            for field in namespace.as_object().unwrap().keys() {
                let mut value = namespace.clone();
                value[field] = if field == "controller_generation" {
                    json!(selected.namespace.controller_generation + 1)
                } else {
                    json!("foreign")
                };
                // An idempotency key is the preparation's selected lookup, not foreign authority.
                if field == "idempotency_key" {
                    continue;
                }
                let mut changed = selected.clone();
                if let Ok(ns) = serde_json::from_value(value) {
                    changed.namespace = ns;
                    assert!(
                        prepare_assignment(&owner, changed).is_err(),
                        "namespace {field}"
                    );
                }
            }
            let reference = selected.namespace.scope_ref.as_ref().unwrap();
            for value in [
                reference.replace('-', ""),
                reference.to_uppercase(),
                format!("{{{reference}}}"),
                format!("urn:uuid:{reference}"),
                Uuid::nil().to_string(),
                "00000000-0000-4000-8000-000000000000".into(),
                "00000000-0000-7000-0000-000000000000".into(),
            ] {
                let mut changed = selected.clone();
                changed.namespace.scope_ref = Some(value.clone());
                assert!(
                    prepare_assignment(&owner, changed).is_err(),
                    "scope encoding {value}"
                );
            }
            assert_eq!(scene.effects(), before);
            assert_eq!(scene.port.calls(), calls);
            for seconds in [1, 3600] {
                let mut positive = selected.clone();
                positive.namespace.idempotency_key = format!("deadline-{seconds}");
                positive.deadline_seconds = seconds;
                let expected = prepare_assignment(&owner, positive.clone()).unwrap();
                assert_eq!(
                    project(
                        &owner,
                        ProjectionLookup::Assignment {
                            namespace: positive.namespace.clone()
                        }
                    )
                    .unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
            }
            assert_eq!(scene.effects(), before);
            assert_eq!(scene.port.calls(), calls);
        }
    }
    #[test]
    fn source_binding_projection_lookup_cannot_alias_another_registration() {
        let scene = Scene::new(false, HostRoute::Codex, false);
        let owner = scene.f.owner();
        let registry = Registry::open(&owner).unwrap();
        let selected = scene.selection("projection-original", None, "structured_review_v3");
        prepare_assignment(&owner, selected.clone()).unwrap();
        let original = index_name("a", &selected.namespace).unwrap();
        let index = fs::read(registry.directory.path.join(&original)).unwrap();
        let mut foreign = selected.namespace.clone();
        foreign.idempotency_key = "projection-alias".into();
        let alias = index_name("a", &foreign).unwrap();
        private(&registry.directory.path.join(&alias), &index);
        let before = scene.effects();
        assert!(
            project(&owner, ProjectionLookup::Assignment { namespace: foreign }).is_err(),
            "assignment lookup aliases another key"
        );
        let mut sources = scene
            .selected
            .capture(&scene.f.home)
            .unwrap()
            .sources_identity()
            .unwrap();
        let original = index_name("p", &sources).unwrap();
        let index = fs::read(registry.directory.path.join(&original)).unwrap();
        let mut alternate = scene.selected.clone();
        alternate.role = SelectedInstructionSource::Inline("a valid different role".into());
        sources = alternate
            .capture(&scene.f.home)
            .unwrap()
            .sources_identity()
            .unwrap();
        let alias = index_name("p", &sources).unwrap();
        private(&registry.directory.path.join(&alias), &index);
        assert!(
            project(
                &owner,
                ProjectionLookup::Provisioning {
                    sources: Box::new(sources.clone())
                }
            )
            .is_err(),
            "provisioning lookup aliases another identity"
        );
        assert!(
            fresh_provisioning(&scene.f.home, &owner, &sources).is_err(),
            "fresh provisioning lookup aliases another identity"
        );
        assert_eq!(scene.effects(), before);
    }

    #[test]
    fn source_binding_preparation_configuration_requires_actual_owned_binding() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Codex, false);
            let owner = scene.f.owner();
            let original =
                services::preparation_configuration(&scope(&owner), scene.aggregate, scene.member)
                    .unwrap();
            let before = scene.effects();
            let calls = scene.port.calls();
            for field in ["workspace", "controller"] {
                let mut view = owner.view.clone();
                let mut controller = owner.controller.clone();
                if field == "workspace" {
                    view.workspace_id.push_str("-foreign");
                } else {
                    controller.identity.generation += 1;
                }
                let foreign = DelegationScope {
                    view: &view,
                    state_root: &owner.state_root,
                    controller: &controller,
                    trusted_invocation: None,
                };
                assert!(
                    services::preparation_configuration(&foreign, scene.aggregate, scene.member)
                        .is_err(),
                    "preparation {managed}/{field}"
                );
            }
            assert!(
                services::preparation_configuration(
                    &scope(&owner),
                    scene.aggregate,
                    Uuid::now_v7()
                )
                .is_err()
            );
            assert_eq!(
                services::preparation_configuration(&scope(&owner), scene.aggregate, scene.member)
                    .unwrap(),
                original
            );
            assert_eq!(scene.effects(), before);
            assert_eq!(scene.port.calls(), calls);
        }
    }

    #[test]
    fn source_binding_cold_service_child() {
        let Some(home) = std::env::var_os("DOLGORAE_SOURCE_BINDING_CHILD") else {
            return;
        };
        let home = DolgoraeHome::from_canonical_home(PathBuf::from(home)).unwrap();
        let path = PathBuf::from(std::env::var_os("DOLGORAE_SOURCE_BINDING_OWNER").unwrap());
        let owner = VerifiedOwner::load(&home, &path).unwrap();
        let input = PrivateDirectory::open(&owner.root.path).unwrap();
        let selected: AssignmentSelection = input
            .read_bounded(
                &owner.root.path.join("child-request.json"),
                ASSIGNMENT_BYTES,
            )
            .unwrap();
        let expected: AcceptedTask = input
            .read_bounded(&owner.root.path.join("child-identity.json"), PUBLIC_BYTES)
            .unwrap();
        let caller = crate::caller_scope::TrustedCallerScope::from_port(
            &crate::trusted_host::ConfiguredCallerPort(&owner),
        )
        .unwrap();
        let port = services::tests::FakePort::new();
        let result = match owner.configuration.owner {
            OwnerKind::External => assign_external(
                &owner,
                &caller,
                &port,
                services::AssignExternalSpecialistTask {
                    engagement_id: Uuid::try_parse(selected.namespace.scope_ref.as_ref().unwrap())
                        .unwrap(),
                    member_ref: selected.member_ref,
                    task: selected.task,
                    execution_intent: &selected.execution_intent,
                    target: None,
                    deadline_seconds: selected.deadline_seconds,
                    idempotency_key: &selected.namespace.idempotency_key,
                    expected_revision: None,
                },
                expected,
            ),
            OwnerKind::Managed { session_ref } => assign_managed(
                &owner,
                &services::ManagedCallContext {
                    session_id: session_ref,
                    source_run_id: session_ref,
                    source_turn_id: "root-turn",
                    source_tool_call_id: "root-call",
                    idempotency_key: &selected.namespace.idempotency_key,
                },
                &caller,
                &port,
                services::AssignSpecialistTask {
                    member_ref: selected.member_ref,
                    task: selected.task,
                    execution_intent: &selected.execution_intent,
                    target: None,
                    deadline_seconds: selected.deadline_seconds,
                    expected_revision: None,
                },
                expected,
            ),
        }
        .unwrap();
        let output = PathBuf::from(std::env::var_os("DOLGORAE_SOURCE_BINDING_RESULT").unwrap());
        private(
            &output,
            &canonical(&json!({"receipt":result.1,"dispatches":port.calls()})).unwrap(),
        );
    }
    #[test]
    fn source_binding_cold_acceptance_lost_output_and_replay_preserve_original_receipt() {
        for managed in [false, true] {
            let scene = Scene::new(managed, HostRoute::Codex, true);
            let owner = scene.f.owner();
            let selected = scene.selection("cold", Some(("head", None)), "structured_review_v4");
            let expected = prepare_assignment(&owner, selected.clone()).unwrap();
            private(
                &owner.root.path.join("child-request.json"),
                &canonical(&selected).unwrap(),
            );
            private(
                &owner.root.path.join("child-identity.json"),
                &canonical(&expected).unwrap(),
            );
            let run = |output: &Path| {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "source_binding::tests::source_binding_cold_service_child",
                        "--nocapture",
                    ])
                    .env("DOLGORAE_SOURCE_BINDING_CHILD", &scene.f.root)
                    .env("DOLGORAE_SOURCE_BINDING_OWNER", scene.f.path())
                    .env("DOLGORAE_SOURCE_BINDING_RESULT", output)
                    .output()
                    .unwrap();
                assert!(
                    status.status.success(),
                    "{}",
                    String::from_utf8_lossy(&status.stdout)
                );
                serde_json::from_slice::<Value>(&fs::read(output).unwrap()).unwrap()
            };
            let first_path = scene.f.root.join("first.json");
            let first = run(&first_path);
            assert_eq!(first["dispatches"], 1);
            fs::remove_file(first_path).unwrap();
            fs::remove_dir_all(owner.root.path.join(DIRECTORY)).unwrap();
            let replay = run(&scene.f.root.join("replay.json"));
            assert_eq!(replay["dispatches"], 0);
            assert_eq!(replay["receipt"], first["receipt"]);
        }
    }
    #[test]
    fn native_validate_install_hire_keeps_authoritative_candidate_facts_and_replays() {
        use crate::native_consumer::{EXTERNAL_CONTROL, EXTERNAL_NATIVE};
        use crate::specialist_facade as facade;
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let scene = Scene::new_with_registration(false, route, false, false, None, true);
            let owner = scene.f.owner();
            crate::native_consumer::initialize(&owner, crate::native_consumer::Mode::NativeOnly {})
                .unwrap();
            let projection =
                prepare_provisioning(&scene.f.home, &owner, scene.selected.clone()).unwrap();
            let deliver = |call: &str, tool: &str, request: &Value| {
                let (_, cap) = {
                    let store = scene.f.ready();
                    store
                        .bind_native(
                            crate::host_invocation::NativeEvent {
                                session: "native-thread".into(),
                                turn: Some("native-turn".into()),
                                tool_use: call.into(),
                                server: "dolgorae".into(),
                                server_source: (route == HostRoute::Claude)
                                    .then(|| "dynamic".into()),
                                tool: tool.into(),
                            },
                            request.clone(),
                        )
                        .unwrap()
                };
                facade::external_with_port(
                    &scene.f.home,
                    &scene.f.path(),
                    tool,
                    if route == HostRoute::Codex {
                        facade::NativeCarrier::Codex {
                            session: "native-thread",
                            call,
                        }
                    } else {
                        facade::NativeCarrier::Claude(&cap)
                    },
                    request,
                    &scene.port,
                )
            };
            let validate = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"validate_provisioning","sources":projection});
            let result = deliver("validate", EXTERNAL_NATIVE, &validate);
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["facts"]["candidate"]["installed"], false);
            let candidate = result["facts"]["candidate"].clone();
            assert!(
                candidate["agent_configuration_digest"]
                    .as_str()
                    .unwrap()
                    .starts_with("sha256:")
            );
            let install = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"install_provisioning","sources":projection,"idempotency_key":"native-candidate-install","expected_revision":null});
            let installed = deliver("install", EXTERNAL_NATIVE, &install);
            assert_eq!(installed["ok"], true, "{installed}");
            let facts = &installed["facts"]["candidate"];
            for key in [
                "provisioning_ref",
                "role_snapshot_digest",
                "instruction_identity_digest",
                "agent_configuration_digest",
                "profile_binding_digest",
                "source_bytes",
            ] {
                assert_eq!(facts[key], candidate[key], "{key}");
            }
            assert_eq!(facts["installed"], true);
            let hire = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"hire_external_specialist","scope_kind":"external_engagement","scope_ref":scene.aggregate.scope_ref(),"provisioning_ref":facts["provisioning_ref"],"role_ref":"reviewer","agent_configuration_digest":facts["agent_configuration_digest"],"idempotency_key":"native-candidate-hire","expected_revision":null});
            let hired = deliver("hire", EXTERNAL_NATIVE, &hire);
            assert_eq!(hired["ok"], true, "{hired}");
            assert!(hired["facts"]["member"]["member_ref"].is_string());
            assert_eq!(
                hired["facts"]["member"]["agent_configuration_digest"],
                facts["agent_configuration_digest"]
            );
            let effects = scene.effects();
            let calls = scene.port.calls();
            let replay = deliver("hire", EXTERNAL_NATIVE, &hire);
            assert_eq!(replay, hired);
            assert_eq!(scene.effects(), effects);
            assert_eq!(scene.port.calls(), calls);
            let status = json!({"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"context"}});
            let observed = deliver("control", EXTERNAL_CONTROL, &status);
            assert_eq!(observed["ok"], true, "{observed}");
            assert_eq!(observed["data"]["mode"], "native_only");
            if let Some(binary) = std::env::var_os("DOLGORAE_NATIVE_PRESENTATION_BINARY") {
                let output = std::process::Command::new(binary)
                    .args([
                        "__trusted-specialist-host",
                        "host-lookup",
                        "--owner-config",
                        scene.f.path().to_str().unwrap(),
                        "--session",
                        "native-thread",
                        "--call",
                        "hire",
                    ])
                    .env("HOME", scene.f.home.root().parent().unwrap())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let recovered: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(recovered["ok"], true, "{recovered}");
                assert_eq!(recovered["facts"]["lookup"]["state"], "accepted");
                assert_eq!(scene.effects(), effects);
            }
        }
    }
    #[test]
    fn native_assignment_replay_precedes_current_consumer_admission_for_both_owners() {
        use std::cell::{Cell, RefCell};

        for managed in [false, true] {
            let scene = consumer_scene(managed);
            let owner = scene.f.owner();
            let connection = consumer_connection(&scene);
            let client = RefCell::new(ScriptedPodway::new(&scene, &connection));
            crate::native_consumer::initialize(
                &owner,
                crate::native_consumer::Mode::Delegated {
                    connection: Box::new(connection),
                },
            )
            .unwrap();
            let bridge = consumer_bridge(&scene, &owner);
            let mut selected = scene.selection("native-admission", None, "complete_result");
            selected
                .task
                .contexts
                .push(crate::task_request::TaskContext {
                    id: "work-package".into(),
                    content: serde_json::to_string(&client.borrow().package["package"]).unwrap(),
                    provenance: "Selected public delegation package".into(),
                });
            let expected = prepare_assignment(&owner, selected.clone()).unwrap();
            let operation = selected.namespace.operation_kind.as_str();
            let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":operation,
                "scope_kind":scene.aggregate.scope_kind(),"scope_ref":scene.aggregate.scope_ref(),
                "member_ref":scene.member,"assignment":{"task_body":selected.task,
                "accepted_task_identity":expected,"deadline_seconds":selected.deadline_seconds},
                "idempotency_key":selected.namespace.idempotency_key,"expected_revision":null});
            let root = scene.aggregate.scope_ref();
            let context = services::ManagedCallContext {
                session_id: root,
                source_run_id: root,
                source_turn_id: "root-turn",
                source_tool_call_id: "root-call",
                idempotency_key: &selected.namespace.idempotency_key,
            };
            let checks = Cell::new(0);
            let admission = || {
                checks.set(checks.get() + 1);
                bridge.assignment_binding(&mut *client.borrow_mut(), &request)
            };
            let dispatch = |request: &Value| {
                crate::specialist_facade::dispatch(
                    &scene.f.home,
                    &owner,
                    &scope(&owner),
                    managed.then_some(&context),
                    &scene.port,
                    request,
                    Some(&admission),
                )
            };
            let before = scene.effects();
            client.borrow_mut().evidence_fault = Some(EvidenceFault::Unavailable);
            assert!(dispatch(&request).is_err());
            assert_eq!(checks.get(), 1);
            assert_eq!(scene.effects(), before, "fresh refusal created an effect");

            client.borrow_mut().evidence_fault = None;
            let first = dispatch(&request).unwrap();
            assert_eq!(checks.get(), 2);
            let accepted = scene.effects();
            let calls = scene.port.calls();
            client.borrow_mut().observation["status"]["session"]["lifecycle"] = json!("completed");
            client.borrow_mut().evidence_fault = Some(EvidenceFault::Unavailable);
            assert!(
                bridge
                    .assignment_binding(&mut *client.borrow_mut(), &request)
                    .is_err()
            );
            fs::remove_dir_all(owner.root.path.join(DIRECTORY)).unwrap();

            let replay = dispatch(&request).unwrap();
            assert_eq!(replay, first);
            assert_eq!(checks.get(), 2, "accepted replay read current Podway state");
            assert_eq!(scene.effects(), accepted);
            assert_eq!(scene.port.calls(), calls);
            let mut conflict = request.clone();
            conflict["assignment"]["task_body"]["contexts"][0]["content"] =
                json!("changed task context");
            assert_eq!(
                dispatch(&conflict).unwrap_err().code,
                "IDEMPOTENCY_CONFLICT"
            );
            assert_eq!(checks.get(), 2);
            assert_eq!(scene.effects(), accepted);
            assert_eq!(scene.port.calls(), calls);
        }
    }

    #[test]
    fn native_collect_selects_real_complete_content_for_both_owners_without_podway() {
        struct NoConsumer;
        impl crate::podway_client::Client for NoConsumer {
            fn invoke(
                &mut self,
                _: &crate::native_consumer::PodwayConnection,
                _: crate::podway_client::Invocation,
                _: &[u8],
                _: &mut dyn FnMut(u32) -> Result<(), MachineError>,
            ) -> Result<crate::podway_client::Reply, MachineError> {
                panic!("native-only collect invoked a consumer")
            }
        }
        for (managed, v3) in [(false, false), (true, false), (false, true), (true, true)] {
            let scene = consumer_scene(managed);
            let owner = scene.f.owner();
            let connection = consumer_connection(&scene);
            let package = ScriptedPodway::new(&scene, &connection).package;
            let task = consumer_retained_task_output(&scene, &package, v3);
            crate::native_consumer::initialize(&owner, crate::native_consumer::Mode::NativeOnly {})
                .unwrap();
            let bridge = consumer_bridge(&scene, &owner);
            let op = if managed {
                "collect_specialist_task"
            } else {
                "collect_external_specialist_task"
            };
            let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":op,"scope_kind":scene.aggregate.scope_kind(),"scope_ref":scene.aggregate.scope_ref(),"member_ref":scene.member,"task_ref":task});
            let collect = |request: &Value| {
                let result = crate::specialist_facade::dispatch(
                    &scene.f.home,
                    &owner,
                    &scope(&owner),
                    bridge.source.managed.as_ref(),
                    &scene.port,
                    request,
                    None,
                );
                let public = crate::specialist_facade::finish(op, result, true);
                assert!(
                    crate::specialist_facade::validates(&public, &format!("{op}_response")),
                    "{public}"
                );
                public
            };
            let public = collect(&request);
            assert!(public["task"]["complete_result"].is_object(), "{public}");
            let presented = bridge.semantic(&mut NoConsumer, &request, &public);
            assert_eq!(presented["ok"], true, "{presented}");
            assert_eq!(
                presented["facts"]["task"]["content_availability"],
                public["task"]["content_availability"]
            );
            assert!(presented["facts"]["task"].get("complete_result").is_none());
            assert!(presented["evidence"]["selection_ref"].is_string());
            let inspect = consumer_control(
                json!({"action":"inspect","selection_ref":presented["evidence"]["selection_ref"],"view":{"kind":"final_response"},"range":{"kind":"start","offset":0}}),
            );
            let original = bridge.handle(&mut NoConsumer, &inspect);
            assert_eq!(original["ok"], true, "{original}");
            let content = crate::consumer_bridge::native_content(&original, false).unwrap();
            assert!(!content[1]["text"].as_str().unwrap().is_empty());
            if v3 {
                let read = bridge.handle(&mut NoConsumer, &consumer_control(json!({"action":"inspect","selection_ref":presented["evidence"]["selection_ref"],"view":{"kind":"structured_output"},"range":{"kind":"start","offset":0}})));
                assert_eq!(read["ok"], true, "{read}");
                assert_eq!(
                    read["data"]["source_digest"],
                    public["task"]["complete_result"]["structured_output"]["sha256"]
                );
                let text = crate::consumer_bridge::native_content(&read, false).unwrap()[1]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                assert_eq!(
                    serde_json::from_str::<Value>(&text).unwrap()["summary"],
                    "Original review 界"
                );
            }
            let proof_selection = bridge
                .source
                .select(
                    &bridge.store,
                    &scene.aggregate.scope_ref().to_string(),
                    scene.member,
                    task,
                )
                .unwrap();
            let next = scene.selection("later-live-work", None, "complete_result");
            let expected = prepare_assignment(&owner, next.clone()).unwrap();
            let (running, _) = scene.assign(&next, expected).unwrap();
            assert_eq!(
                bridge
                    .source
                    .select(
                        &bridge.store,
                        &scene.aggregate.scope_ref().to_string(),
                        scene.member,
                        task
                    )
                    .unwrap_err()
                    .code,
                "DISPOSITION_NOT_IDLE"
            );
            let during_work = collect(&request);
            let presented_busy = bridge.semantic(&mut NoConsumer, &request, &during_work);
            assert_eq!(presented_busy["ok"], true, "{presented_busy}");
            for reference in [
                &presented["evidence"]["selection_ref"],
                &json!(proof_selection),
                &presented_busy["evidence"]["selection_ref"],
            ] {
                let read = bridge.handle(&mut NoConsumer, &consumer_control(json!({"action":"inspect","selection_ref":reference,"view":{"kind":"final_response"},"range":{"kind":"start","offset":0}})));
                assert_eq!(read["ok"], true, "{read}");
                assert_eq!(
                    crate::consumer_bridge::native_content(&read, false).unwrap()[1],
                    content[1]
                );
                assert_eq!(
                    read["data"]["source_digest"],
                    original["data"]["source_digest"]
                );
            }
            let mut pending_request = request.clone();
            pending_request["task_ref"] = json!(running.task_ref);
            let no_result = collect(&pending_request);
            assert_eq!(no_result["task"]["content_availability"], "pending");
            let absent = bridge.semantic(&mut NoConsumer, &pending_request, &no_result);
            assert_eq!(absent["ok"], true);
            assert!(absent["evidence"].is_null());
            assert_eq!(absent["facts"]["task"]["required_action"], "await_task");
        }
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::semantic::successor_fixture::{AcceptingPort, ManagedMember, SuccessorMember};
    use std::cell::Cell;

    struct Source {
        expected: AcceptedTask,
        selection: crate::review::ReviewTargetRequest,
        resolves: Cell<usize>,
        verifies: Cell<usize>,
    }
    impl AssignmentSource for Source {
        fn expected(&self) -> &AcceptedTask {
            &self.expected
        }
        fn selection(&self) -> Result<Option<crate::review::ReviewTargetRequest>, MachineError> {
            self.resolves.set(self.resolves.get() + 1);
            Ok(Some(self.selection.clone()))
        }
        fn verify_capture(
            &self,
            accepted: &AcceptedTask,
            _: &crate::successor_capture::AcceptedTaskContents,
        ) -> Result<(), MachineError> {
            self.verifies.set(self.verifies.get() + 1);
            if *accepted != self.expected {
                return Err(conflict());
            }
            Ok(())
        }
    }
    fn prepared_source(
        view: &crate::workspace::WorkspaceView,
        configuration: &crate::run::AgentConfigurationSnapshot,
        task: &crate::task_request::SpecialistTaskRequest,
    ) -> Source {
        let selection = crate::review::ReviewTargetRequest {
            kind: "head".into(),
            revision: None,
        };
        let target = crate::direct_review_target::DirectTarget::admit(
            &view.canonical_path.to_path_buf().unwrap(),
            view.mode,
            &selection.kind,
            None,
            Uuid::now_v7(),
        )
        .unwrap();
        let bytes = target.metadata_bytes().unwrap();
        let expected = crate::successor_capture::capture_facade_task(
            configuration,
            task,
            "read_only",
            Some((&target.target_sha256, &bytes)),
        )
        .unwrap()
        .0;
        Source {
            expected,
            selection,
            resolves: Cell::new(0),
            verifies: Cell::new(0),
        }
    }
    #[test]
    fn source_binding_current_precedes_all_live_assignment_capture_for_both_owners() {
        for old in [false, true] {
            for output in ["plain_text", "structured_review_v3", "structured_review_v4"] {
                let external = if old {
                    SuccessorMember::historical()
                } else {
                    SuccessorMember::reviewer()
                };
                crate::engagement::ensure_delegation_namespace(&external.state_root).unwrap();
                let scope = DelegationScope {
                    view: &external.view,
                    state_root: &external.state_root,
                    controller: &external.owner_binding,
                    trusted_invocation: None,
                };
                let mut task = services::fixture_task();
                task.expected_output = output.into();
                let source = prepared_source(
                    &external.view,
                    &external.manifest.agent_configuration,
                    &task,
                );
                assert!(services::qualified_successor_review(&external.manifest));
                services::source_trace();
                let result = services::assign_external_with_source(
                    &scope,
                    &crate::caller_scope::fixture(3),
                    &AcceptingPort,
                    services::AssignExternalSpecialistTask {
                        engagement_id: external.engagement_id,
                        member_ref: external.member_ref,
                        task,
                        execution_intent: "read_only",
                        target: None,
                        deadline_seconds: 600,
                        idempotency_key: "order",
                        expected_revision: None,
                    },
                    &source,
                );
                let trace = services::source_trace();
                if old {
                    assert_eq!(result.unwrap_err().code, "CAPABILITY_UNSUPPORTED");
                    assert_eq!(
                        source.resolves.get(),
                        0,
                        "old binding resolved a live selector"
                    );
                    assert_eq!(source.verifies.get(), 0);
                    assert_eq!(trace, ["current"]);
                    assert_eq!(external.rounds_used(), 0);
                } else {
                    assert!(result.is_ok(), "{output}: {result:?}");
                    assert_eq!(trace, ["current", "target", "task"]);
                    assert_eq!(source.resolves.get(), 1);
                    assert_eq!(source.verifies.get(), 1);
                }
                let managed = if old {
                    ManagedMember::historical()
                } else {
                    ManagedMember::new()
                };
                crate::engagement::ensure_delegation_namespace(&managed.state_root).unwrap();
                let controller = external.owner_binding.clone();
                let root =
                    services::tests::root_manifest(&managed.view, &controller, managed.session_id);
                let placeholder = crate::run::run_root(&managed.state_root, managed.session_id);
                assert!(!placeholder.join("manifest.json").exists());
                std::fs::remove_dir_all(placeholder).unwrap();
                crate::run::RunStore::new(
                    crate::workspace::SystemWorkspacePlatform,
                    &managed.state_root,
                )
                .publish(&root)
                .unwrap();
                let scope = DelegationScope {
                    view: &managed.view,
                    state_root: &managed.state_root,
                    controller: &controller,
                    trusted_invocation: None,
                };
                services::tests::record_fixture_spawn(
                    &scope,
                    managed.session_id,
                    managed.member_ref,
                );
                let mut task = services::fixture_task();
                task.expected_output = output.into();
                let source =
                    prepared_source(&managed.view, &managed.manifest.agent_configuration, &task);
                assert!(services::qualified_successor_review(&managed.manifest));
                services::source_trace();
                let context = services::ManagedCallContext {
                    session_id: managed.session_id,
                    source_run_id: managed.session_id,
                    source_turn_id: "root-turn",
                    source_tool_call_id: "root-call",
                    idempotency_key: "order",
                };
                let result = services::assign_managed_with_source(
                    &scope,
                    &context,
                    &crate::caller_scope::fixture(3),
                    &AcceptingPort,
                    services::AssignSpecialistTask {
                        member_ref: managed.member_ref,
                        task,
                        execution_intent: "read_only",
                        target: None,
                        deadline_seconds: 600,
                        expected_revision: None,
                    },
                    &source,
                );
                let trace = services::source_trace();
                if old {
                    assert_eq!(result.unwrap_err().code, "CAPABILITY_UNSUPPORTED");
                    assert_eq!(
                        source.resolves.get(),
                        0,
                        "old binding resolved a live selector"
                    );
                    assert_eq!(source.verifies.get(), 0);
                    assert_eq!(trace, ["current"]);
                } else {
                    assert!(result.is_ok(), "{output}: {result:?}");
                    assert_eq!(trace, ["current", "target", "task"]);
                    assert_eq!(source.resolves.get(), 1);
                    assert_eq!(source.verifies.get(), 1);
                }
            }
        }
    }
}
