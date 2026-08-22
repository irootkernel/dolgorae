//! Controller and local-operator credential authority.
//!
//! Secret material exists only in zeroizing buffers owned by an already-open
//! carrier. Public identities and persisted state deliberately omit it.

use crate::darwin::DarwinSystem;
use crate::domain::{ControllerIdentity, ControllerKind, Purpose, RunLifecycle};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{LedgerClock as _, SystemLedgerClock};
use crate::machine::{MachineError, new_uuid_v7};
use crate::projection::{ProjectedWriterAuthority, RunStateProjection};
use crate::run::{ControllerBinding, ParentReference, RunStore, controller_capability_digest};
use crate::workspace::{SystemWorkspacePlatform, WorkspaceService};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::RawFd;
use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

const CREDENTIAL_MAX_BYTES: u64 = 4096;
const OPERATOR_DOMAIN: &[u8] = b"dolgorae.operator-capability.v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationLaunch {
    pub use_case: String,
    pub specialist_policy_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ControllerPublic {
    pub controller_id: Uuid,
    pub kind: ControllerKind,
    pub instance_id: String,
    pub subject_id: Option<String>,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperatorPublic {
    pub operator_id: Uuid,
    pub operator_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CredentialCreated<T> {
    pub credential: T,
    pub output_path: PathBuf,
    pub fingerprint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControllerWire {
    schema_version: u32,
    controller_id: Uuid,
    kind: ControllerKind,
    instance_id: String,
    subject_id: Option<String>,
    capability: String,
    orchestration_launch: Option<OrchestrationLaunch>,
}

impl Drop for ControllerWire {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

#[derive(Serialize)]
struct ControllerWireRef<'a> {
    schema_version: u32,
    controller_id: Uuid,
    kind: ControllerKind,
    instance_id: &'a str,
    subject_id: Option<&'a str>,
    capability: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    orchestration_launch: Option<&'a OrchestrationLaunch>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorWire {
    schema_version: u32,
    operator_id: Uuid,
    capability: String,
}

impl Drop for OperatorWire {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

struct ZeroizingJson(crate::jcs::LosslessJson);

impl Drop for ZeroizingJson {
    fn drop(&mut self) {
        fn clear(value: &mut crate::jcs::LosslessJson) {
            match value {
                crate::jcs::LosslessJson::Number(number)
                | crate::jcs::LosslessJson::String(number) => number.zeroize(),
                crate::jcs::LosslessJson::Array(values) => values.iter_mut().for_each(clear),
                crate::jcs::LosslessJson::Object(entries) => {
                    for (key, value) in entries {
                        key.zeroize();
                        clear(value);
                    }
                }
                crate::jcs::LosslessJson::Null | crate::jcs::LosslessJson::Bool(_) => {}
            }
        }
        clear(&mut self.0);
    }
}

#[derive(Serialize)]
struct OperatorWireRef<'a> {
    schema_version: u32,
    operator_id: Uuid,
    capability: &'a str,
}

struct ControllerSecret {
    public: ControllerPublic,
    capability: Zeroizing<[u8; 32]>,
}

struct OperatorSecret {
    operator_id: Uuid,
    capability: Zeroizing<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

pub struct CredentialCarrier {
    file: File,
    snapshot: FileSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialOperation {
    ControllerCreate,
    OperatorInitialize,
    OperatorRotate,
    RunVerify,
    RunReset,
}

pub fn execute(
    operation: CredentialOperation,
    arguments: &[std::ffi::OsString],
) -> Result<serde_json::Value, MachineError> {
    match operation {
        CredentialOperation::ControllerCreate => {
            let kind = parse_controller_kind(&required_utf8(arguments, "--kind")?)?;
            let instance_id = required_utf8(arguments, "--instance-id")?;
            let subject_id = optional_utf8(arguments, "--subject-id")?;
            let policy = optional_utf8(arguments, "--orchestration-policy")?;
            let output = PathBuf::from(required_os(arguments, "--output")?);
            let created =
                create_controller_credential(&output, kind, instance_id, subject_id, policy)?;
            Ok(serde_json::json!({
                "controller": created.credential,
                "output_path": created.output_path,
                "fingerprint": created.fingerprint,
            }))
        }
        CredentialOperation::OperatorInitialize => {
            let output = PathBuf::from(required_os(arguments, "--output")?);
            let created = OperatorStore::new(default_operator_root()?).initialize(&output)?;
            Ok(serde_json::json!({
                "operator_id": created.credential.operator_id,
                "operator_generation": created.credential.operator_generation,
                "output_path": created.output_path,
                "fingerprint": created.fingerprint,
            }))
        }
        CredentialOperation::OperatorRotate => {
            let output = PathBuf::from(required_os(arguments, "--output")?);
            let carrier = carrier_from_options(arguments, "--operator-file", "--operator-fd")?;
            let created = OperatorStore::new(default_operator_root()?).rotate(&carrier, &output)?;
            Ok(serde_json::json!({
                "operator_id": created.credential.operator_id,
                "operator_generation": created.credential.operator_generation,
                "output_path": created.output_path,
                "fingerprint": created.fingerprint,
            }))
        }
        CredentialOperation::RunVerify => verify_run(arguments),
        CredentialOperation::RunReset => reset_run(arguments),
    }
}

fn verify_run(arguments: &[std::ffi::OsString]) -> Result<serde_json::Value, MachineError> {
    let run_id = positional(arguments, 0)?
        .to_str()
        .ok_or_else(|| MachineError::invalid_argument("run-id", "run id must be UTF-8"))?
        .parse::<Uuid>()
        .map_err(|_| MachineError::invalid_argument("run-id", "run id must be a UUID"))?;
    let workspace = optional_os(arguments, "--workspace")?.map(PathBuf::from);
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let application_support = default_operator_root()?
        .parent()
        .expect("operator root has Application Support parent")
        .to_path_buf();
    let store = RunStore::new(
        SystemWorkspacePlatform,
        application_support
            .join("workspaces")
            .join(&view.workspace_id),
    );
    let binding = store.load_controller_binding(run_id)?;
    let carrier = carrier_from_options(arguments, "--controller-file", "--controller-fd")?;
    let controller = authorize_controller(&binding, &carrier)?;
    Ok(serde_json::json!({
        "run_id": run_id,
        "controller_id": controller.controller_id,
        "generation": controller.generation,
        "kind": controller.kind,
        "verified_at": SystemLedgerClock::default().timestamp(),
    }))
}

fn reset_run(arguments: &[std::ffi::OsString]) -> Result<serde_json::Value, MachineError> {
    let run_id = positional(arguments, 0)?
        .to_str()
        .ok_or_else(|| MachineError::invalid_argument("run-id", "run id must be UTF-8"))?
        .parse::<Uuid>()
        .map_err(|_| MachineError::invalid_argument("run-id", "run id must be a UUID"))?;
    let confirmation = required_utf8(arguments, "--confirm")?
        .parse::<Uuid>()
        .map_err(|_| MachineError::invalid_argument("--confirm", "confirmation must be a UUID"))?;
    let workspace = optional_os(arguments, "--workspace")?.map(PathBuf::from);
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let application_support = default_operator_root()?
        .parent()
        .expect("operator root has Application Support parent")
        .to_path_buf();
    let state_root = application_support
        .join("workspaces")
        .join(&view.workspace_id);
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let binding = store.load_controller_binding(run_id)?;
    let run_root = state_root.join("runs").join(run_id.to_string());
    let projection = load_reset_projection(&run_root.join("state.json"), run_id)?;
    let (writer_authority, handoff_active, generation_verifiable) =
        match projection.writer_authority {
            ProjectedWriterAuthority::None => (false, false, true),
            ProjectedWriterAuthority::Active => (true, false, true),
            ProjectedWriterAuthority::HandoffPrepared => (true, true, true),
            ProjectedWriterAuthority::Reserved
            | ProjectedWriterAuthority::Releasing
            | ProjectedWriterAuthority::BlockedUnknown => (true, false, false),
        };
    let operator = carrier_from_options(arguments, "--operator-file", "--operator-fd")?;
    let replacement =
        carrier_from_options(arguments, "--new-controller-file", "--new-controller-fd")?;
    let mut state = ControllerResetState {
        run_id,
        binding: binding.clone(),
        state_revision: projection.ledger_head.sequence,
        lifecycle: projection.lifecycle,
        pending_interaction: !projection.pending_requests.is_empty()
            || projection.active_turn_id.is_some(),
        handoff_active,
        writer_authority,
        generation_verifiable,
    };
    let mut journal = DurableResetJournal::new(&run_root, binding);
    let record = reset_controller(
        &mut state,
        confirmation,
        &OperatorStore::new(default_operator_root()?),
        &operator,
        &replacement,
        &mut journal,
        || Ok(()),
    )?;
    Ok(serde_json::json!({
        "run_id": record.run_id,
        "controller_id": record.new_controller_id,
        "generation": record.new_generation,
        "writer_released": record.writer_released,
        "operation_id": record.operation_id,
    }))
}

fn load_reset_projection(path: &Path, run_id: Uuid) -> Result<RunStateProjection, MachineError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| reset_not_allowed())?;
    let metadata = file.metadata().map_err(|_| reset_not_allowed())?;
    if !metadata.is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > crate::jcs::RAW_PAYLOAD_LIMIT as u64
    {
        return Err(reset_not_allowed());
    }
    let mut bytes = Vec::new();
    (&file)
        .read_to_end(&mut bytes)
        .map_err(|_| reset_not_allowed())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| reset_not_allowed())?;
    let canonical = canonicalize(&parse(text).map_err(|_| reset_not_allowed())?)
        .map_err(|_| reset_not_allowed())?;
    let projection: RunStateProjection =
        serde_json::from_slice(&canonical).map_err(|_| reset_not_allowed())?;
    if projection.run_id != run_id {
        return Err(reset_not_allowed());
    }
    Ok(projection)
}

fn reset_not_allowed() -> MachineError {
    MachineError::new(
        "CONTROLLER_RESET_NOT_ALLOWED",
        "controller reset is not allowed in the current state",
        false,
        serde_json::json!({}),
    )
}

struct DurableResetJournal {
    journal_path: PathBuf,
    controller_path: PathBuf,
    previous: ControllerBinding,
}

impl DurableResetJournal {
    fn new(run_root: &Path, previous: ControllerBinding) -> Self {
        Self {
            journal_path: run_root.join("recovery/controller-reset.jsonl"),
            controller_path: run_root.join("controller.json"),
            previous,
        }
    }

    fn append(&self, value: &serde_json::Value) -> Result<(), MachineError> {
        let serialized = serde_json::to_string(value).map_err(|_| reset_not_allowed())?;
        let mut bytes = canonicalize(&parse(&serialized).map_err(|_| reset_not_allowed())?)
            .map_err(|_| reset_not_allowed())?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.journal_path)
            .map_err(|_| reset_not_allowed())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| reset_not_allowed())
    }
}

impl ResetJournal for DurableResetJournal {
    fn prepare(
        &mut self,
        operation_id: Uuid,
        state: &ControllerResetState,
    ) -> Result<(), MachineError> {
        self.append(&serde_json::json!({
            "schema_version": 1,
            "operation_id": operation_id,
            "status": "prepared",
            "run_id": state.run_id,
            "state_revision": state.state_revision,
            "controller_id": state.binding.identity.controller_id,
            "controller_generation": state.binding.identity.generation,
            "writer_authority": state.writer_authority,
        }))
    }

    fn commit(
        &mut self,
        record: &ControllerResetRecord,
        binding: &ControllerBinding,
    ) -> Result<(), MachineError> {
        atomic_replace_binding(&self.controller_path, binding)?;
        if let Err(error) = self.append(&serde_json::json!({
            "schema_version": 1,
            "operation_id": record.operation_id,
            "status": "committed",
            "run_id": record.run_id,
            "old_controller_id": record.old_controller_id,
            "new_controller_id": record.new_controller_id,
            "new_generation": record.new_generation,
            "writer_released": record.writer_released,
        })) {
            let _ = atomic_replace_binding(&self.controller_path, &self.previous);
            return Err(error);
        }
        Ok(())
    }

    fn fail(&mut self, operation_id: Uuid) -> Result<(), MachineError> {
        self.append(&serde_json::json!({
            "schema_version": 1,
            "operation_id": operation_id,
            "status": "failed",
        }))
    }
}

fn atomic_replace_binding(path: &Path, binding: &ControllerBinding) -> Result<(), MachineError> {
    let parent = path.parent().ok_or_else(reset_not_allowed)?;
    let temporary = parent.join(format!(".controller-{}.tmp", new_uuid_v7()));
    let bytes = serde_json::to_vec(binding).map_err(|_| reset_not_allowed())?;
    create_credential_file(&temporary, &bytes)?;
    fs::rename(&temporary, path).map_err(|_| reset_not_allowed())?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| reset_not_allowed())
}

pub fn default_operator_root() -> Result<PathBuf, MachineError> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MachineError::runtime_path_invalid("HOME", "HOME is not set"))?;
    let canonical = fs::canonicalize(home)
        .map_err(|_| MachineError::runtime_path_invalid("HOME", "HOME cannot be resolved"))?;
    Ok(canonical.join("Library/Application Support/Dolgorae/operator"))
}

pub fn carrier_from_options(
    arguments: &[std::ffi::OsString],
    file_flag: &str,
    fd_flag: &str,
) -> Result<CredentialCarrier, MachineError> {
    let file = optional_os(arguments, file_flag)?;
    let fd = optional_utf8(arguments, fd_flag)?
        .map(|value| {
            value.parse::<RawFd>().map_err(|_| {
                MachineError::invalid_argument(fd_flag, "fd must be a nonnegative integer")
            })
        })
        .transpose()?;
    if file.is_some() == fd.is_some() || fd.is_some_and(|value| value < 0) {
        return Err(MachineError::invalid_argument(
            file_flag,
            "exactly one credential carrier is required",
        ));
    }
    match (file, fd) {
        (Some(path), None) => CredentialCarrier::open_path(Path::new(&path)),
        (None, Some(descriptor)) => CredentialCarrier::duplicate_fd(descriptor),
        _ => unreachable!("exclusive carrier checked"),
    }
}

fn parse_controller_kind(value: &str) -> Result<ControllerKind, MachineError> {
    match value {
        "human_cli" => Ok(ControllerKind::HumanCli),
        "interactive_client" => Ok(ControllerKind::InteractiveClient),
        "workflow_orchestrator" => Ok(ControllerKind::WorkflowOrchestrator),
        "automation" => Ok(ControllerKind::Automation),
        "other" => Ok(ControllerKind::Other),
        _ => Err(MachineError::invalid_argument(
            "--kind",
            "unsupported controller kind",
        )),
    }
}

fn required_utf8(arguments: &[std::ffi::OsString], flag: &str) -> Result<String, MachineError> {
    let value = required_os(arguments, flag)?;
    value
        .into_string()
        .map_err(|_| MachineError::invalid_argument(flag, "value must be UTF-8"))
}

fn optional_utf8(
    arguments: &[std::ffi::OsString],
    flag: &str,
) -> Result<Option<String>, MachineError> {
    optional_os(arguments, flag)?
        .map(|value| {
            value
                .into_string()
                .map_err(|_| MachineError::invalid_argument(flag, "value must be UTF-8"))
        })
        .transpose()
}

fn required_os(
    arguments: &[std::ffi::OsString],
    flag: &str,
) -> Result<std::ffi::OsString, MachineError> {
    optional_os(arguments, flag)?
        .ok_or_else(|| MachineError::invalid_argument(flag, "required option is missing"))
}

fn optional_os(
    arguments: &[std::ffi::OsString],
    flag: &str,
) -> Result<Option<std::ffi::OsString>, MachineError> {
    use std::ffi::OsStr;
    let mut result = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == OsStr::new(flag) {
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| MachineError::invalid_argument(flag, "option value is missing"))?;
            if result.replace(value.clone()).is_some() {
                return Err(MachineError::invalid_argument(
                    flag,
                    "option may appear only once",
                ));
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(result)
}

fn positional(
    arguments: &[std::ffi::OsString],
    requested: usize,
) -> Result<&std::ffi::OsString, MachineError> {
    use std::ffi::OsStr;
    let value_flags = [
        "--workspace",
        "--controller-file",
        "--controller-fd",
        "--operator-file",
        "--operator-fd",
        "--new-controller-file",
        "--new-controller-fd",
        "--confirm",
    ];
    let mut position = 0;
    let mut index = 0;
    while index < arguments.len() {
        if value_flags
            .iter()
            .any(|flag| arguments[index] == OsStr::new(flag))
        {
            index += 2;
            continue;
        }
        if !arguments[index].as_encoded_bytes().starts_with(b"--") {
            if position == requested {
                return Ok(&arguments[index]);
            }
            position += 1;
        }
        index += 1;
    }
    Err(MachineError::invalid_argument(
        "run-id",
        "required positional argument is missing",
    ))
}

impl CredentialCarrier {
    pub fn open_path(path: &Path) -> Result<Self, MachineError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| credential_invalid())?;
        Self::from_file(file)
    }

    pub fn duplicate_fd(fd: RawFd) -> Result<Self, MachineError> {
        let owned = DarwinSystem
            .duplicate_fd_cloexec(fd)
            .map_err(|_| credential_invalid())?;
        Self::from_file(File::from(owned))
    }

    pub fn from_received_fd(fd: std::os::fd::OwnedFd) -> Result<Self, MachineError> {
        Self::from_file(File::from(fd))
    }

    fn from_file(file: File) -> Result<Self, MachineError> {
        let snapshot = secure_snapshot(&file)?;
        Ok(Self { file, snapshot })
    }

    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        use std::os::fd::AsRawFd as _;
        self.file.as_raw_fd()
    }

    fn reread(&self) -> Result<Zeroizing<Vec<u8>>, MachineError> {
        let before = secure_snapshot(&self.file)?;
        if before != self.snapshot {
            return Err(credential_invalid());
        }
        let capacity = usize::try_from(before.size).map_err(|_| credential_invalid())?;
        let mut bytes = Zeroizing::new(vec![0_u8; capacity]);
        let count = self
            .file
            .read_at(&mut bytes, 0)
            .map_err(|_| credential_invalid())?;
        if count != capacity || secure_snapshot(&self.file)? != before {
            return Err(credential_invalid());
        }
        Ok(bytes)
    }
}

fn secure_snapshot(file: &File) -> Result<FileSnapshot, MachineError> {
    let metadata = file.metadata().map_err(|_| credential_invalid())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() == 0
        || metadata.len() > CREDENTIAL_MAX_BYTES
    {
        return Err(credential_invalid());
    }
    Ok(FileSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn credential_invalid() -> MachineError {
    MachineError::new(
        "INVALID_ARGUMENT",
        "credential carrier is invalid",
        false,
        serde_json::json!({}),
    )
}

fn controller_mismatch() -> MachineError {
    MachineError::new(
        "CONTROLLER_MISMATCH",
        "controller credential does not authorize this operation",
        false,
        serde_json::json!({}),
    )
}

fn operator_mismatch() -> MachineError {
    MachineError::new(
        "OPERATOR_MISMATCH",
        "operator credential does not authorize this operation",
        false,
        serde_json::json!({}),
    )
}

fn parse_controller(bytes: &[u8]) -> Result<ControllerSecret, MachineError> {
    let text = std::str::from_utf8(bytes).map_err(|_| credential_invalid())?;
    let lossless = ZeroizingJson(parse(text).map_err(|_| credential_invalid())?);
    let canonical = Zeroizing::new(canonicalize(&lossless.0).map_err(|_| credential_invalid())?);
    let wire: ControllerWire =
        serde_json::from_slice(&canonical).map_err(|_| credential_invalid())?;
    if wire.schema_version != 1 || wire.controller_id.get_version_num() != 7 {
        return Err(credential_invalid());
    }
    validate_text(&wire.instance_id, 128, false)?;
    if let Some(subject) = &wire.subject_id {
        validate_text(subject, 256, false)?;
    }
    if let Some(launch) = &wire.orchestration_launch
        && (!matches!(
            wire.kind,
            ControllerKind::HumanCli | ControllerKind::InteractiveClient
        ) || launch.use_case != "dolgorae_orchestrated_session"
            || !valid_policy_name(&launch.specialist_policy_name))
    {
        return Err(credential_invalid());
    }
    let capability = decode_capability(&wire.capability)?;
    Ok(ControllerSecret {
        public: ControllerPublic {
            controller_id: wire.controller_id,
            kind: wire.kind,
            instance_id: wire.instance_id.clone(),
            subject_id: wire.subject_id.clone(),
            generation: 1,
        },
        capability,
    })
}

fn parse_operator(bytes: &[u8]) -> Result<OperatorSecret, MachineError> {
    let text = std::str::from_utf8(bytes).map_err(|_| credential_invalid())?;
    let lossless = ZeroizingJson(parse(text).map_err(|_| credential_invalid())?);
    let canonical = Zeroizing::new(canonicalize(&lossless.0).map_err(|_| credential_invalid())?);
    let wire: OperatorWire =
        serde_json::from_slice(&canonical).map_err(|_| credential_invalid())?;
    if wire.schema_version != 1 || wire.operator_id.get_version_num() != 7 {
        return Err(credential_invalid());
    }
    Ok(OperatorSecret {
        operator_id: wire.operator_id,
        capability: decode_capability(&wire.capability)?,
    })
}

fn validate_text(value: &str, maximum_bytes: usize, allow_empty: bool) -> Result<(), MachineError> {
    if (!allow_empty && value.is_empty())
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(credential_invalid());
    }
    Ok(())
}

fn valid_policy_name(value: &str) -> bool {
    value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn decode_capability(encoded: &str) -> Result<Zeroizing<[u8; 32]>, MachineError> {
    if encoded.len() != 43
        || encoded
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_' && byte != b'-')
    {
        return Err(credential_invalid());
    }
    let mut decoded = Zeroizing::new([0_u8; 32]);
    let count = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode_slice(encoded, &mut *decoded)
        .map_err(|_| credential_invalid())?;
    if count != 32 || base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*decoded) != encoded {
        return Err(credential_invalid());
    }
    Ok(decoded)
}

fn random_capability() -> Result<Zeroizing<[u8; 32]>, MachineError> {
    let mut capability = Zeroizing::new([0_u8; 32]);
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut *capability))
        .map_err(|_| {
            MachineError::new(
                "INTERNAL_ERROR",
                "secure randomness is unavailable",
                true,
                serde_json::json!({}),
            )
        })?;
    Ok(capability)
}

#[must_use]
pub fn normalized_principal(public: &ControllerPublic) -> (ControllerKind, &str) {
    (
        public.kind,
        public.subject_id.as_deref().unwrap_or(&public.instance_id),
    )
}

pub fn create_controller_credential(
    output: &Path,
    kind: ControllerKind,
    instance_id: String,
    subject_id: Option<String>,
    orchestration_policy: Option<String>,
) -> Result<CredentialCreated<ControllerPublic>, MachineError> {
    validate_text(&instance_id, 128, false)?;
    if let Some(subject) = &subject_id {
        validate_text(subject, 256, false)?;
    }
    let orchestration_launch =
        orchestration_policy.map(|specialist_policy_name| OrchestrationLaunch {
            use_case: "dolgorae_orchestrated_session".to_owned(),
            specialist_policy_name,
        });
    if orchestration_launch.as_ref().is_some_and(|launch| {
        !matches!(
            kind,
            ControllerKind::HumanCli | ControllerKind::InteractiveClient
        ) || !valid_policy_name(&launch.specialist_policy_name)
    }) {
        return Err(MachineError::invalid_argument(
            "--orchestration-policy",
            "policy is invalid for this controller",
        ));
    }
    let public = ControllerPublic {
        controller_id: new_uuid_v7(),
        kind,
        instance_id,
        subject_id,
        generation: 1,
    };
    let mut capability = random_capability()?;
    let encoded =
        Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*capability));
    let wire = ControllerWireRef {
        schema_version: 1,
        controller_id: public.controller_id,
        kind: public.kind,
        instance_id: &public.instance_id,
        subject_id: public.subject_id.as_deref(),
        capability: &encoded,
        orchestration_launch: orchestration_launch.as_ref(),
    };
    let serialized = Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| credential_invalid())?);
    create_credential_file(output, &serialized)?;
    let digest = controller_capability_digest(&capability);
    let fingerprint = digest[..16].to_owned();
    capability.zeroize();
    Ok(CredentialCreated {
        credential: public,
        output_path: output.to_path_buf(),
        fingerprint,
    })
}

fn create_credential_file(path: &Path, bytes: &[u8]) -> Result<(), MachineError> {
    let parent = path.parent().ok_or_else(credential_invalid)?;
    let metadata = fs::metadata(parent).map_err(|_| credential_invalid())?;
    if !metadata.is_dir()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(credential_invalid());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| credential_invalid())?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(credential_invalid());
    }
    if File::open(parent)
        .and_then(|directory| directory.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(credential_invalid());
    }
    Ok(())
}

pub fn authorize_controller(
    binding: &ControllerBinding,
    carrier: &CredentialCarrier,
) -> Result<ControllerIdentity, MachineError> {
    let bytes = carrier.reread().map_err(|_| controller_mismatch())?;
    let secret = parse_controller(&bytes).map_err(|_| controller_mismatch())?;
    let observed = controller_capability_digest(&secret.capability);
    let identity_matches = secret.public.controller_id == binding.identity.controller_id;
    let capability_matches =
        constant_time_equal(observed.as_bytes(), binding.capability_sha256.as_bytes());
    if !(identity_matches & capability_matches) {
        return Err(controller_mismatch());
    }
    Ok(binding.identity.clone())
}

pub fn binding_from_credential(
    public: &ControllerPublic,
    generation: u64,
    carrier: &CredentialCarrier,
) -> Result<ControllerBinding, MachineError> {
    let bytes = carrier.reread()?;
    let secret = parse_controller(&bytes)?;
    if secret.public.controller_id != public.controller_id
        || normalized_principal(&secret.public) != normalized_principal(public)
    {
        return Err(controller_mismatch());
    }
    Ok(ControllerBinding {
        identity: ControllerIdentity {
            controller_id: public.controller_id,
            kind: public.kind,
            instance_id: public.instance_id.clone(),
            subject_id: public.subject_id.clone(),
            generation,
        },
        capability_sha256: controller_capability_digest(&secret.capability),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOperation {
    Observe,
    Status,
    Wait,
    Pending,
    Events,
    WriterStatus,
    Verify,
    Timeline,
    InteractionGet,
    ArtifactControllerRead,
    Export,
    Send,
    Submit,
    Respond,
    Interrupt,
    SetEffort,
    AcquireWrite,
    ReleaseWrite,
    Pause,
    Resume,
    Recover,
    Reconcile,
    Fork,
    CreateWriteContinuation,
    Close,
    Delete,
    WriterHandoff,
}

impl RunOperation {
    #[must_use]
    pub const fn requires_controller(self) -> bool {
        !matches!(
            self,
            Self::Observe
                | Self::Status
                | Self::Wait
                | Self::Pending
                | Self::Events
                | Self::WriterStatus
                | Self::Verify
        )
    }
}

pub struct RunAuthority {
    binding: Mutex<ControllerBinding>,
    state_revision: Mutex<u64>,
}

impl RunAuthority {
    #[must_use]
    pub fn new(binding: ControllerBinding, state_revision: u64) -> Self {
        Self {
            binding: Mutex::new(binding),
            state_revision: Mutex::new(state_revision),
        }
    }

    pub fn mutate<T>(
        &self,
        operation: RunOperation,
        expected_revision: u64,
        carrier: &CredentialCarrier,
        effect: impl FnOnce() -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        if !operation.requires_controller() {
            return Err(MachineError::new(
                "INTERNAL_ERROR",
                "mutation operation is not classified",
                false,
                serde_json::json!({}),
            ));
        }
        let binding = self.binding.lock().map_err(|_| state_conflict())?;
        let revision = self.state_revision.lock().map_err(|_| state_conflict())?;
        if *revision != expected_revision {
            return Err(state_conflict());
        }
        authorize_controller(&binding, carrier)?;
        effect()
    }
}

fn state_conflict() -> MachineError {
    MachineError::new(
        "RUN_STATE_CONFLICT",
        "run state changed",
        true,
        serde_json::json!({}),
    )
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorState {
    pub schema_version: u32,
    pub operator_id: Uuid,
    pub generation: u64,
    pub capability_sha256: String,
}

pub struct OperatorStore {
    root: PathBuf,
}

impl OperatorStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn initialize(
        &self,
        output: &Path,
    ) -> Result<CredentialCreated<OperatorPublic>, MachineError> {
        self.ensure_root()?;
        let lock = self.lock()?;
        DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|_| operator_state_error())?;
        if self.state_path().exists() {
            return Err(MachineError::new(
                "RUN_STATE_CONFLICT",
                "operator credential is already initialized",
                false,
                serde_json::json!({}),
            ));
        }
        let operator_id = new_uuid_v7();
        let capability = random_capability()?;
        let encoded =
            Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*capability));
        let serialized = Zeroizing::new(
            serde_json::to_vec(&OperatorWireRef {
                schema_version: 1,
                operator_id,
                capability: &encoded,
            })
            .map_err(|_| operator_state_error())?,
        );
        create_credential_file(output, &serialized)?;
        let state = OperatorState {
            schema_version: 1,
            operator_id,
            generation: 1,
            capability_sha256: operator_digest(&capability),
        };
        if let Err(error) = self.write_state_create(&state) {
            let _ = fs::remove_file(output);
            return Err(error);
        }
        Ok(CredentialCreated {
            credential: OperatorPublic {
                operator_id,
                operator_generation: 1,
            },
            output_path: output.to_path_buf(),
            fingerprint: state.capability_sha256[..16].to_owned(),
        })
    }

    pub fn rotate(
        &self,
        carrier: &CredentialCarrier,
        output: &Path,
    ) -> Result<CredentialCreated<OperatorPublic>, MachineError> {
        self.ensure_root()?;
        let lock = self.lock()?;
        DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|_| operator_state_error())?;
        let current = self.load_state()?;
        authorize_operator_state(&current, carrier)?;
        let capability = random_capability()?;
        let encoded =
            Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*capability));
        let serialized = Zeroizing::new(
            serde_json::to_vec(&OperatorWireRef {
                schema_version: 1,
                operator_id: current.operator_id,
                capability: &encoded,
            })
            .map_err(|_| operator_state_error())?,
        );
        create_credential_file(output, &serialized)?;
        let next = OperatorState {
            generation: current
                .generation
                .checked_add(1)
                .ok_or_else(operator_state_error)?,
            capability_sha256: operator_digest(&capability),
            ..current
        };
        if let Err(error) = self.write_state_replace(&next) {
            let _ = fs::remove_file(output);
            return Err(error);
        }
        Ok(CredentialCreated {
            credential: OperatorPublic {
                operator_id: next.operator_id,
                operator_generation: next.generation,
            },
            output_path: output.to_path_buf(),
            fingerprint: next.capability_sha256[..16].to_owned(),
        })
    }

    pub fn authorize(&self, carrier: &CredentialCarrier) -> Result<OperatorPublic, MachineError> {
        self.ensure_root()?;
        let lock = self.lock()?;
        DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|_| operator_state_error())?;
        let state = self.load_state()?;
        authorize_operator_state(&state, carrier)?;
        Ok(OperatorPublic {
            operator_id: state.operator_id,
            operator_generation: state.generation,
        })
    }

    fn ensure_root(&self) -> Result<(), MachineError> {
        if !self.root.exists() {
            let mut missing = Vec::new();
            let mut candidate = self.root.as_path();
            while !candidate.exists() {
                missing.push(candidate.to_path_buf());
                candidate = candidate.parent().ok_or_else(operator_state_error)?;
            }
            for directory in missing.into_iter().rev() {
                fs::create_dir(&directory).map_err(|_| operator_state_error())?;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                    .map_err(|_| operator_state_error())?;
            }
        }
        let metadata = fs::symlink_metadata(&self.root).map_err(|_| operator_state_error())?;
        if !metadata.is_dir()
            || metadata.uid() != DarwinSystem.current_uid()
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(operator_state_error());
        }
        if let Some(parent) = self.root.parent() {
            let parent_metadata =
                fs::symlink_metadata(parent).map_err(|_| operator_state_error())?;
            if !parent_metadata.is_dir()
                || parent_metadata.uid() != DarwinSystem.current_uid()
                || parent_metadata.permissions().mode() & 0o077 != 0
            {
                return Err(operator_state_error());
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<File, MachineError> {
        let path = self.root.join("operator.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| operator_state_error())?;
        let metadata = file.metadata().map_err(|_| operator_state_error())?;
        if !metadata.is_file()
            || metadata.uid() != DarwinSystem.current_uid()
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(operator_state_error());
        }
        Ok(file)
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("operator.json")
    }

    fn load_state(&self) -> Result<OperatorState, MachineError> {
        let carrier =
            CredentialCarrier::open_path(&self.state_path()).map_err(|_| operator_state_error())?;
        let bytes = carrier.reread().map_err(|_| operator_state_error())?;
        let value = parse(std::str::from_utf8(&bytes).map_err(|_| operator_state_error())?)
            .map_err(|_| operator_state_error())?;
        let canonical = canonicalize(&value).map_err(|_| operator_state_error())?;
        let state: OperatorState =
            serde_json::from_slice(&canonical).map_err(|_| operator_state_error())?;
        if state.schema_version != 1
            || state.operator_id.get_version_num() != 7
            || state.generation == 0
            || state.capability_sha256.len() != 64
        {
            return Err(operator_state_error());
        }
        Ok(state)
    }

    fn write_state_create(&self, state: &OperatorState) -> Result<(), MachineError> {
        create_credential_file(
            &self.state_path(),
            &serde_json::to_vec(state).map_err(|_| operator_state_error())?,
        )
        .map_err(|_| operator_state_error())
    }

    fn write_state_replace(&self, state: &OperatorState) -> Result<(), MachineError> {
        let temporary = self.root.join(format!(".operator-{}.tmp", new_uuid_v7()));
        create_credential_file(
            &temporary,
            &serde_json::to_vec(state).map_err(|_| operator_state_error())?,
        )?;
        fs::rename(&temporary, self.state_path()).map_err(|_| operator_state_error())?;
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| operator_state_error())
    }
}

fn operator_state_error() -> MachineError {
    MachineError::new(
        "OPERATOR_MISMATCH",
        "operator credential does not authorize this operation",
        false,
        serde_json::json!({}),
    )
}

fn operator_digest(capability: &[u8; 32]) -> String {
    let mut preimage = Vec::with_capacity(OPERATOR_DOMAIN.len() + capability.len());
    preimage.extend_from_slice(OPERATOR_DOMAIN);
    preimage.extend_from_slice(capability);
    sha256_hex(&preimage)
}

fn authorize_operator_state(
    state: &OperatorState,
    carrier: &CredentialCarrier,
) -> Result<(), MachineError> {
    let bytes = carrier.reread().map_err(|_| operator_mismatch())?;
    let secret = parse_operator(&bytes).map_err(|_| operator_mismatch())?;
    let observed = operator_digest(&secret.capability);
    let identity_matches = secret.operator_id == state.operator_id;
    let capability_matches =
        constant_time_equal(observed.as_bytes(), state.capability_sha256.as_bytes());
    if !(identity_matches & capability_matches) {
        return Err(operator_mismatch());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SafeRunObservation {
    pub run_id: Uuid,
    pub controller: ControllerIdentity,
    pub purpose: Purpose,
    pub parent_ref: Option<ParentReference>,
    pub lifecycle: RunLifecycle,
    pub state_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerResetState {
    pub run_id: Uuid,
    pub binding: ControllerBinding,
    pub state_revision: u64,
    pub lifecycle: RunLifecycle,
    pub pending_interaction: bool,
    pub handoff_active: bool,
    pub writer_authority: bool,
    pub generation_verifiable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ControllerResetRecord {
    pub operation_id: Uuid,
    pub run_id: Uuid,
    pub old_controller_id: Uuid,
    pub new_controller_id: Uuid,
    pub new_generation: u64,
    pub writer_released: bool,
}

pub trait ResetJournal {
    fn prepare(
        &mut self,
        operation_id: Uuid,
        state: &ControllerResetState,
    ) -> Result<(), MachineError>;
    fn commit(
        &mut self,
        record: &ControllerResetRecord,
        binding: &ControllerBinding,
    ) -> Result<(), MachineError>;
    fn fail(&mut self, operation_id: Uuid) -> Result<(), MachineError>;
}

pub fn reset_controller(
    state: &mut ControllerResetState,
    confirm_run_id: Uuid,
    operator_store: &OperatorStore,
    operator: &CredentialCarrier,
    replacement: &CredentialCarrier,
    journal: &mut impl ResetJournal,
    apply: impl FnOnce() -> Result<(), MachineError>,
) -> Result<ControllerResetRecord, MachineError> {
    operator_store.authorize(operator)?;
    if confirm_run_id != state.run_id
        || !matches!(
            state.lifecycle,
            RunLifecycle::Idle | RunLifecycle::Paused | RunLifecycle::OutcomeUnknown
        )
        || state.pending_interaction
        || state.handoff_active
        || !state.generation_verifiable
    {
        return Err(MachineError::new(
            "CONTROLLER_RESET_NOT_ALLOWED",
            "controller reset is not allowed in the current state",
            false,
            serde_json::json!({}),
        ));
    }
    let bytes = replacement.reread().map_err(|_| controller_mismatch())?;
    let next = parse_controller(&bytes).map_err(|_| controller_mismatch())?;
    let old_public = ControllerPublic {
        controller_id: state.binding.identity.controller_id,
        kind: state.binding.identity.kind,
        instance_id: state.binding.identity.instance_id.clone(),
        subject_id: state.binding.identity.subject_id.clone(),
        generation: state.binding.identity.generation,
    };
    if next.public.controller_id == old_public.controller_id
        || normalized_principal(&next.public) != normalized_principal(&old_public)
    {
        return Err(controller_mismatch());
    }
    let operation_id = new_uuid_v7();
    journal.prepare(operation_id, state)?;
    if let Err(error) = apply() {
        let _ = journal.fail(operation_id);
        return Err(error);
    }
    let new_generation = state
        .binding
        .identity
        .generation
        .checked_add(1)
        .ok_or_else(state_conflict)?;
    let record = ControllerResetRecord {
        operation_id,
        run_id: state.run_id,
        old_controller_id: state.binding.identity.controller_id,
        new_controller_id: next.public.controller_id,
        new_generation,
        writer_released: state.writer_authority,
    };
    let new_revision = state
        .state_revision
        .checked_add(1)
        .ok_or_else(state_conflict)?;
    let old_binding = state.binding.clone();
    let old_writer_authority = state.writer_authority;
    let old_revision = state.state_revision;
    state.binding = ControllerBinding {
        identity: ControllerIdentity {
            controller_id: next.public.controller_id,
            kind: next.public.kind,
            instance_id: next.public.instance_id,
            subject_id: next.public.subject_id,
            generation: new_generation,
        },
        capability_sha256: controller_capability_digest(&next.capability),
    };
    state.writer_authority = false;
    state.state_revision = new_revision;
    if let Err(error) = journal.commit(&record, &state.binding) {
        state.binding = old_binding;
        state.writer_authority = old_writer_authority;
        state.state_revision = old_revision;
        let _ = journal.fail(operation_id);
        return Err(error);
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn private_directory() -> PathBuf {
        let root = std::env::temp_dir().join(format!("dolgorae-controller-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn strict_controller_creation_and_authorization_hide_secret() {
        let root = private_directory();
        let path = root.join("controller.json");
        let created = create_controller_credential(
            &path,
            ControllerKind::Automation,
            "ci-1".to_owned(),
            Some("pipeline".to_owned()),
            None,
        )
        .unwrap();
        let carrier = CredentialCarrier::open_path(&path).unwrap();
        let binding = binding_from_credential(&created.credential, 1, &carrier).unwrap();
        assert_eq!(
            authorize_controller(&binding, &carrier).unwrap().generation,
            1
        );
        let serialized = serde_json::to_string(&binding.identity).unwrap();
        assert!(!serialized.contains("capability"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn observer_is_open_and_mutation_denies_before_effect() {
        assert!(!RunOperation::Status.requires_controller());
        assert!(RunOperation::Submit.requires_controller());
        let root = private_directory();
        let path = root.join("controller.json");
        let created = create_controller_credential(
            &path,
            ControllerKind::HumanCli,
            "terminal".to_owned(),
            None,
            None,
        )
        .unwrap();
        let carrier = CredentialCarrier::open_path(&path).unwrap();
        let mut binding = binding_from_credential(&created.credential, 1, &carrier).unwrap();
        binding.identity.controller_id = new_uuid_v7();
        let authority = RunAuthority::new(binding, 4);
        let effect = std::sync::atomic::AtomicBool::new(false);
        assert_eq!(
            authority
                .mutate(RunOperation::Submit, 4, &carrier, || {
                    effect.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .unwrap_err()
                .code,
            "CONTROLLER_MISMATCH"
        );
        assert!(!effect.load(std::sync::atomic::Ordering::SeqCst));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inherited_descriptor_survives_path_replacement_and_scm_rights() {
        let root = private_directory();
        let path = root.join("controller.json");
        create_controller_credential(
            &path,
            ControllerKind::Other,
            "caller".to_owned(),
            None,
            None,
        )
        .unwrap();
        let original = CredentialCarrier::open_path(&path).unwrap();
        let (manager, worker) = UnixStream::pair().unwrap();
        DarwinSystem.send_fd(&manager, original.raw_fd()).unwrap();
        let received = DarwinSystem.receive_fd(&worker).unwrap();
        let worker_carrier = CredentialCarrier::from_received_fd(received).unwrap();
        assert_eq!(original.reread().unwrap(), worker_carrier.reread().unwrap());
        assert!(worker_carrier.raw_fd() >= 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn carrier_rejects_symlink_wrong_mode_oversize_and_duplicate_members() {
        use std::os::unix::fs::symlink;

        let root = private_directory();
        let valid = root.join("valid.json");
        create_controller_credential(
            &valid,
            ControllerKind::Other,
            "strict".to_owned(),
            None,
            None,
        )
        .unwrap();
        let link = root.join("link.json");
        symlink(&valid, &link).unwrap();
        assert!(CredentialCarrier::open_path(&link).is_err());

        fs::set_permissions(&valid, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(CredentialCarrier::open_path(&valid).is_err());
        fs::set_permissions(&valid, fs::Permissions::from_mode(0o600)).unwrap();

        let oversized = root.join("oversized.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&oversized)
            .unwrap();
        file.write_all(&vec![b'x'; 4097]).unwrap();
        assert!(CredentialCarrier::open_path(&oversized).is_err());

        assert!(parse_controller(br#"{"schema_version":1,"schema_version":1}"#).is_err());
        assert!(decode_capability("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operator_rotation_invalidates_the_old_generation() {
        let root = private_directory();
        let store = OperatorStore::new(root.join("operator"));
        let first_path = root.join("operator-1.json");
        let first = store.initialize(&first_path).unwrap();
        assert_eq!(first.credential.operator_generation, 1);
        assert_eq!(first.fingerprint.len(), 16);
        let first_carrier = CredentialCarrier::open_path(&first_path).unwrap();
        let second_path = root.join("operator-2.json");
        let second = store.rotate(&first_carrier, &second_path).unwrap();
        assert_eq!(second.credential.operator_generation, 2);
        assert_eq!(
            store.authorize(&first_carrier).unwrap_err().code,
            "OPERATOR_MISMATCH"
        );
        let second_carrier = CredentialCarrier::open_path(&second_path).unwrap();
        assert_eq!(
            store
                .authorize(&second_carrier)
                .unwrap()
                .operator_generation,
            2
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Default)]
    struct MemoryJournal {
        prepared: bool,
        committed: bool,
        failed: bool,
        fail_commit: bool,
    }

    impl ResetJournal for MemoryJournal {
        fn prepare(&mut self, _: Uuid, _: &ControllerResetState) -> Result<(), MachineError> {
            self.prepared = true;
            Ok(())
        }

        fn commit(
            &mut self,
            _: &ControllerResetRecord,
            _: &ControllerBinding,
        ) -> Result<(), MachineError> {
            if self.fail_commit {
                return Err(state_conflict());
            }
            self.committed = true;
            Ok(())
        }

        fn fail(&mut self, _: Uuid) -> Result<(), MachineError> {
            self.failed = true;
            Ok(())
        }
    }

    #[test]
    fn reset_matrix_and_commit_failure_preserve_old_authority() {
        let root = private_directory();
        let operator_store = OperatorStore::new(root.join("operator"));
        let operator_path = root.join("operator.json");
        operator_store.initialize(&operator_path).unwrap();
        let operator = CredentialCarrier::open_path(&operator_path).unwrap();

        let old_path = root.join("old.json");
        let old = create_controller_credential(
            &old_path,
            ControllerKind::Automation,
            "job-1".to_owned(),
            Some("pipeline".to_owned()),
            None,
        )
        .unwrap();
        let old_carrier = CredentialCarrier::open_path(&old_path).unwrap();
        let new_path = root.join("new.json");
        create_controller_credential(
            &new_path,
            ControllerKind::Automation,
            "job-2".to_owned(),
            Some("pipeline".to_owned()),
            None,
        )
        .unwrap();
        let replacement = CredentialCarrier::open_path(&new_path).unwrap();
        let run_id = new_uuid_v7();
        let binding = binding_from_credential(&old.credential, 3, &old_carrier).unwrap();
        let mut state = ControllerResetState {
            run_id,
            binding,
            state_revision: 8,
            lifecycle: RunLifecycle::Running,
            pending_interaction: false,
            handoff_active: false,
            writer_authority: true,
            generation_verifiable: true,
        };
        let mut journal = MemoryJournal::default();
        assert_eq!(
            reset_controller(
                &mut state,
                run_id,
                &operator_store,
                &operator,
                &replacement,
                &mut journal,
                || Ok(())
            )
            .unwrap_err()
            .code,
            "CONTROLLER_RESET_NOT_ALLOWED"
        );
        state.lifecycle = RunLifecycle::Paused;
        journal.fail_commit = true;
        let old_binding = state.binding.clone();
        assert!(
            reset_controller(
                &mut state,
                run_id,
                &operator_store,
                &operator,
                &replacement,
                &mut journal,
                || Ok(())
            )
            .is_err()
        );
        assert_eq!(state.binding, old_binding);
        assert!(state.writer_authority);
        assert!(journal.prepared && journal.failed && !journal.committed);

        for lifecycle in [
            RunLifecycle::Idle,
            RunLifecycle::Paused,
            RunLifecycle::OutcomeUnknown,
        ] {
            let mut allowed = ControllerResetState {
                lifecycle,
                writer_authority: true,
                binding: old_binding.clone(),
                ..state.clone()
            };
            let mut success_journal = MemoryJournal::default();
            let record = reset_controller(
                &mut allowed,
                run_id,
                &operator_store,
                &operator,
                &replacement,
                &mut success_journal,
                || Ok(()),
            )
            .unwrap();
            assert_eq!(record.new_generation, 4);
            assert!(!allowed.writer_authority);
            assert!(success_journal.prepared && success_journal.committed);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_credential_can_bind_multiple_runs_without_sharing_mutation_state() {
        let root = private_directory();
        let path = root.join("controller.json");
        let created = create_controller_credential(
            &path,
            ControllerKind::WorkflowOrchestrator,
            "workflow".to_owned(),
            Some("session".to_owned()),
            None,
        )
        .unwrap();
        let carrier = CredentialCarrier::open_path(&path).unwrap();
        let first = binding_from_credential(&created.credential, 1, &carrier).unwrap();
        let second = binding_from_credential(&created.credential, 1, &carrier).unwrap();
        let first_authority = RunAuthority::new(first, 2);
        let second_authority = RunAuthority::new(second, 9);
        assert!(
            first_authority
                .mutate(RunOperation::Submit, 2, &carrier, || Ok(()))
                .is_ok()
        );
        assert_eq!(
            second_authority
                .mutate(RunOperation::Submit, 2, &carrier, || Ok(()))
                .unwrap_err()
                .code,
            "RUN_STATE_CONFLICT"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
