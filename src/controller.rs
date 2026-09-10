//! Controller and local-operator credential authority.
//!
//! Secret material exists only in zeroizing buffers owned by an already-open
//! carrier. Public identities and persisted state deliberately omit it.

use crate::darwin::DarwinSystem;
use crate::domain::{ControllerIdentity, ControllerKind, Purpose, RunLifecycle};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{LedgerClock as _, SystemLedgerClock};
use crate::machine::{MachineError, new_uuid_v7};
use crate::paths::DolgoraeHome;
use crate::projection::{ProjectedWriterAuthority, RunStateProjection};
use crate::run::{ControllerBinding, ParentReference, RunStore, controller_capability_digest};
use crate::workspace::{SystemWorkspacePlatform, WorkspaceService};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::RawFd;
use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
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
    expected_generation: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialOperation {
    ControllerCreate,
    OperatorInitialize,
    OperatorRotate,
    RunVerify,
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
    let dolgorae_home = default_operator_root()?
        .parent()
        .expect("operator root has Dolgorae home parent")
        .to_path_buf();
    let state_root = dolgorae_home.join("workspaces").join(&view.workspace_id);
    let binding = load_reconciled_controller_binding(&state_root, run_id)?;
    let carrier = carrier_from_options(arguments, "--controller-file", "--controller-fd")?;
    let controller = authorize_controller(run_id, "run.controller.verify", &binding, &carrier)?;
    Ok(serde_json::json!({
        "run_id": run_id,
        "controller_id": controller.controller_id,
        "generation": controller.generation,
        "kind": controller.kind,
        "verified_at": SystemLedgerClock::default().timestamp(),
    }))
}

/// `run controller reset`: replace a Run's Controller under the operator
/// capability, serialized against everything that could accept work under the
/// Controller being replaced.
pub fn reset_run(
    arguments: &[std::ffi::OsString],
    environment: &dyn RunResetEnvironment,
) -> Result<serde_json::Value, MachineError> {
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
    let dolgorae_home = default_operator_root()?
        .parent()
        .expect("operator root has Dolgorae home parent")
        .to_path_buf();
    let state_root = dolgorae_home.join("workspaces").join(&view.workspace_id);
    let run_root = state_root.join("runs").join(run_id.to_string());
    let binding = load_controller_binding_for_reset(&state_root, run_id)?;
    let operator = carrier_from_options(arguments, "--operator-file", "--operator-fd")?;
    let replacement =
        carrier_from_options(arguments, "--new-controller-file", "--new-controller-fd")?;
    // Probe the Run's worker before taking any lock: a control round trip is
    // an external wait, and SPEC-013 forbids one under a filesystem lock. The
    // proof is completed under the lock, where the runtime record is reread
    // and required to be the same one this probe answered for.
    let probed = environment.worker_fingerprint(&state_root, run_id);
    let run_lock = environment.open_run_lock(&state_root, run_id)?;
    let live_worker = std::cell::Cell::new(false);
    let mut state = ControllerResetState {
        run_id,
        binding: binding.clone(),
        // Filled in by `observe`, under the reset's own lock prefix.
        state_revision: 0,
        lifecycle: RunLifecycle::ReconciliationRequired,
        pending_interaction: true,
        handoff_active: true,
        writer_authority: true,
        generation_verifiable: false,
    };
    let mut journal = DurableResetJournal::new(run_id, &run_root, binding);
    let record = reset_controller(
        &mut state,
        confirmation,
        &ResetAuthority {
            operator_store: &OperatorStore::new(default_operator_root()?),
            operator: &operator,
            run_lock: run_lock.as_ref(),
        },
        &replacement,
        &mut journal,
        |state| {
            // Under `operator.lock` and the Run's startup lock: no worker can
            // win the startup election from here until PREPARE is durable, so
            // reproducing the pre-lock probe completes the proof rather than
            // repeating a guess.
            let observed = environment.worker_fingerprint(&state_root, run_id);
            if observed != probed {
                // A worker started or stopped between the probe and the lock.
                // Under the lock the answer is now stable, so the operator
                // simply runs the command again against the Run as it is.
                return Err(MachineError::new(
                    "RUN_BUSY",
                    "this run's worker changed while the reset was proving it",
                    true,
                    serde_json::json!({"run_id": run_id, "owner_kind": "startup"}),
                ));
            }
            live_worker.set(observed.is_some());
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
            state.state_revision = projection.ledger_head.sequence;
            state.lifecycle = projection.lifecycle;
            state.pending_interaction =
                !projection.pending_requests.is_empty() || projection.active_turn_id.is_some();
            state.handoff_active = handoff_active;
            state.writer_authority = writer_authority;
            state.generation_verifiable = generation_verifiable;
            Ok(())
        },
        || {
            if !live_worker.get() {
                // No worker could accept a mutation while the startup lock was
                // held, and none can accept one now: the durable prepare fences
                // every mutation until this operation resolves.
                return Ok(());
            }
            environment.fence_live_worker(&state_root, run_id, confirmation)
        },
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
    // A run whose durable state projection cannot be read has no lifecycle to
    // report, and `CONTROLLER_RESET_NOT_ALLOWED` must name one.  What is
    // actually true is that this Run's own state cannot be proved safe, which
    // is exactly the registered recovery refusal.
    let blocked = |reason: &str| recovery_required(run_id, 0, reason);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| blocked("state_projection_unreadable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| blocked("state_projection_unreadable"))?;
    if !metadata.is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > crate::jcs::RAW_PAYLOAD_LIMIT as u64
    {
        return Err(blocked("state_projection_insecure"));
    }
    let mut bytes = Vec::new();
    (&file)
        .read_to_end(&mut bytes)
        .map_err(|_| blocked("state_projection_unreadable"))?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| blocked("state_projection_invalid_encoding"))?;
    let canonical =
        canonicalize(&parse(text).map_err(|_| blocked("state_projection_invalid_json"))?)
            .map_err(|_| blocked("state_projection_invalid_json"))?;
    let projection: RunStateProjection = serde_json::from_slice(&canonical)
        .map_err(|_| blocked("state_projection_schema_invalid"))?;
    if projection.run_id != run_id {
        return Err(blocked("state_projection_run_id_mismatch"));
    }
    Ok(projection)
}

/// The refusal a reset reports when live Run state blocks it.
///
/// Public because the composition layer that talks to the Run's worker builds
/// the same refusal from the worker's answer.  The lifecycle is the Run's own,
/// carried from whoever observed it: the checked contract binds `state` to the
/// run lifecycle, so no caller of this may invent one.
#[must_use]
pub fn reset_blocked_by(run_id: Uuid, state: RunLifecycle, blockers: Vec<String>) -> MachineError {
    reset_not_allowed(run_id, state, blockers)
}

/// The registered refusal for durable Run state that cannot be proved safe.
#[must_use]
pub fn recovery_required(run_id: Uuid, generation: u64, reason: &str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "run state cannot be proved safe",
        false,
        serde_json::json!({
            "run_id": run_id,
            "generation": generation,
            "identity_verdict": "Unverifiable",
            "reason": reason,
        }),
    )
}

/// The registered busy refusal, naming which owner holds the Run.
#[must_use]
pub fn run_busy(run_id: Uuid, owner_kind: &str, message: &str) -> MachineError {
    MachineError::new(
        "RUN_BUSY",
        message,
        true,
        serde_json::json!({"run_id": run_id, "owner_kind": owner_kind}),
    )
}

fn reset_not_allowed(run_id: Uuid, state: RunLifecycle, blockers: Vec<String>) -> MachineError {
    MachineError::new(
        "CONTROLLER_RESET_NOT_ALLOWED",
        "controller reset is not allowed in the current state",
        false,
        serde_json::json!({"run_id": run_id, "state": state, "blockers": blockers}),
    )
}

/// The last unresolved (crashed mid-flight) prepare in a run's durable reset
/// journal, if the journal's final record is a "prepared" entry with no
/// later "committed" or "failed" resolution for the same attempt.
struct PendingReset {
    controller_generation: u64,
}

/// Scans a run's durable reset journal for a crashed, unresolved reset
/// attempt. The journal is a single-writer append log for one run's reset
/// history, so the terminal record is authoritative: `committed`/`failed`
/// always resolves the prepare immediately before it, and a trailing
/// `prepared` record with nothing after it means the process died between
/// `prepare` and `commit`/`fail`.
fn last_unresolved_prepare(
    run_root: &Path,
    run_id: Uuid,
) -> Result<Option<PendingReset>, MachineError> {
    let blocked = |blocker: &str| {
        reset_not_allowed(
            run_id,
            RunLifecycle::ReconciliationRequired,
            vec![blocker.to_owned()],
        )
    };
    let journal_path = run_root.join("recovery/controller-reset.jsonl");
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&journal_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(blocked("reset_journal_unreadable")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| blocked("reset_journal_unreadable"))?;
    if !metadata.is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > crate::jcs::RAW_PAYLOAD_LIMIT as u64
    {
        return Err(blocked("reset_journal_insecure"));
    }
    let mut bytes = Vec::new();
    (&file)
        .read_to_end(&mut bytes)
        .map_err(|_| blocked("reset_journal_unreadable"))?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| blocked("reset_journal_invalid_encoding"))?;
    let mut pending: Option<PendingReset> = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: serde_json::Value =
            serde_json::from_str(line).map_err(|_| blocked("reset_journal_invalid_record"))?;
        match record.get("status").and_then(serde_json::Value::as_str) {
            Some("prepared") => {
                let controller_generation = record
                    .get("controller_generation")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| blocked("reset_journal_invalid_record"))?;
                pending = Some(PendingReset {
                    controller_generation,
                });
            }
            Some("committed" | "failed") => pending = None,
            _ => return Err(blocked("reset_journal_invalid_record")),
        }
    }
    Ok(pending)
}

/// What a loaded Controller binding is about to be used for.
///
/// The two uses disagree about one thing only: an unresolved reset prepare.
/// For a mutation it is a fence and must stop the caller; for the reset that
/// is resolving it, refusing would make a crashed prepare permanent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BindingPurpose {
    /// Authorizing an effect on the Run.
    Mutation,
    /// Deciding a controller reset, which is what resolves a prepare.
    Reset,
}

/// Fails closed on the two ways a durable reset journal can withhold authority
/// from `controller.json`.
///
/// * **A prepare in flight.** SPEC-013: a reset PREPARE "fences new turns and
///   fsyncs a reset token before releasing all locks". The fsynced token *is*
///   that fence, and it is the only one that reaches a worker in another
///   process: from the moment it is durable until the operation resolves,
///   every mutation authorized from this binding stops. That is what keeps a
///   Turn from being accepted under a Controller the operator is replacing,
///   and it is crash-safe — a process that dies mid-reset leaves the Run
///   fenced rather than half-reset. The reset itself is exempt, because
///   resolving the token is exactly what it is for.
/// * **A binding that outran its commit token.** A crash between the binding
///   write and the journal's "committed" append (or a failed rollback of
///   either) can leave `controller.json` naming a new controller generation
///   the journal never durably confirmed; treating that file as authoritative
///   would let the new controller become authoritative without ever having a
///   durable committed token. No caller, reset included, may trust it.
fn reconcile_binding_with_journal(
    run_root: &Path,
    run_id: Uuid,
    binding: ControllerBinding,
    purpose: BindingPurpose,
) -> Result<ControllerBinding, MachineError> {
    let Some(pending) = last_unresolved_prepare(run_root, run_id)? else {
        return Ok(binding);
    };
    if binding.identity.generation > pending.controller_generation {
        // `controller.json` names a generation the journal never confirmed, so
        // the binding proves nothing and no caller — reset included — may act
        // on it.  That is a recovery condition, not a statement about a reset.
        return Err(recovery_required(
            run_id,
            binding.identity.generation,
            "controller_binding_newer_than_reset_journal",
        ));
    }
    if purpose == BindingPurpose::Mutation {
        // SPEC-013's fsynced reset token owns this Run's startup/mutation
        // serialization until the reset resolves.  `CONTROLLER_RESET_NOT_ALLOWED`
        // is the answer to `run controller reset`; an ordinary mutation that
        // loses to that owner is simply busy, and may come back.
        return Err(run_busy(
            run_id,
            "startup",
            "an unresolved controller reset owns this run's mutation serialization",
        ));
    }
    Ok(binding)
}

/// Whether this Run carries a durable, unresolved controller reset prepare.
///
/// The worker uses it as the whole authorization for answering a reset fence:
/// writing that record requires the operator capability under `operator.lock`
/// and the Run's startup lock, and the same record is what
/// `reconcile_binding_with_journal` fails every mutation closed on.
pub fn run_reset_prepare_is_pending(state_root: &Path, run_id: Uuid) -> Result<bool, MachineError> {
    let run_root = state_root.join("runs").join(run_id.to_string());
    Ok(last_unresolved_prepare(&run_root, run_id)?.is_some())
}

/// Load controller authority through the reset-recovery gate.
///
/// Every production authorization path must use this function rather than
/// trusting `controller.json` directly: an unresolved durable reset prepare
/// can prove that the binding advanced without a committed authority token.
pub fn load_reconciled_controller_binding(
    state_root: &Path,
    run_id: Uuid,
) -> Result<ControllerBinding, MachineError> {
    load_binding_for(state_root, run_id, BindingPurpose::Mutation)
}

/// Load controller authority for the reset that will resolve the journal.
///
/// Identical to the mutation gate except that it does not treat its own
/// unresolved prepare as a fence: a crashed reset has to remain recoverable by
/// another reset.
fn load_controller_binding_for_reset(
    state_root: &Path,
    run_id: Uuid,
) -> Result<ControllerBinding, MachineError> {
    load_binding_for(state_root, run_id, BindingPurpose::Reset)
}

fn load_binding_for(
    state_root: &Path,
    run_id: Uuid,
    purpose: BindingPurpose,
) -> Result<ControllerBinding, MachineError> {
    let binding =
        RunStore::new(SystemWorkspacePlatform, state_root).load_controller_binding(run_id)?;
    reconcile_binding_with_journal(
        &state_root.join("runs").join(run_id.to_string()),
        run_id,
        binding,
        purpose,
    )
}

struct DurableResetJournal {
    run_id: Uuid,
    journal_path: PathBuf,
    controller_path: PathBuf,
    previous: ControllerBinding,
}

impl DurableResetJournal {
    fn new(run_id: Uuid, run_root: &Path, previous: ControllerBinding) -> Self {
        Self {
            run_id,
            journal_path: run_root.join("recovery/controller-reset.jsonl"),
            controller_path: run_root.join("controller.json"),
            previous,
        }
    }

    fn blocked(&self, blocker: &str) -> MachineError {
        reset_not_allowed(
            self.run_id,
            RunLifecycle::ReconciliationRequired,
            vec![blocker.to_owned()],
        )
    }

    fn append(&self, value: &serde_json::Value) -> Result<(), MachineError> {
        let serialized = serde_json::to_string(value)
            .map_err(|_| self.blocked("reset_journal_encode_failed"))?;
        let mut bytes = canonicalize(
            &parse(&serialized).map_err(|_| self.blocked("reset_journal_encode_failed"))?,
        )
        .map_err(|_| self.blocked("reset_journal_encode_failed"))?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.journal_path)
            .map_err(|_| self.blocked("reset_journal_write_failed"))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| self.blocked("reset_journal_write_failed"))
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
        atomic_replace_binding(self.run_id, &self.controller_path, binding)?;
        let append_result = self.append(&serde_json::json!({
            "schema_version": 1,
            "operation_id": record.operation_id,
            "status": "committed",
            "run_id": record.run_id,
            "old_controller_id": record.old_controller_id,
            "new_controller_id": record.new_controller_id,
            "new_generation": record.new_generation,
            "writer_released": record.writer_released,
        }));
        commit_or_reconcile(self.run_id, append_result, || {
            atomic_replace_binding(self.run_id, &self.controller_path, &self.previous)
        })
    }

    fn fail(&mut self, operation_id: Uuid) -> Result<(), MachineError> {
        self.append(&serde_json::json!({
            "schema_version": 1,
            "operation_id": operation_id,
            "status": "failed",
        }))
    }
}

/// Resolves the outcome of the durable commit-token append: `Ok(())` if it
/// succeeded, otherwise attempts `restore` (rolling the binding file back to
/// the previous controller) and returns an error either way. A restore
/// failure is never discarded: at that point disk holds the new controller
/// binding with no durable "committed" record, so the combined error
/// carries both failure messages and reports the typed
/// `reconciliation_required` state rather than the original (now
/// misleading, since the binding did not actually roll back) append error.
fn commit_or_reconcile(
    run_id: Uuid,
    append_result: Result<(), MachineError>,
    restore: impl FnOnce() -> Result<(), MachineError>,
) -> Result<(), MachineError> {
    let Err(append_error) = append_result else {
        return Ok(());
    };
    Err(match restore() {
        Ok(()) => append_error,
        Err(restore_error) => reset_not_allowed(
            run_id,
            RunLifecycle::ReconciliationRequired,
            vec![
                describe_failure("reset_journal_commit_failed", &append_error),
                describe_failure("controller_binding_restore_failed", &restore_error),
            ],
        ),
    })
}

/// Renders an underlying error for inclusion in a `blockers` list, pulling
/// forward its own nested `blockers` (if it carries any, as every
/// `reset_not_allowed` error does) so a combined reconciliation error stays
/// specific instead of collapsing to a shared generic message.
fn describe_failure(prefix: &str, error: &MachineError) -> String {
    let detail = error
        .details
        .get("blockers")
        .and_then(serde_json::Value::as_array)
        .map(|blockers| {
            blockers
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|joined| !joined.is_empty())
        .or_else(|| {
            error
                .details
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    match detail {
        Some(detail) => format!("{prefix}: {} [{detail}]", error.message),
        None => format!("{prefix}: {}", error.message),
    }
}

fn atomic_replace_binding(
    run_id: Uuid,
    path: &Path,
    binding: &ControllerBinding,
) -> Result<(), MachineError> {
    let blocked = |blocker: &str| {
        reset_not_allowed(
            run_id,
            RunLifecycle::ReconciliationRequired,
            vec![blocker.to_owned()],
        )
    };
    let parent = path
        .parent()
        .ok_or_else(|| blocked("controller_binding_path_invalid"))?;
    let temporary = parent.join(format!(".controller-{}.tmp", new_uuid_v7()));
    let bytes =
        serde_json::to_vec(binding).map_err(|_| blocked("controller_binding_encode_failed"))?;
    create_credential_file(&temporary, &bytes)?;
    fs::rename(&temporary, path).map_err(|_| blocked("controller_binding_rename_failed"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| blocked("controller_binding_directory_sync_failed"))
}

pub fn default_operator_root() -> Result<PathBuf, MachineError> {
    Ok(DolgoraeHome::system()?.operator_root())
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
        "human-cli" => Ok(ControllerKind::HumanCli),
        "interactive-client" => Ok(ControllerKind::InteractiveClient),
        "workflow-orchestrator" => Ok(ControllerKind::WorkflowOrchestrator),
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
            .map_err(|_| {
                credential_invalid("credential-file", "credential file cannot be opened")
            })?;
        Self::from_file(file)
    }

    /// Capture a protected client carrier beneath the advertised private root.
    pub(crate) fn open_confined(root: &Path, path: &Path) -> Result<Self, MachineError> {
        let relative = path.strip_prefix(root).map_err(|_| {
            MachineError::invalid_argument(
                "controller.absolute_file_path",
                "carrier must be beneath the advertised root",
            )
        })?;
        let components = relative.components().collect::<Vec<_>>();
        if components.len() < 3
            || components
                .iter()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(MachineError::invalid_argument(
                "controller.absolute_file_path",
                "carrier must be in a client descendant directory",
            ));
        }
        let uid = DarwinSystem.current_uid();
        let mut directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .map_err(|_| {
                MachineError::invalid_argument("controller", "carrier directory cannot be opened")
            })?;
        for part in root.components() {
            if let Component::Normal(name) = part {
                directory = DarwinSystem
                    .openat_nofollow(&directory, name, true)
                    .map_err(|_| {
                        MachineError::invalid_argument("controller", "carrier root is unsafe")
                    })?;
            }
        }
        for (index, part) in components.iter().enumerate() {
            let metadata = directory.metadata().map_err(|_| {
                MachineError::invalid_argument(
                    "controller",
                    "carrier directory cannot be inspected",
                )
            })?;
            if metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
                return Err(MachineError::invalid_argument(
                    "controller",
                    "carrier directories require same-owner mode 0700",
                ));
            }
            let Component::Normal(name) = part else {
                unreachable!()
            };
            let leaf = index + 1 == components.len();
            let file = DarwinSystem
                .openat_nofollow(&directory, name, !leaf)
                .map_err(|_| {
                    MachineError::invalid_argument("controller", "carrier path is unsafe")
                })?;
            if leaf {
                return Self::from_received_fd(file.into());
            }
            directory = file;
        }
        unreachable!()
    }

    pub fn duplicate_fd(fd: RawFd) -> Result<Self, MachineError> {
        let owned = DarwinSystem.duplicate_fd_cloexec(fd).map_err(|_| {
            credential_invalid(
                "credential-fd",
                "credential descriptor cannot be duplicated",
            )
        })?;
        Self::from_file(File::from(owned))
    }

    pub fn from_received_fd(fd: std::os::fd::OwnedFd) -> Result<Self, MachineError> {
        Self::from_file(File::from(fd))
    }

    fn from_file(file: File) -> Result<Self, MachineError> {
        let snapshot = secure_snapshot(&file)?;
        Ok(Self {
            file,
            snapshot,
            expected_generation: None,
        })
    }

    pub fn with_expected_generation(mut self, generation: u64) -> Self {
        self.expected_generation = Some(generation);
        self
    }

    pub fn expected_generation(&self) -> Option<u64> {
        self.expected_generation
    }

    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        use std::os::fd::AsRawFd as _;
        self.file.as_raw_fd()
    }

    fn reread(&self) -> Result<Zeroizing<Vec<u8>>, MachineError> {
        let before = secure_snapshot(&self.file)?;
        if before != self.snapshot {
            return Err(credential_invalid(
                "credential",
                "credential file changed since it was opened",
            ));
        }
        let capacity = usize::try_from(before.size)
            .map_err(|_| credential_invalid("credential", "credential file size is invalid"))?;
        let mut bytes = Zeroizing::new(vec![0_u8; capacity]);
        let count = self
            .file
            .read_at(&mut bytes, 0)
            .map_err(|_| credential_invalid("credential", "credential file could not be read"))?;
        if count != capacity || secure_snapshot(&self.file)? != before {
            return Err(credential_invalid(
                "credential",
                "credential file changed while it was read",
            ));
        }
        Ok(bytes)
    }
}

fn secure_snapshot(file: &File) -> Result<FileSnapshot, MachineError> {
    let metadata = file
        .metadata()
        .map_err(|_| credential_invalid("credential", "credential file metadata is unavailable"))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() == 0
        || metadata.len() > CREDENTIAL_MAX_BYTES
    {
        return Err(credential_invalid(
            "credential",
            "credential file must be a same-uid private regular file within size bounds",
        ));
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

fn credential_invalid(argument: impl Into<String>, reason: impl Into<String>) -> MachineError {
    MachineError::invalid_argument(argument, reason)
}

fn controller_mismatch(run_id: Uuid, operation: impl Into<String>) -> MachineError {
    MachineError::new(
        "CONTROLLER_MISMATCH",
        "controller credential does not authorize this operation",
        false,
        serde_json::json!({"run_id": run_id, "operation": operation.into()}),
    )
}

fn operator_error(operation: impl Into<String>) -> MachineError {
    MachineError::new(
        "OPERATOR_MISMATCH",
        "operator credential does not authorize this operation",
        false,
        serde_json::json!({"operation": operation.into()}),
    )
}

fn parse_controller(bytes: &[u8]) -> Result<ControllerSecret, MachineError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        credential_invalid("controller-credential", "credential payload is not UTF-8")
    })?;
    let lossless = ZeroizingJson(parse(text).map_err(|_| {
        credential_invalid(
            "controller-credential",
            "credential payload is not valid JSON",
        )
    })?);
    let canonical = Zeroizing::new(canonicalize(&lossless.0).map_err(|_| {
        credential_invalid(
            "controller-credential",
            "credential payload is not canonicalizable",
        )
    })?);
    let wire: ControllerWire = serde_json::from_slice(&canonical).map_err(|_| {
        credential_invalid(
            "controller-credential",
            "credential payload does not match the controller credential schema",
        )
    })?;
    if wire.schema_version != 1 || wire.controller_id.get_version_num() != 7 {
        return Err(credential_invalid(
            "controller-credential",
            "credential schema version or controller id is unsupported",
        ));
    }
    validate_text(&wire.instance_id, 128, false, "instance_id")?;
    if let Some(subject) = &wire.subject_id {
        validate_text(subject, 256, false, "subject_id")?;
    }
    if let Some(launch) = &wire.orchestration_launch
        && (!matches!(
            wire.kind,
            ControllerKind::HumanCli | ControllerKind::InteractiveClient
        ) || launch.use_case != "dolgorae_orchestrated_session"
            || !valid_policy_name(&launch.specialist_policy_name))
    {
        return Err(credential_invalid(
            "orchestration_launch",
            "orchestration launch is invalid for this controller kind or policy name",
        ));
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
    let text = std::str::from_utf8(bytes).map_err(|_| {
        credential_invalid("operator-credential", "credential payload is not UTF-8")
    })?;
    let lossless = ZeroizingJson(parse(text).map_err(|_| {
        credential_invalid(
            "operator-credential",
            "credential payload is not valid JSON",
        )
    })?);
    let canonical = Zeroizing::new(canonicalize(&lossless.0).map_err(|_| {
        credential_invalid(
            "operator-credential",
            "credential payload is not canonicalizable",
        )
    })?);
    let wire: OperatorWire = serde_json::from_slice(&canonical).map_err(|_| {
        credential_invalid(
            "operator-credential",
            "credential payload does not match the operator credential schema",
        )
    })?;
    if wire.schema_version != 1 || wire.operator_id.get_version_num() != 7 {
        return Err(credential_invalid(
            "operator-credential",
            "credential schema version or operator id is unsupported",
        ));
    }
    Ok(OperatorSecret {
        operator_id: wire.operator_id,
        capability: decode_capability(&wire.capability)?,
    })
}

fn validate_text(
    value: &str,
    maximum_bytes: usize,
    allow_empty: bool,
    field: &str,
) -> Result<(), MachineError> {
    if (!allow_empty && value.is_empty())
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(credential_invalid(
            field,
            "text field is empty, too long, or contains control characters",
        ));
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
        return Err(credential_invalid(
            "capability",
            "capability encoding has the wrong length or characters",
        ));
    }
    let mut decoded = Zeroizing::new([0_u8; 32]);
    let count = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode_slice(encoded, &mut *decoded)
        .map_err(|_| credential_invalid("capability", "capability is not valid base64url"))?;
    if count != 32 || base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*decoded) != encoded {
        return Err(credential_invalid(
            "capability",
            "capability does not decode to exactly 32 bytes in canonical encoding",
        ));
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
                false,
                serde_json::json!({"invariant": "secure randomness source /dev/urandom is unavailable"}),
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
    validate_text(&instance_id, 128, false, "--instance-id")?;
    if let Some(subject) = &subject_id {
        validate_text(subject, 256, false, "--subject-id")?;
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
    let serialized = Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| {
        credential_invalid(
            "controller-credential",
            "credential could not be serialized",
        )
    })?);
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
    let parent = path
        .parent()
        .ok_or_else(|| credential_invalid("output", "output path has no parent directory"))?;
    let metadata = fs::metadata(parent)
        .map_err(|_| credential_invalid("output", "output directory is unavailable"))?;
    if !metadata.is_dir()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(credential_invalid(
            "output",
            "output directory must be a private uid-owned directory with mode 0700",
        ));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| {
            credential_invalid("output", "output file already exists or cannot be created")
        })?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(credential_invalid(
            "output",
            "credential could not be written",
        ));
    }
    if File::open(parent)
        .and_then(|directory| directory.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(credential_invalid(
            "output",
            "output directory could not be synced",
        ));
    }
    Ok(())
}

pub fn authorize_controller(
    run_id: Uuid,
    operation: &str,
    binding: &ControllerBinding,
    carrier: &CredentialCarrier,
) -> Result<ControllerIdentity, MachineError> {
    let bytes = carrier
        .reread()
        .map_err(|_| controller_mismatch(run_id, operation))?;
    let secret = parse_controller(&bytes).map_err(|_| controller_mismatch(run_id, operation))?;
    let observed = controller_capability_digest(&secret.capability);
    let identity_matches = secret.public.controller_id == binding.identity.controller_id;
    let capability_matches =
        constant_time_equal(observed.as_bytes(), binding.capability_sha256.as_bytes());
    if !(identity_matches & capability_matches)
        || carrier
            .expected_generation
            .is_some_and(|generation| generation != binding.identity.generation)
    {
        return Err(controller_mismatch(run_id, operation));
    }
    Ok(binding.identity.clone())
}

/// Builds a `ControllerBinding` after verifying `carrier` matches an
/// already-known, independently sourced `public` identity claim — e.g. the
/// incumbent's binding during a controller reset, where the replacement
/// credential's principal must match the outgoing one.
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
        return Err(credential_invalid(
            "controller",
            "credential does not match the supplied controller identity",
        ));
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

/// Builds the initial `ControllerBinding` directly from a freshly presented
/// credential carrier, with no prior public identity to cross-check —
/// appropriate for minting a new run's first binding, where the credential
/// itself is the sole source of truth for who the controller is.
pub fn binding_from_carrier(
    carrier: &CredentialCarrier,
    generation: u64,
) -> Result<ControllerBinding, MachineError> {
    let bytes = carrier.reread()?;
    let secret = parse_controller(&bytes)?;
    Ok(ControllerBinding {
        identity: ControllerIdentity {
            controller_id: secret.public.controller_id,
            kind: secret.public.kind,
            instance_id: secret.public.instance_id,
            subject_id: secret.public.subject_id,
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
    run_id: Uuid,
    binding: Mutex<ControllerBinding>,
    state_revision: Mutex<u64>,
    lifecycle: RunLifecycle,
}

impl RunAuthority {
    #[must_use]
    pub fn new(
        run_id: Uuid,
        binding: ControllerBinding,
        state_revision: u64,
        lifecycle: RunLifecycle,
    ) -> Self {
        Self {
            run_id,
            binding: Mutex::new(binding),
            state_revision: Mutex::new(state_revision),
            lifecycle,
        }
    }

    pub fn mutate<T>(
        &self,
        operation: RunOperation,
        expected_revision: u64,
        carrier: &CredentialCarrier,
        effect: impl FnOnce() -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        let operation_name = format!("{operation:?}");
        if !operation.requires_controller() {
            return Err(MachineError::new(
                "INTERNAL_ERROR",
                "mutation operation is not classified",
                false,
                serde_json::json!({"invariant": format!("{operation_name} has no mutation classification")}),
            ));
        }
        let binding = self
            .binding
            .lock()
            .map_err(|_| state_conflict(self.run_id, self.lifecycle, &operation_name))?;
        let revision = self
            .state_revision
            .lock()
            .map_err(|_| state_conflict(self.run_id, self.lifecycle, &operation_name))?;
        if *revision != expected_revision {
            return Err(state_conflict(self.run_id, self.lifecycle, &operation_name));
        }
        authorize_controller(self.run_id, &operation_name, &binding, carrier)?;
        effect()
    }
}

fn state_conflict(run_id: Uuid, state: RunLifecycle, operation: &str) -> MachineError {
    MachineError::new(
        "RUN_STATE_CONFLICT",
        "run state changed",
        false,
        serde_json::json!({"run_id": run_id, "state": state, "operation": operation}),
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

/// Exclusive hold on the one global `operator.lock`.
///
/// `flock` is tied to the open file description, so a second `open` of the same
/// path — even from this same process — waits behind it. `Drop` issues an
/// explicit `LOCK_UN` before closing the descriptor: close alone is insufficient
/// when a concurrent fork inherited another reference before exec. Re-entry is
/// therefore a deadlock rather than a no-op, so anything that must act while a
/// hold is already alive is reached through a `*_holding` entry point that
/// reuses the hold instead of reopening the path.
#[derive(Debug)]
#[must_use = "operator.lock is held only while this value is alive"]
pub(crate) struct OperatorLock {
    file: File,
}

impl OperatorLock {
    /// Releases `operator.lock` at an explicit point rather than wherever the
    /// enclosing scope happens to end.
    pub(crate) fn release(self) {
        drop(self);
    }
}

impl Drop for OperatorLock {
    fn drop(&mut self) {
        // The descriptor is live and was verified as a regular private file
        // before the lock was taken. Closing remains the final fallback if an
        // invariant violation nevertheless makes the explicit unlock fail.
        let _ = DarwinSystem.unlock(&self.file);
    }
}

/// Proof that the operator credential authorized this operation, carrying the
/// `operator.lock` hold that keeps the proof true.
///
/// SPEC-013 and ADR-016 require the authoritative consumer to reread the
/// capability descriptor and revalidate ID, generation, and digest under
/// `operator.lock` *before* acquiring home/server/run locks or causing
/// effects. Handing the hold back with the identity turns that into a
/// type-level obligation: the value has to stay alive until the authorized
/// effect reaches its handoff boundary — the next lock in the hierarchy, or
/// the fsynced prepare/commit — so a concurrent rotation cannot revoke the
/// generation in between.
///
/// While one of these is alive, never call an `OperatorStore` method that
/// takes the lock itself; the deadlock-free way to act under an existing hold
/// is a `*_holding` entry point.
#[derive(Debug)]
#[must_use = "the operator.lock hold must outlive the authorized effect's handoff boundary"]
pub struct OperatorAuthorization {
    lock: OperatorLock,
    public: OperatorPublic,
}

impl OperatorAuthorization {
    /// Releases `operator.lock` once the authorized effect has reached its
    /// handoff boundary.
    pub fn release(self) {
        self.lock.release();
    }
}

impl std::ops::Deref for OperatorAuthorization {
    type Target = OperatorPublic;

    fn deref(&self) -> &Self::Target {
        &self.public
    }
}

impl OperatorStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Takes the global `operator.lock`, blocking until it is free.
    fn lock_exclusive(&self) -> Result<OperatorLock, MachineError> {
        self.ensure_root()?;
        let file = self.open_lock()?;
        DarwinSystem
            .lock_exclusive(&file)
            .map_err(|_| operator_error("operator.lock"))?;
        Ok(OperatorLock { file })
    }

    /// Takes the global `operator.lock` only if it is free right now.
    ///
    /// Contention is reported as `None` rather than an error so a test can
    /// observe whether a hold is live at an exact point without confusing
    /// "someone else holds it" with a permission or integrity failure.
    #[cfg(test)]
    pub(crate) fn try_lock_exclusive(&self) -> Result<Option<OperatorLock>, MachineError> {
        self.ensure_root()?;
        let file = self.open_lock()?;
        loop {
            match DarwinSystem.lock_exclusive_nonblocking(&file) {
                Ok(()) => return Ok(Some(OperatorLock { file })),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(operator_error("operator.lock")),
            }
        }
    }

    pub fn initialize(
        &self,
        output: &Path,
    ) -> Result<CredentialCreated<OperatorPublic>, MachineError> {
        let lock = self.lock_exclusive()?;
        let created = self.initialize_holding(&lock, output);
        lock.release();
        created
    }

    /// The body of `initialize`, split out so it borrows the caller's hold
    /// rather than reopening `operator.lock` — reopening here would wait on
    /// the hold the wrapper above already owns.
    fn initialize_holding(
        &self,
        _lock: &OperatorLock,
        output: &Path,
    ) -> Result<CredentialCreated<OperatorPublic>, MachineError> {
        if self.state_path().exists() {
            // `initialize` is create-exclusive and this installation already
            // has a registered operator credential; replacing it requires the
            // current one through `rotate`, which is exactly the registered
            // "supplied operator capability is absent" refusal.
            return Err(operator_error("operator.credential.initialize"));
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
            .map_err(|_| operator_error("operator.initialize"))?,
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
        let authorization = self.authorize(carrier)?;
        let rotated = self.rotate_holding(&authorization, output);
        authorization.release();
        rotated
    }

    /// The body of `rotate`, split out for the same reason. Rotation is
    /// itself an operator-authorized effect, so the wrapper above reuses the
    /// authorization's hold instead of taking a second one: the reread, the
    /// constant-time comparison, and the published new generation all happen
    /// without ever releasing `operator.lock`.
    fn rotate_holding(
        &self,
        authorization: &OperatorAuthorization,
        output: &Path,
    ) -> Result<CredentialCreated<OperatorPublic>, MachineError> {
        let capability = random_capability()?;
        let encoded =
            Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*capability));
        let serialized = Zeroizing::new(
            serde_json::to_vec(&OperatorWireRef {
                schema_version: 1,
                operator_id: authorization.operator_id,
                capability: &encoded,
            })
            .map_err(|_| operator_error("operator.rotate"))?,
        );
        create_credential_file(output, &serialized)?;
        let next = OperatorState {
            schema_version: 1,
            operator_id: authorization.operator_id,
            generation: authorization
                .operator_generation
                .checked_add(1)
                .ok_or_else(|| operator_error("operator.rotate"))?,
            capability_sha256: operator_digest(&capability),
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

    /// Authorizes the operator credential and keeps `operator.lock` held.
    ///
    /// The returned hold is the caller's obligation, not a convenience: it
    /// must stay alive until the authorized effect reaches its handoff
    /// boundary. Dropping it right away leaves only a non-authoritative
    /// pre-check, which SPEC-013 says can never authorize an operation
    /// across a concurrent rotation.
    pub fn authorize(
        &self,
        carrier: &CredentialCarrier,
    ) -> Result<OperatorAuthorization, MachineError> {
        let lock = self.lock_exclusive()?;
        self.authorize_holding(lock, carrier)
    }

    /// The body of `authorize`: it consumes the hold and hands it back
    /// inside the authorization, so the reread and constant-time comparison
    /// share one unbroken hold with whatever effect follows.
    fn authorize_holding(
        &self,
        lock: OperatorLock,
        carrier: &CredentialCarrier,
    ) -> Result<OperatorAuthorization, MachineError> {
        let state = self.load_state()?;
        // `authorize_operator_state` owns both the reread bytes and the
        // parsed secret and drops them before it returns, so no capability
        // material is still live when the caller goes on to take the next
        // lock or apply the effect.
        authorize_operator_state(&state, carrier, "operator.authorize")?;
        Ok(OperatorAuthorization {
            lock,
            public: OperatorPublic {
                operator_id: state.operator_id,
                operator_generation: state.generation,
            },
        })
    }

    fn ensure_root(&self) -> Result<(), MachineError> {
        if !self.root.exists() {
            let mut missing = Vec::new();
            let mut candidate = self.root.as_path();
            while !candidate.exists() {
                missing.push(candidate.to_path_buf());
                candidate = candidate
                    .parent()
                    .ok_or_else(|| operator_error("operator.root"))?;
            }
            for directory in missing.into_iter().rev() {
                fs::create_dir(&directory).map_err(|_| operator_error("operator.root"))?;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                    .map_err(|_| operator_error("operator.root"))?;
            }
        }
        let metadata =
            fs::symlink_metadata(&self.root).map_err(|_| operator_error("operator.root"))?;
        if !metadata.is_dir()
            || metadata.uid() != DarwinSystem.current_uid()
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(operator_error("operator.root"));
        }
        if let Some(parent) = self.root.parent() {
            let parent_metadata =
                fs::symlink_metadata(parent).map_err(|_| operator_error("operator.root"))?;
            if !parent_metadata.is_dir()
                || parent_metadata.uid() != DarwinSystem.current_uid()
                || parent_metadata.permissions().mode() & 0o077 != 0
            {
                return Err(operator_error("operator.root"));
            }
        }
        Ok(())
    }

    fn open_lock(&self) -> Result<File, MachineError> {
        let path = self.root.join("operator.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| operator_error("operator.lock"))?;
        let metadata = file
            .metadata()
            .map_err(|_| operator_error("operator.lock"))?;
        if !metadata.is_file()
            || metadata.uid() != DarwinSystem.current_uid()
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(operator_error("operator.lock"));
        }
        Ok(file)
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("operator.json")
    }

    fn load_state(&self) -> Result<OperatorState, MachineError> {
        let carrier = CredentialCarrier::open_path(&self.state_path())
            .map_err(|_| operator_error("operator.state.load"))?;
        let bytes = carrier
            .reread()
            .map_err(|_| operator_error("operator.state.load"))?;
        let value =
            parse(std::str::from_utf8(&bytes).map_err(|_| operator_error("operator.state.load"))?)
                .map_err(|_| operator_error("operator.state.load"))?;
        let canonical = canonicalize(&value).map_err(|_| operator_error("operator.state.load"))?;
        let state: OperatorState = serde_json::from_slice(&canonical)
            .map_err(|_| operator_error("operator.state.load"))?;
        if state.schema_version != 1
            || state.operator_id.get_version_num() != 7
            || state.generation == 0
            || state.capability_sha256.len() != 64
        {
            return Err(operator_error("operator.state.load"));
        }
        Ok(state)
    }

    fn write_state_create(&self, state: &OperatorState) -> Result<(), MachineError> {
        create_credential_file(
            &self.state_path(),
            &serde_json::to_vec(state).map_err(|_| operator_error("operator.state.write"))?,
        )
        .map_err(|_| operator_error("operator.state.write"))
    }

    fn write_state_replace(&self, state: &OperatorState) -> Result<(), MachineError> {
        let temporary = self.root.join(format!(".operator-{}.tmp", new_uuid_v7()));
        create_credential_file(
            &temporary,
            &serde_json::to_vec(state).map_err(|_| operator_error("operator.state.write"))?,
        )?;
        fs::rename(&temporary, self.state_path())
            .map_err(|_| operator_error("operator.state.write"))?;
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| operator_error("operator.state.write"))
    }
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
    operation: &str,
) -> Result<(), MachineError> {
    let bytes = carrier.reread().map_err(|_| operator_error(operation))?;
    let secret = parse_operator(&bytes).map_err(|_| operator_error(operation))?;
    let observed = operator_digest(&secret.capability);
    let identity_matches = secret.operator_id == state.operator_id;
    let capability_matches =
        constant_time_equal(observed.as_bytes(), state.capability_sha256.as_bytes());
    if !(identity_matches & capability_matches) {
        return Err(operator_error(operation));
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

/// The live-Run facts a controller reset cannot get from durable files alone.
///
/// SPEC-013's lock prefix for `run controller reset` is "Operator, run
/// startup/mutation".  The startup half is a file range this process can take;
/// the mutation half is the in-process serializer that lives inside the Run's
/// *worker*, in another process, and can only be reached by asking it.  Both
/// arrive through this trait rather than being reached for directly: `worker`
/// already depends on `controller` for ADR-016 authority, so the reverse edge
/// would be a cycle, and the composition layer that is allowed to name both is
/// the one that supplies this.
pub trait RunResetEnvironment {
    /// Open the Run's startup/mutation lock.  Opening does not acquire it.
    fn open_run_lock(
        &self,
        state_root: &Path,
        run_id: Uuid,
    ) -> Result<Box<dyn RunMutationLock>, MachineError>;

    /// An opaque fingerprint of the worker answering for this Run right now,
    /// or `None` when none answers.  Equal fingerprints mean the same worker;
    /// the reset reproduces it under its own lock before trusting it.
    fn worker_fingerprint(&self, state_root: &Path, run_id: Uuid) -> Option<String>;

    /// Ask the live worker's own mutation serializer what the Run
    /// authoritatively is, under the durable fence this reset has already
    /// written.  `Ok(())` means the Run holds no live work and the reset may
    /// commit.
    fn fence_live_worker(
        &self,
        state_root: &Path,
        run_id: Uuid,
        confirmation: Uuid,
    ) -> Result<(), MachineError>;
}

/// The Run's startup/mutation lock, as the controller reset holds it.
///
/// SPEC-013's prefix for `run controller reset` is "Operator, run
/// startup/mutation".  This is the second half; the trait exists so the
/// ordering is testable without a real Run directory, and so the reset never
/// has to know whether the lock is a file range or something else.
pub trait RunMutationLock {
    /// Take the lock for one PREPARE or COMMIT prefix.
    fn acquire(&self) -> Result<(), MachineError>;
    /// Release it.  APPLY performs external work and must hold no
    /// coordination lock at all.
    fn release(&self);
}

/// A reset that serializes against nothing, for callers that have already
/// proven exclusivity another way.
pub struct UnlockedRun;

impl RunMutationLock for UnlockedRun {
    fn acquire(&self) -> Result<(), MachineError> {
        Ok(())
    }
    fn release(&self) {}
}

/// Everything a controller reset serializes against, in the order SPEC-013
/// requires: `operator.lock` first, then the Run's startup/mutation lock.
pub struct ResetAuthority<'a> {
    pub operator_store: &'a OperatorStore,
    pub operator: &'a CredentialCarrier,
    pub run_lock: &'a dyn RunMutationLock,
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

/// Replace a Run's Controller under the operator capability.
///
/// The phases follow SPEC-013 exactly:
///
/// * **PREPARE** holds `operator.lock` and then the Run's startup/mutation
///   lock — in that order, never the reverse — across the observation, the
///   blocker decision, and the fsynced reset token.  `observe` runs *inside*
///   that prefix: whatever the caller decides resettability from has to be
///   read under the same hold that fences the decision, or an idle-looking
///   projection can race a worker accepting a Turn under the old binding.
/// * **APPLY** runs under no coordination lock at all, because it inspects and
///   interrupts live work.  Its refusal is a rollback, not a failure to
///   authorize.
/// * **COMMIT** reacquires the identical prefix and revalidates both the
///   operator credential and the Run lock before installing anything.
pub fn reset_controller(
    state: &mut ControllerResetState,
    confirm_run_id: Uuid,
    authority: &ResetAuthority<'_>,
    replacement: &CredentialCarrier,
    journal: &mut impl ResetJournal,
    observe: impl FnOnce(&mut ControllerResetState) -> Result<(), MachineError>,
    apply: impl FnOnce() -> Result<(), MachineError>,
) -> Result<ControllerResetRecord, MachineError> {
    let ResetAuthority {
        operator_store,
        operator,
        run_lock,
    } = *authority;
    // PREPARE holds `operator.lock` from the credential reread through the
    // fsynced reset token, so a rotation that lands between the two cannot
    // let a revoked generation stake the claim.
    let prepare_authorization = operator_store.authorize(operator)?;
    // Then, and only then, the Run's startup/mutation lock: acquiring it first
    // would be the upward acquisition SPEC-013 forbids.
    if let Err(error) = run_lock.acquire() {
        prepare_authorization.release();
        return Err(error);
    }
    let observed = observe(state);
    if let Err(error) = observed {
        run_lock.release();
        return Err(error);
    }
    if confirm_run_id != state.run_id
        || !matches!(
            state.lifecycle,
            RunLifecycle::Idle | RunLifecycle::Paused | RunLifecycle::OutcomeUnknown
        )
        || state.pending_interaction
        || state.handoff_active
        || !state.generation_verifiable
    {
        let mut blockers = Vec::new();
        if confirm_run_id != state.run_id {
            blockers.push("confirmation_mismatch".to_owned());
        }
        if !matches!(
            state.lifecycle,
            RunLifecycle::Idle | RunLifecycle::Paused | RunLifecycle::OutcomeUnknown
        ) {
            blockers.push("lifecycle_not_resettable".to_owned());
        }
        if state.pending_interaction {
            blockers.push("pending_interaction".to_owned());
        }
        if state.handoff_active {
            blockers.push("handoff_active".to_owned());
        }
        if !state.generation_verifiable {
            blockers.push("writer_generation_unverifiable".to_owned());
        }
        run_lock.release();
        return Err(reset_not_allowed(state.run_id, state.lifecycle, blockers));
    }
    let replacement_identity = (|| {
        let bytes = replacement.reread().ok()?;
        parse_controller(&bytes).ok()
    })();
    let Some(next) = replacement_identity else {
        run_lock.release();
        return Err(controller_mismatch(state.run_id, "run.controller.reset"));
    };
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
        run_lock.release();
        return Err(controller_mismatch(state.run_id, "run.controller.reset"));
    }
    let operation_id = new_uuid_v7();
    if let Err(error) = journal.prepare(operation_id, state) {
        run_lock.release();
        return Err(error);
    }
    // APPLY interrupts and inspects live work, so it must run under no
    // coordination lock at all.  The durable prepare is what fences the Run
    // from here on: every worker mutation reconciles against it and fails
    // closed until this operation resolves.
    prepare_authorization.release();
    run_lock.release();
    if let Err(error) = apply() {
        // Resolving our own prepared token terminally is a rollback, not a
        // new operator-authorized effect, so it still proceeds when a
        // concurrent rotation has already revoked the generation that
        // prepared it.
        let _ = journal.fail(operation_id);
        return Err(error);
    }
    // COMMIT reacquires the identical prefix and revalidates the credential:
    // a rotation during APPLY must leave the old controller authoritative
    // instead of letting the prepared generation install a new one.
    let commit_authorization = match operator_store.authorize(operator) {
        Ok(authorization) => authorization,
        Err(error) => {
            let _ = journal.fail(operation_id);
            return Err(error);
        }
    };
    if let Err(error) = run_lock.acquire() {
        commit_authorization.release();
        let _ = journal.fail(operation_id);
        return Err(error);
    }
    let Some(new_generation) = state.binding.identity.generation.checked_add(1) else {
        run_lock.release();
        let _ = journal.fail(operation_id);
        return Err(state_conflict(
            state.run_id,
            state.lifecycle,
            "run.controller.reset",
        ));
    };
    let record = ControllerResetRecord {
        operation_id,
        run_id: state.run_id,
        old_controller_id: state.binding.identity.controller_id,
        new_controller_id: next.public.controller_id,
        new_generation,
        writer_released: state.writer_authority,
    };
    let Some(new_revision) = state.state_revision.checked_add(1) else {
        run_lock.release();
        let _ = journal.fail(operation_id);
        return Err(state_conflict(
            state.run_id,
            state.lifecycle,
            "run.controller.reset",
        ));
    };
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
        run_lock.release();
        return Err(error);
    }
    run_lock.release();
    commit_authorization.release();
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_kind_parser_matches_the_public_cli_spellings() {
        assert_eq!(
            parse_controller_kind("human-cli").unwrap(),
            ControllerKind::HumanCli
        );
        assert_eq!(
            parse_controller_kind("interactive-client").unwrap(),
            ControllerKind::InteractiveClient
        );
        assert_eq!(
            parse_controller_kind("workflow-orchestrator").unwrap(),
            ControllerKind::WorkflowOrchestrator
        );
        assert_eq!(
            parse_controller_kind("automation").unwrap(),
            ControllerKind::Automation
        );
        assert_eq!(
            parse_controller_kind("other").unwrap(),
            ControllerKind::Other
        );
        assert!(parse_controller_kind("human_cli").is_err());
    }
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
        let run_id = new_uuid_v7();
        assert_eq!(
            authorize_controller(run_id, "test", &binding, &carrier)
                .unwrap()
                .generation,
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
        let authority = RunAuthority::new(new_uuid_v7(), binding, 4, RunLifecycle::Running);
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
            record: &ControllerResetRecord,
            _: &ControllerBinding,
        ) -> Result<(), MachineError> {
            if self.fail_commit {
                return Err(state_conflict(record.run_id, RunLifecycle::Paused, "test"));
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
                &ResetAuthority {
                    operator_store: &operator_store,
                    operator: &operator,
                    run_lock: &UnlockedRun,
                },
                &replacement,
                &mut journal,
                |_| Ok(()),
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
                &ResetAuthority {
                    operator_store: &operator_store,
                    operator: &operator,
                    run_lock: &UnlockedRun,
                },
                &replacement,
                &mut journal,
                |_| Ok(()),
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
                &ResetAuthority {
                    operator_store: &operator_store,
                    operator: &operator,
                    run_lock: &UnlockedRun,
                },
                &replacement,
                &mut success_journal,
                |_| Ok(()),
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
        let first_authority = RunAuthority::new(new_uuid_v7(), first, 2, RunLifecycle::Running);
        let second_authority = RunAuthority::new(new_uuid_v7(), second, 9, RunLifecycle::Running);
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

    fn matched_operator(root: &Path) -> (OperatorStore, CredentialCarrier) {
        let store = OperatorStore::new(root.join("operator"));
        let operator_path = root.join("operator.json");
        store.initialize(&operator_path).unwrap();
        let operator = CredentialCarrier::open_path(&operator_path).unwrap();
        (store, operator)
    }

    /// An old controller binding plus a same-principal replacement carrier,
    /// matching the identity rule `reset_controller` enforces (same kind
    /// and subject, different controller id).
    fn matched_replacement_pair(root: &Path) -> (ControllerBinding, CredentialCarrier) {
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
        (
            binding_from_credential(&old.credential, 1, &old_carrier).unwrap(),
            replacement,
        )
    }

    fn resettable_state(run_id: Uuid, binding: ControllerBinding) -> ControllerResetState {
        ControllerResetState {
            run_id,
            binding,
            state_revision: 1,
            lifecycle: RunLifecycle::Idle,
            pending_interaction: false,
            handoff_active: false,
            writer_authority: false,
            generation_verifiable: true,
        }
    }

    #[test]
    fn reset_prepare_write_failure_blocks_effect_and_binding_write() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();

        // No `recovery/` directory under run_root: the journal's very first
        // write (`prepare`) fails closed before anything else happens.
        let run_root = root.join("run");
        fs::create_dir(&run_root).unwrap();
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700)).unwrap();

        let mut state = resettable_state(run_id, binding.clone());
        let mut journal = DurableResetJournal::new(run_id, &run_root, binding);
        let effect_ran = std::sync::atomic::AtomicBool::new(false);
        let error = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &UnlockedRun,
            },
            &replacement,
            &mut journal,
            |_| Ok(()),
            || {
                effect_ran.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "CONTROLLER_RESET_NOT_ALLOWED");
        assert_eq!(error.details["state"], "reconciliation_required");
        assert!(!effect_ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!run_root.join("controller.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reset_binding_write_failure_blocks_commit_and_preserves_old_authority_on_disk() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();

        let run_root = root.join("run");
        fs::create_dir(&run_root).unwrap();
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(run_root.join("recovery")).unwrap();
        fs::set_permissions(run_root.join("recovery"), fs::Permissions::from_mode(0o700)).unwrap();
        // Journal writes (a subdirectory) stay reachable; the binding write,
        // which targets run_root itself, does not.
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o500)).unwrap();

        let mut state = resettable_state(run_id, binding.clone());
        let mut journal = DurableResetJournal::new(run_id, &run_root, binding);
        let error = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &UnlockedRun,
            },
            &replacement,
            &mut journal,
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert_eq!(error.details["argument"], "output");

        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!run_root.join("controller.json").exists());
        let journal_text =
            fs::read_to_string(run_root.join("recovery/controller-reset.jsonl")).unwrap();
        assert!(journal_text.contains("\"prepared\""));
        assert!(!journal_text.contains("\"committed\""));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reset_journal_commit_failure_rolls_back_binding_when_restore_succeeds() {
        let root = private_directory();
        let (old_binding, _replacement) = matched_replacement_pair(&root);
        let new_binding = ControllerBinding {
            identity: ControllerIdentity {
                generation: 2,
                ..old_binding.identity.clone()
            },
            ..old_binding.clone()
        };
        let run_id = new_uuid_v7();

        let run_root = root.join("run");
        fs::create_dir(&run_root).unwrap();
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(run_root.join("recovery")).unwrap();
        fs::set_permissions(run_root.join("recovery"), fs::Permissions::from_mode(0o700)).unwrap();

        let mut journal = DurableResetJournal::new(run_id, &run_root, old_binding.clone());
        let operation_id = new_uuid_v7();
        let state = resettable_state(run_id, old_binding.clone());
        journal.prepare(operation_id, &state).unwrap();

        // The "prepared" record already landed; block only the commit-time
        // durable token append.
        fs::set_permissions(
            run_root.join("recovery/controller-reset.jsonl"),
            fs::Permissions::from_mode(0o400),
        )
        .unwrap();

        let record = ControllerResetRecord {
            operation_id,
            run_id,
            old_controller_id: old_binding.identity.controller_id,
            new_controller_id: new_binding.identity.controller_id,
            new_generation: 2,
            writer_released: false,
        };
        let error = journal.commit(&record, &new_binding).unwrap_err();
        assert_eq!(error.code, "CONTROLLER_RESET_NOT_ALLOWED");

        let on_disk: ControllerBinding =
            serde_json::from_slice(&fs::read(run_root.join("controller.json")).unwrap()).unwrap();
        assert_eq!(on_disk, old_binding);

        fs::set_permissions(
            run_root.join("recovery/controller-reset.jsonl"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn commit_or_reconcile_carries_both_errors_when_restore_also_fails() {
        let run_id = new_uuid_v7();
        let append_error = reset_not_allowed(
            run_id,
            RunLifecycle::ReconciliationRequired,
            vec!["reset_journal_write_failed".to_owned()],
        );
        let restore_error = credential_invalid("output", "disk is full");
        let error =
            commit_or_reconcile(run_id, Err(append_error), || Err(restore_error)).unwrap_err();
        assert_eq!(error.code, "CONTROLLER_RESET_NOT_ALLOWED");
        assert_eq!(error.details["state"], "reconciliation_required");
        let blockers: Vec<String> = error.details["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.contains("reset_journal_write_failed"))
        );
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.contains("disk is full"))
        );
    }

    #[test]
    fn commit_or_reconcile_surfaces_original_error_when_restore_succeeds() {
        let run_id = new_uuid_v7();
        let append_error = credential_invalid("output", "append failed for test");
        let error = commit_or_reconcile(run_id, Err(append_error), || Ok(())).unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert_eq!(error.details["reason"], "append failed for test");
    }

    #[test]
    fn reconcile_binding_with_journal_fails_closed_when_binding_outran_the_commit_token() {
        let root = private_directory();
        let (old_binding, _replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let run_root = root.join("run");
        fs::create_dir(&run_root).unwrap();
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(run_root.join("recovery")).unwrap();
        fs::set_permissions(run_root.join("recovery"), fs::Permissions::from_mode(0o700)).unwrap();

        // A crash-truncated journal: the reset that would have produced
        // generation 2 only ever got as far as "prepared".
        let mut journal = DurableResetJournal::new(run_id, &run_root, old_binding.clone());
        let state = resettable_state(run_id, old_binding.clone());
        journal.prepare(new_uuid_v7(), &state).unwrap();

        let advanced_binding = ControllerBinding {
            identity: ControllerIdentity {
                generation: 2,
                ..old_binding.identity.clone()
            },
            ..old_binding
        };
        for purpose in [BindingPurpose::Mutation, BindingPurpose::Reset] {
            let error = reconcile_binding_with_journal(
                &run_root,
                run_id,
                advanced_binding.clone(),
                purpose,
            )
            .unwrap_err();
            // The emitter of `CONTROLLER_RESET_NOT_ALLOWED` is `run controller
            // reset` and its `state` is the run lifecycle; a binding the
            // journal never confirmed is a recovery condition instead, and the
            // checked contract requires exactly these members.
            assert_eq!(error.code, "RECOVERY_REQUIRED");
            assert!(!error.retryable);
            assert_eq!(error.details["run_id"], serde_json::json!(run_id));
            assert_eq!(error.details["generation"], 2);
            assert_eq!(error.details["identity_verdict"], "Unverifiable");
            assert_eq!(
                error.details["reason"], "controller_binding_newer_than_reset_journal",
                "a binding with no committed token is trusted by nobody, reset included"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unresolved_prepare_fences_mutations_and_still_admits_the_reset_that_resolves_it() {
        let root = private_directory();
        let (binding, _replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let run_root = root.join("run");
        fs::create_dir(&run_root).unwrap();
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(run_root.join("recovery")).unwrap();
        fs::set_permissions(run_root.join("recovery"), fs::Permissions::from_mode(0o700)).unwrap();

        let mut journal = DurableResetJournal::new(run_id, &run_root, binding.clone());
        let state = resettable_state(run_id, binding.clone());
        let operation_id = new_uuid_v7();

        // Before the prepare, the binding authorizes mutations.
        assert_eq!(
            reconcile_binding_with_journal(
                &run_root,
                run_id,
                binding.clone(),
                BindingPurpose::Mutation
            )
            .unwrap(),
            binding
        );

        // The fsynced prepare names the generation it is replacing, so the
        // binding is not "newer" than the journal — and it must still stop
        // every mutation until the operation resolves.
        journal.prepare(operation_id, &state).unwrap();
        let fenced = reconcile_binding_with_journal(
            &run_root,
            run_id,
            binding.clone(),
            BindingPurpose::Mutation,
        )
        .unwrap_err();
        // A mutation that loses to the reset's durable fence is busy, not the
        // answer `run controller reset` itself earns: `RUN_BUSY` is the
        // registered refusal for losing a run's startup/mutation serialization,
        // and it is retryable because the reset will resolve.
        assert_eq!(fenced.code, "RUN_BUSY");
        assert!(fenced.retryable);
        assert_eq!(fenced.details["run_id"], serde_json::json!(run_id));
        assert_eq!(fenced.details["owner_kind"], "startup");
        // The reset that owns the token is not fenced by it.
        assert_eq!(
            reconcile_binding_with_journal(
                &run_root,
                run_id,
                binding.clone(),
                BindingPurpose::Reset
            )
            .unwrap(),
            binding
        );

        // Resolving it terminally lifts the fence.
        journal.fail(operation_id).unwrap();
        assert_eq!(
            reconcile_binding_with_journal(
                &run_root,
                run_id,
                binding.clone(),
                BindingPurpose::Mutation
            )
            .unwrap(),
            binding
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reconcile_binding_with_journal_allows_consistent_state() {
        let root = private_directory();
        let (old_binding, _replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let run_root = root.join("run");
        fs::create_dir(&run_root).unwrap();
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700)).unwrap();

        // No reset journal yet: nothing to reconcile against.
        assert_eq!(
            reconcile_binding_with_journal(
                &run_root,
                run_id,
                old_binding.clone(),
                BindingPurpose::Mutation
            )
            .unwrap(),
            old_binding
        );

        // A durably committed reset is consistent even though the binding
        // generation advanced, because the commit token exists.
        fs::create_dir(run_root.join("recovery")).unwrap();
        fs::set_permissions(run_root.join("recovery"), fs::Permissions::from_mode(0o700)).unwrap();
        let mut journal = DurableResetJournal::new(run_id, &run_root, old_binding.clone());
        let state = resettable_state(run_id, old_binding.clone());
        let operation_id = new_uuid_v7();
        journal.prepare(operation_id, &state).unwrap();
        let advanced_binding = ControllerBinding {
            identity: ControllerIdentity {
                generation: 2,
                ..old_binding.identity.clone()
            },
            ..old_binding.clone()
        };
        let record = ControllerResetRecord {
            operation_id,
            run_id,
            old_controller_id: old_binding.identity.controller_id,
            new_controller_id: old_binding.identity.controller_id,
            new_generation: 2,
            writer_released: false,
        };
        journal.commit(&record, &advanced_binding).unwrap();
        assert_eq!(
            reconcile_binding_with_journal(
                &run_root,
                run_id,
                advanced_binding.clone(),
                BindingPurpose::Mutation
            )
            .unwrap(),
            advanced_binding
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn credential_invalid_details_satisfy_the_error_contract_schema() {
        let error = match CredentialCarrier::open_path(Path::new(
            "/nonexistent/dolgorae-credential-test.json",
        )) {
            Err(error) => error,
            Ok(_) => panic!("expected opening a nonexistent credential file to fail"),
        };
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert!(!error.retryable);
        let details = error.details.as_object().unwrap();
        assert_eq!(details.len(), 2);
        assert!(
            details["argument"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            details["reason"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[test]
    fn controller_mismatch_details_satisfy_the_error_contract_schema() {
        let root = private_directory();
        let path = root.join("controller.json");
        let created = create_controller_credential(
            &path,
            ControllerKind::Automation,
            "job".to_owned(),
            None,
            None,
        )
        .unwrap();
        let carrier = CredentialCarrier::open_path(&path).unwrap();
        let mut binding = binding_from_credential(&created.credential, 1, &carrier).unwrap();
        binding.capability_sha256 = "0".repeat(64);
        let run_id = new_uuid_v7();
        let error =
            authorize_controller(run_id, "run.controller.verify", &binding, &carrier).unwrap_err();
        assert_eq!(error.code, "CONTROLLER_MISMATCH");
        assert!(!error.retryable);
        let details = error.details.as_object().unwrap();
        assert_eq!(details.len(), 2);
        assert_eq!(details["run_id"], run_id.to_string());
        assert_eq!(details["operation"], "run.controller.verify");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operator_mismatch_details_satisfy_the_error_contract_schema() {
        let root = private_directory();
        let store = OperatorStore::new(root.join("operator"));
        let first_path = root.join("operator-1.json");
        store.initialize(&first_path).unwrap();
        let stale_carrier = CredentialCarrier::open_path(&first_path).unwrap();
        let second_path = root.join("operator-2.json");
        store.rotate(&stale_carrier, &second_path).unwrap();

        let error = store.authorize(&stale_carrier).unwrap_err();
        assert_eq!(error.code, "OPERATOR_MISMATCH");
        assert!(!error.retryable);
        let details = error.details.as_object().unwrap();
        assert_eq!(details.len(), 1);
        assert!(
            details["operation"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reset_not_allowed_details_satisfy_the_error_contract_schema() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let mut state = ControllerResetState {
            lifecycle: RunLifecycle::Running,
            pending_interaction: true,
            handoff_active: true,
            generation_verifiable: false,
            ..resettable_state(run_id, binding)
        };
        let mut journal = MemoryJournal::default();
        let error = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &UnlockedRun,
            },
            &replacement,
            &mut journal,
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.code, "CONTROLLER_RESET_NOT_ALLOWED");
        assert!(!error.retryable);
        let details = error.details.as_object().unwrap();
        assert_eq!(details.len(), 3);
        assert_eq!(details["run_id"], run_id.to_string());
        assert_eq!(details["state"], "running");
        let blockers: Vec<String> = details["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            blockers,
            vec![
                "lifecycle_not_resettable",
                "pending_interaction",
                "handoff_active",
                "writer_generation_unverifiable",
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Records whether `operator.lock` was free at each reset journal
    /// boundary. Those two points are exactly where SPEC-013 pins the hold:
    /// PREPARE runs under the authorizing hold, and COMMIT runs under a
    /// freshly reacquired one.
    struct LockObservingJournal<'a> {
        store: &'a OperatorStore,
        prepare_saw_free_lock: Option<bool>,
        commit_saw_free_lock: Option<bool>,
        committed: bool,
    }

    impl LockObservingJournal<'_> {
        fn new(store: &OperatorStore) -> LockObservingJournal<'_> {
            LockObservingJournal {
                store,
                prepare_saw_free_lock: None,
                commit_saw_free_lock: None,
                committed: false,
            }
        }

        fn lock_is_free(&self) -> bool {
            self.store.try_lock_exclusive().unwrap().is_some()
        }
    }

    impl ResetJournal for LockObservingJournal<'_> {
        fn prepare(&mut self, _: Uuid, _: &ControllerResetState) -> Result<(), MachineError> {
            self.prepare_saw_free_lock = Some(self.lock_is_free());
            Ok(())
        }

        fn commit(
            &mut self,
            _: &ControllerResetRecord,
            _: &ControllerBinding,
        ) -> Result<(), MachineError> {
            self.commit_saw_free_lock = Some(self.lock_is_free());
            self.committed = true;
            Ok(())
        }

        fn fail(&mut self, _: Uuid) -> Result<(), MachineError> {
            Ok(())
        }
    }

    #[test]
    fn rotation_waits_for_an_authorized_hold_and_then_revokes_the_old_generation() {
        let root = private_directory();
        let store = OperatorStore::new(root.join("operator"));
        let first_path = root.join("operator-1.json");
        store.initialize(&first_path).unwrap();
        let first_carrier = CredentialCarrier::open_path(&first_path).unwrap();
        let authorization = store.authorize(&first_carrier).unwrap();
        assert_eq!(authorization.operator_generation, 1);

        let (started, running) = std::sync::mpsc::channel();
        let rotation_root = root.clone();
        let rotator = std::thread::spawn(move || {
            let store = OperatorStore::new(rotation_root.join("operator"));
            let carrier =
                CredentialCarrier::open_path(&rotation_root.join("operator-1.json")).unwrap();
            started.send(()).unwrap();
            store.rotate(&carrier, &rotation_root.join("operator-2.json"))
        });
        running.recv().unwrap();

        // Neither assertion is a timing guess. Rotation cannot take the lock
        // this thread still holds, and it cannot publish a generation without
        // taking it, so both hold however far the rotator has run.
        assert!(store.try_lock_exclusive().unwrap().is_none());
        assert_eq!(store.load_state().unwrap().generation, 1);

        authorization.release();
        let rotated = rotator.join().unwrap().unwrap();
        assert_eq!(rotated.credential.operator_generation, 2);
        assert_eq!(
            store.authorize(&first_carrier).unwrap_err().code,
            "OPERATOR_MISMATCH"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotating_under_a_hold_the_caller_already_owns_never_relocks() {
        let root = private_directory();
        let store = OperatorStore::new(root.join("operator"));
        let first_path = root.join("operator-1.json");
        store.initialize(&first_path).unwrap();
        let first_carrier = CredentialCarrier::open_path(&first_path).unwrap();
        let authorization = store.authorize(&first_carrier).unwrap();

        // `store.rotate` here would wait forever on the hold this very thread
        // owns, because `flock` is per open file description; reusing the
        // hold is the only way through, and reaching the assertion below is
        // the proof that it does not deadlock.
        let second_path = root.join("operator-2.json");
        let rotated = store.rotate_holding(&authorization, &second_path).unwrap();
        assert_eq!(rotated.credential.operator_id, authorization.operator_id);
        assert_eq!(rotated.credential.operator_generation, 2);
        authorization.release();

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

    #[test]
    fn releasing_an_operator_lock_unlocks_a_fork_style_duplicate() {
        let root = private_directory();
        let store = OperatorStore::new(root.join("operator"));
        let lock = store.lock_exclusive().unwrap();
        let inherited = lock.file.try_clone().unwrap();

        assert!(store.try_lock_exclusive().unwrap().is_none());
        lock.release();
        assert!(store.try_lock_exclusive().unwrap().is_some());

        drop(inherited);
        fs::remove_dir_all(root).unwrap();
    }

    /// A Run mutation lock that records the exact order it is taken in, and
    /// whether `operator.lock` was already held each time.
    struct OrderedRunLock<'a> {
        store: &'a OperatorStore,
        events: std::cell::RefCell<Vec<&'static str>>,
        operator_held_on_acquire: std::cell::RefCell<Vec<bool>>,
        refuse: bool,
    }

    impl OrderedRunLock<'_> {
        fn new(store: &OperatorStore, refuse: bool) -> OrderedRunLock<'_> {
            OrderedRunLock {
                store,
                events: std::cell::RefCell::new(Vec::new()),
                operator_held_on_acquire: std::cell::RefCell::new(Vec::new()),
                refuse,
            }
        }

        fn held(&self) -> bool {
            // `flock` is per open file description, so a free probe here
            // proves no other descriptor holds it.
            self.store.try_lock_exclusive().unwrap().is_none()
        }

        fn note(&self, event: &'static str) {
            self.events.borrow_mut().push(event);
        }

        fn events(&self) -> Vec<&'static str> {
            self.events.borrow().clone()
        }
    }

    impl RunMutationLock for OrderedRunLock<'_> {
        fn acquire(&self) -> Result<(), MachineError> {
            self.operator_held_on_acquire.borrow_mut().push(self.held());
            if self.refuse {
                self.note("refused");
                return Err(MachineError::new(
                    "RUN_BUSY",
                    "another operation holds this run's startup lock",
                    true,
                    serde_json::json!({"run_id": new_uuid_v7(), "owner_kind": "startup"}),
                ));
            }
            self.note("acquire");
            Ok(())
        }

        fn release(&self) {
            self.note("release");
        }
    }

    #[test]
    fn the_reset_takes_the_run_lock_after_the_operator_lock_and_frees_both_for_apply() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let mut state = resettable_state(run_id, binding);
        let mut journal = MemoryJournal::default();
        let run_lock = OrderedRunLock::new(&operator_store, false);
        let observed_under_lock = std::cell::Cell::new(false);
        let apply_saw_free_operator_lock = std::cell::Cell::new(false);

        let record = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &run_lock,
            },
            &replacement,
            &mut journal,
            |_| {
                // The observation that decides resettability happens inside
                // the prefix, not before it.
                observed_under_lock.set(run_lock.events() == vec!["acquire"] && run_lock.held());
                run_lock.note("observe");
                Ok(())
            },
            || {
                run_lock.note("apply");
                apply_saw_free_operator_lock
                    .set(operator_store.try_lock_exclusive().unwrap().is_some());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(record.run_id, run_id);
        assert!(
            observed_under_lock.get(),
            "the projection must be read under both locks"
        );
        assert_eq!(
            run_lock.events(),
            vec![
                "acquire", "observe", "release", "apply", "acquire", "release"
            ],
            "PREPARE takes the run lock and frees it for APPLY; COMMIT retakes it"
        );
        assert_eq!(
            *run_lock.operator_held_on_acquire.borrow(),
            vec![true, true],
            "the run lock is only ever taken with operator.lock already held"
        );
        assert!(
            apply_saw_free_operator_lock.get(),
            "APPLY holds no coordination lock at all"
        );
        assert!(journal.committed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_run_lock_it_cannot_take_refuses_the_reset_before_anything_is_prepared() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let mut state = resettable_state(run_id, binding.clone());
        let mut journal = MemoryJournal::default();
        let run_lock = OrderedRunLock::new(&operator_store, true);

        let error = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &run_lock,
            },
            &replacement,
            &mut journal,
            |_| panic!("nothing is observed without the run lock"),
            || panic!("nothing is applied without the run lock"),
        )
        .unwrap_err();

        assert_eq!(error.code, "RUN_BUSY");
        assert!(error.retryable);
        assert!(!journal.prepared, "no token may be written");
        assert_eq!(
            state.binding, binding,
            "the old controller stays authoritative"
        );
        assert!(
            operator_store.try_lock_exclusive().unwrap().is_some(),
            "a refused run lock must not strand operator.lock"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_observed_under_the_lock_decides_resettability_not_the_state_passed_in() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        // Idle going in — a worker accepted a Turn before the lock was taken.
        let mut state = resettable_state(run_id, binding.clone());
        let mut journal = MemoryJournal::default();
        let run_lock = OrderedRunLock::new(&operator_store, false);

        let error = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &run_lock,
            },
            &replacement,
            &mut journal,
            |state| {
                state.lifecycle = RunLifecycle::Running;
                Ok(())
            },
            || panic!("a Run with a live Turn never reaches APPLY"),
        )
        .unwrap_err();

        assert_eq!(error.code, "CONTROLLER_RESET_NOT_ALLOWED");
        assert_eq!(
            error.details["blockers"],
            serde_json::json!(["lifecycle_not_resettable"])
        );
        assert!(!journal.prepared);
        assert_eq!(state.binding, binding);
        assert_eq!(
            run_lock.events(),
            vec!["acquire", "release"],
            "a refusal inside the prefix still frees the run lock"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_apply_that_finds_live_work_rolls_the_prepare_back_and_installs_nothing() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let mut state = resettable_state(run_id, binding.clone());
        let mut journal = MemoryJournal::default();
        let run_lock = OrderedRunLock::new(&operator_store, false);

        let error = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &run_lock,
            },
            &replacement,
            &mut journal,
            |_| Ok(()),
            || {
                // What a live worker's drain answers when it accepted a Turn
                // before the prepare landed.
                Err(reset_not_allowed(
                    run_id,
                    RunLifecycle::ReconciliationRequired,
                    vec!["active_turn".to_owned()],
                ))
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "CONTROLLER_RESET_NOT_ALLOWED");
        assert_eq!(
            error.details["blockers"],
            serde_json::json!(["active_turn"])
        );
        assert!(journal.prepared, "the fence was written before APPLY");
        assert!(!journal.committed, "nothing is installed over a live Turn");
        assert!(journal.failed, "the prepare is resolved terminally");
        assert_eq!(state.binding, binding);
        assert_eq!(
            run_lock.events(),
            vec!["acquire", "release"],
            "APPLY refused, so the run lock is never retaken for COMMIT"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn controller_reset_holds_the_operator_lock_for_prepare_and_reacquires_it_for_commit() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let mut state = resettable_state(run_id, binding);
        let mut journal = LockObservingJournal::new(&operator_store);
        let apply_saw_free_lock = std::sync::atomic::AtomicBool::new(false);

        let record = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &UnlockedRun,
            },
            &replacement,
            &mut journal,
            |_| Ok(()),
            || {
                // APPLY interrupts and inspects live work, so it must hold no
                // coordination lock at all.
                apply_saw_free_lock.store(
                    operator_store.try_lock_exclusive().unwrap().is_some(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(record.run_id, run_id);
        assert_eq!(journal.prepare_saw_free_lock, Some(false));
        assert!(apply_saw_free_lock.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(journal.commit_saw_free_lock, Some(false));
        assert!(journal.committed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn controller_reset_refuses_to_commit_after_a_rotation_during_apply() {
        let root = private_directory();
        let (operator_store, operator) = matched_operator(&root);
        let (binding, replacement) = matched_replacement_pair(&root);
        let run_id = new_uuid_v7();
        let mut state = resettable_state(run_id, binding.clone());
        let mut journal = MemoryJournal::default();

        let error = reset_controller(
            &mut state,
            run_id,
            &ResetAuthority {
                operator_store: &operator_store,
                operator: &operator,
                run_lock: &UnlockedRun,
            },
            &replacement,
            &mut journal,
            |_| Ok(()),
            || {
                // The APPLY window is the one place the hold is deliberately
                // released, so a rotation lands here and must revoke the
                // generation that prepared the reset.
                operator_store
                    .rotate(&operator, &root.join("operator-2.json"))
                    .unwrap();
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "OPERATOR_MISMATCH");
        assert!(journal.prepared);
        assert!(!journal.committed);
        assert!(journal.failed);
        assert_eq!(state.binding, binding);
        assert_eq!(state.state_revision, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
