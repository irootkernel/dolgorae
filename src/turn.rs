//! Thread and Turn lifecycle owned by one Run worker connection.

use crate::app_server::{JsonRpcConnection, SolicitedSink, TransportError, Wire};
use crate::audit::AuditKind;
use crate::domain::RunLifecycle;
use crate::fault::{FaultInjector, NoFaults};
use crate::interaction_payload::{
    ChangeArtifactPayload, CommandApprovalPayload, FileApprovalPayload, FileChangePayload,
};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{Ledger, LedgerClock, SystemLedgerClock};
use crate::machine::MachineError;
use crate::primary_tool::{PrimaryCallContext, PrimaryToolContract};
use crate::workspace::LosslessPath;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

pub const MAX_INLINE_FINAL_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_FINAL_RESPONSE_ARTIFACT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_INTERACTION_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TURN_ITEMS: usize = 16_384;
pub const MAX_PENDING_PRIMARY_TOOLS: usize = 256;
/// Retention bound for one streamed Turn read back from `thread/read`.  This is
/// a product policy about how large a single Turn's items may be, not a
/// transport bound on the reply: the reply itself streams unbounded and every
/// Turn that is not the one being completed is discarded as it arrives.
pub const MAX_SOLICITED_TURN_BYTES: usize =
    MAX_FINAL_RESPONSE_ARTIFACT_BYTES + MAX_TURN_ITEMS * 1024;
/// How many foreign-thread server requests are recorded before correlation is
/// treated as broken rather than merely noisy.
pub const MAX_IGNORED_FOREIGN_REQUESTS: usize = 64;
/// How much of one foreign-thread routing field is retained.
///
/// The values come from a shared app-server this Run does not control, so each
/// is truncated on a character boundary before it reaches the durable
/// diagnostic journal; the bound plus `MAX_IGNORED_FOREIGN_REQUESTS` is what
/// keeps a foreign Thread from driving the journal's size.
pub const MAX_FOREIGN_FIELD_CHARS: usize = 128;
#[derive(Clone, Debug)]
pub struct PrimaryToolCall {
    pub request_id: u64,
    pub run_generation: u64,
    pub context: PrimaryCallContext,
    pub payload: Value,
}

#[derive(Clone, Debug)]
pub struct PrimaryToolCompletion {
    pub request_id: u64,
    pub run_generation: u64,
    pub source_tool_call_id: String,
    pub result: Result<Value, MachineError>,
}

pub trait PrimaryToolDispatcher: Send + Sync {
    fn dispatch(&self, call: PrimaryToolCall) -> Result<(), MachineError>;
}

pub struct PrimaryToolConfig {
    pub session_id: Uuid,
    pub contract: PrimaryToolContract,
    pub dispatcher: Arc<dyn PrimaryToolDispatcher>,
}

#[derive(Clone, Debug)]
struct PendingPrimaryToolCall {
    payload_sha256: String,
    request_ids: Vec<u64>,
}

/// Interaction policy fixed for the lifetime of one Run worker session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSafetyPolicy {
    #[default]
    Standard,
    ReviewerReadOnly,
}

/// Where an app-server transport failure happened.
///
/// The checked error contract requires every `TRANSPORT_FAILURE` to name the
/// stage it failed at, so the failure carries the stage rather than leaving a
/// caller to infer it from prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportStage {
    Connect,
    Write,
    Read,
    Decode,
    Correlate,
    Shutdown,
}

impl TransportStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Write => "write",
            Self::Read => "read",
            Self::Decode => "decode",
            Self::Correlate => "correlate",
            Self::Shutdown => "shutdown",
        }
    }
}

/// The Run facts a Turn failure names.
///
/// The checked error contract gives every code a required `details` schema,
/// and most of those members describe the Run rather than the fault. Carrying
/// them here keeps `TurnError` a statement about what went wrong while the
/// envelope still names which Run it went wrong in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnFailureContext {
    pub run_id: Uuid,
    pub profile: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnError {
    InvalidInput(&'static str),
    ModelMismatch {
        expected: String,
        actual: String,
    },
    EffortUnsupported {
        requested: String,
        supported: Vec<String>,
    },
    TurnBusy,
    IdempotencyConflict {
        recorded: String,
        observed: String,
    },
    Transport {
        stage: TransportStage,
        reason: String,
    },
    /// The app-server refused this request on the reply it was correlated to.
    ///
    /// This is definitive, not uncertain: the request reached the app-server,
    /// the app-server decided it, and it answered the very request this
    /// operation was waiting for.  Nothing about the Run's state is in doubt,
    /// so it is never restated as `outcome_unknown` and never quarantines the
    /// Run the way a lost answer does.
    AppServerRejected {
        code: i64,
        message: String,
    },
    CorrelationMismatch,
    DuplicateTerminal,
    OutcomeUnknown,
    InteractionNotFound {
        request_id: u64,
    },
    InteractionResponseInvalid {
        request_id: u64,
    },
    InteractionAlreadyResolved {
        request_id: u64,
    },
    InteractionStale {
        request_id: u64,
    },
    Journal(String),
    Artifact(String),
}

impl TurnError {
    /// A transport failure that names the stage it happened at.
    #[must_use]
    pub fn transport(stage: TransportStage, reason: impl Into<String>) -> Self {
        Self::Transport {
            stage,
            reason: reason.into(),
        }
    }

    /// The app-server's own JSON-RPC error object, as this Run reports it.
    ///
    /// The app-server's prose is bounded here, exactly as a foreign
    /// observation's is: it is another process's text and must not decide how
    /// large this Run's diagnostics grow.
    #[must_use]
    pub fn rejected(error: &Value) -> Self {
        Self::AppServerRejected {
            code: error
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MIN),
            message: bounded_diagnostic_text(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unstructured app-server error"),
            ),
        }
    }
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(formatter, "invalid turn input: {reason}"),
            Self::ModelMismatch { expected, actual } => write!(
                formatter,
                "turn model {actual} differs from fixed model {expected}"
            ),
            Self::EffortUnsupported { requested, .. } => {
                write!(formatter, "reasoning effort {requested} is not advertised")
            }
            Self::TurnBusy => formatter.write_str("run already has an active turn"),
            Self::IdempotencyConflict { .. } => {
                formatter.write_str("idempotency key was reused with different input")
            }
            Self::Transport { stage, reason } => write!(
                formatter,
                "app-server transport failed at {}: {reason}",
                stage.as_str()
            ),
            Self::AppServerRejected { code, message } => write!(
                formatter,
                "app-server refused the request with error {code}: {message}"
            ),
            Self::CorrelationMismatch => {
                formatter.write_str("thread or turn correlation failed closed")
            }
            Self::DuplicateTerminal => {
                formatter.write_str("duplicate terminal event failed closed")
            }
            Self::OutcomeUnknown => formatter.write_str("turn acceptance or outcome is unknown"),
            Self::InteractionNotFound { request_id } => {
                write!(formatter, "interaction {request_id} is not pending")
            }
            Self::InteractionResponseInvalid { request_id } => {
                write!(
                    formatter,
                    "interaction {request_id} response does not match its schema"
                )
            }
            Self::InteractionAlreadyResolved { request_id } => {
                write!(formatter, "interaction {request_id} is already resolved")
            }
            Self::InteractionStale { request_id } => {
                write!(formatter, "interaction {request_id} is stale")
            }
            Self::Journal(reason) => write!(formatter, "durable journal failed: {reason}"),
            Self::Artifact(reason) => write!(formatter, "response artifact failed: {reason}"),
        }
    }
}

impl std::error::Error for TurnError {}

impl TurnError {
    /// Restate this failure as the checked machine error for one Run.
    ///
    /// Every code's `details` members are required and closed, so the mapping
    /// fills them from the fault plus `context`; none of them is ever left
    /// empty. Retryability is a property of the fault and is unchanged by the
    /// Run it happened in.
    #[must_use]
    pub fn into_machine_error(self, context: &TurnFailureContext) -> MachineError {
        let message = self.to_string();
        let run_id = context.run_id.to_string();
        match self {
            Self::InvalidInput(_) => MachineError::invalid_argument("turn", message),
            Self::ModelMismatch { expected, actual } => MachineError::new(
                "COMPATIBILITY_REJECTED",
                message,
                false,
                serde_json::json!({
                    "profile": context.profile,
                    "check": "model",
                    "expected": expected,
                    "actual": actual,
                }),
            ),
            Self::EffortUnsupported {
                requested,
                supported,
            } => MachineError::new(
                "COMPATIBILITY_REJECTED",
                message,
                false,
                serde_json::json!({
                    "profile": context.profile,
                    "check": "reasoning_effort",
                    "expected": supported,
                    "actual": requested,
                }),
            ),
            Self::TurnBusy => MachineError::new(
                "RUN_BUSY",
                message,
                true,
                serde_json::json!({"run_id": run_id, "owner_kind": "turn"}),
            ),
            Self::IdempotencyConflict { recorded, observed } => MachineError::new(
                "IDEMPOTENCY_CONFLICT",
                message,
                false,
                serde_json::json!({
                    "run_id": run_id,
                    "recorded_input_digest": recorded,
                    "observed_input_digest": observed,
                }),
            ),
            // Retryable, so the contract admits only an acceptance a caller may
            // safely retry from.  A Turn that was accepted and then lost is
            // reported as `OUTCOME_UNKNOWN` by the coordinator instead, which is
            // where the uncertain-acceptance case lives.
            Self::Transport { stage, .. } => MachineError::new(
                "TRANSPORT_FAILURE",
                message,
                true,
                serde_json::json!({
                    "stage": stage.as_str(),
                    "acceptance": "not_accepted",
                    "request_id": Value::Null,
                }),
            ),
            // A refusal the app-server correlated to this very request is
            // definitive: the write reached it and was declined, so nothing
            // was accepted and the identical invocation stays safe to reissue.
            // That is the retryable `not_accepted` slot the contract reserves,
            // and it is not the uncertainty `OUTCOME_UNKNOWN` describes.
            Self::AppServerRejected { .. } => MachineError::new(
                "TRANSPORT_FAILURE",
                message,
                true,
                serde_json::json!({
                    "stage": TransportStage::Read.as_str(),
                    "acceptance": "not_accepted",
                    "request_id": Value::Null,
                }),
            ),
            Self::CorrelationMismatch | Self::DuplicateTerminal | Self::OutcomeUnknown => {
                MachineError::new(
                    "OUTCOME_UNKNOWN",
                    message,
                    false,
                    serde_json::json!({
                        "run_id": run_id,
                        "thread_id": context.thread_id,
                        "turn_id": context.turn_id,
                    }),
                )
            }
            Self::InteractionNotFound { request_id } => {
                MachineError::interaction_not_found(context.run_id, request_id, message)
            }
            Self::InteractionResponseInvalid { request_id } => MachineError::new(
                "INTERACTION_RESPONSE_INVALID",
                message,
                false,
                serde_json::json!({"run_id": run_id, "request_id": request_id.to_string()}),
            ),
            Self::InteractionAlreadyResolved { request_id } => MachineError::new(
                "INTERACTION_ALREADY_RESOLVED",
                message,
                false,
                serde_json::json!({"run_id": run_id, "request_id": request_id.to_string()}),
            ),
            Self::InteractionStale { request_id } => MachineError::new(
                "INTERACTION_STALE",
                message,
                false,
                serde_json::json!({"run_id": run_id, "request_id": request_id.to_string()}),
            ),
            Self::Journal(reason) => MachineError::new(
                "INTERNAL_ERROR",
                message,
                false,
                serde_json::json!({"invariant": reason}),
            ),
            // A response artifact that could not be written is a durable-write
            // invariant failure, not the digest mismatch
            // `ARTIFACT_INTEGRITY_FAILURE` describes; claiming that code would
            // require an artifact identity and digests that do not exist.
            Self::Artifact(reason) => MachineError::new(
                "INTERNAL_ERROR",
                message,
                false,
                serde_json::json!({"invariant": reason}),
            ),
        }
    }
}

impl From<TransportError> for TurnError {
    fn from(value: TransportError) -> Self {
        let stage = match value {
            TransportError::UpgradeRejected => TransportStage::Connect,
            TransportError::InvalidFrame(_)
            | TransportError::MessageTooLarge
            | TransportError::InvalidUtf8
            | TransportError::InvalidJson => TransportStage::Decode,
            TransportError::CorrelationMismatch => TransportStage::Correlate,
            // A JSON-RPC error the app-server correlated to a request is a
            // decision, not a transport fault: the reply arrived, decoded, and
            // answered the request it was issued for.  Restating it as a lost
            // answer is what would quarantine a Run over an app-server saying
            // "no", so every transport this Run speaks reports it as the
            // refusal it is.
            TransportError::RemoteError { code, message } => {
                return Self::AppServerRejected {
                    code,
                    message: bounded_diagnostic_text(&message),
                };
            }
            TransportError::Io(_) | TransportError::Closed => TransportStage::Read,
        };
        Self::Transport {
            stage,
            reason: value.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Send,
    Submit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
}

impl ImageDetail {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSnapshot {
    pub detail: ImageDetail,
    pub canonical_path: LosslessPath,
    pub byte_length: u64,
    pub sha256: String,
}

impl ImageSnapshot {
    pub fn capture(path: &Path, detail: ImageDetail) -> Result<Self, TurnError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| TurnError::InvalidInput("image path is unreadable"))?;
        let mut file =
            File::open(&canonical).map_err(|_| TurnError::InvalidInput("image is unreadable"))?;
        let before = file
            .metadata()
            .map_err(|_| TurnError::InvalidInput("image metadata is unavailable"))?;
        if !before.file_type().is_file() {
            return Err(TurnError::InvalidInput("image must be a regular file"));
        }
        let mut hasher = Sha256::new();
        let mut length = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| TurnError::InvalidInput("image read failed"))?;
            if read == 0 {
                break;
            }
            length = length
                .checked_add(u64::try_from(read).expect("buffer length is bounded"))
                .ok_or(TurnError::InvalidInput("image is too large"))?;
            hasher.update(&buffer[..read]);
        }
        let after = file
            .metadata()
            .map_err(|_| TurnError::InvalidInput("image metadata is unavailable"))?;
        if length != before.len() || !same_file_snapshot(&before, &after) {
            return Err(TurnError::InvalidInput("image changed while hashing"));
        }
        Ok(Self {
            detail,
            canonical_path: LosslessPath::from_path(&canonical),
            byte_length: length,
            sha256: format!("{:x}", hasher.finalize()),
        })
    }

    pub fn verify(&self) -> Result<PathBuf, TurnError> {
        let path = self
            .canonical_path
            .to_path_buf()
            .map_err(|_| TurnError::InvalidInput("image path encoding is invalid"))?;
        let current = Self::capture(&path, self.detail)?;
        if current != *self {
            return Err(TurnError::OutcomeUnknown);
        }
        Ok(path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnRequest {
    pub write: bool,
    pub idempotency_key: String,
    pub message: String,
    pub images: Vec<ImageSnapshot>,
    pub model: String,
    pub effort: Option<String>,
    pub delivery: DeliveryMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationIntent {
    pub operation_id: Uuid,
    pub idempotency_key: String,
    pub request_sha256: String,
    pub run_generation: u64,
    pub server_key: String,
    pub server_epoch: u64,
    pub provisional_thread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_input: Option<AcceptedUserInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedInputStorage {
    pub artifact_id: Uuid,
    pub created_at: String,
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptedInputContent {
    Inline { storage: AcceptedInputStorage },
    Artifact { storage: AcceptedInputStorage },
}

impl AcceptedInputContent {
    #[must_use]
    pub const fn storage(&self) -> &AcceptedInputStorage {
        match self {
            Self::Inline { storage } | Self::Artifact { storage } => storage,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedImageMetadata {
    pub ordinal: u32,
    pub detail: ImageDetail,
    pub media_type: String,
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedUserInput {
    pub content: AcceptedInputContent,
    pub images: Vec<AcceptedImageMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JournalEntry {
    Intent(OperationIntent),
    ProvisionalThread {
        operation_id: Uuid,
        thread_id: String,
    },
    ThreadBound {
        operation_id: Uuid,
        thread_id: String,
        thread_generation: u64,
        effective_policy: crate::domain::EffectivePolicy,
    },
    TurnAccepted {
        operation_id: Uuid,
        thread_id: String,
        turn_id: String,
        effort: String,
        effective_policy: crate::domain::EffectivePolicy,
        controller: Option<crate::domain::ControllerIdentity>,
    },
    InteractionOpened {
        request_id: u64,
        method: String,
    },
    ApprovalRequested {
        interaction: Value,
    },
    InteractionResolved {
        request_id: u64,
    },
    ApprovalDecided {
        request_id: u64,
        idempotency_key: String,
        response_sha256: Option<String>,
        resolution_receipt_id: Option<Uuid>,
        resolved_at: String,
        resolution: Value,
    },
    FileChangeSnapshot {
        thread_id: String,
        turn_id: String,
        item_id: String,
        revision: u64,
        snapshot_sha256: String,
    },
    WriterAccessChanged {
        write: bool,
        writer_generation: u64,
        transaction_id: Uuid,
        effective_policy: crate::domain::EffectivePolicy,
    },
    Reconciliation {
        thread_id: String,
        turn_id: String,
        observed_status: Option<String>,
        server_key: String,
        server_epoch: u64,
    },
    CleanupResult {
        outcome: String,
    },
    LifecycleTransition {
        previous: RunLifecycle,
        current: RunLifecycle,
        terminal_seal: bool,
        interrupt_terminal_confirmed: bool,
    },
    /// One Turn reached a terminal state.
    ///
    /// docs/specs/README.md sends a Master to `run status.data.last_terminal` for the
    /// response, usage, and cursor behind an intentionally minimal exit-7
    /// envelope, and a worker's control state is process memory that a
    /// restart or a shutdown erases.  The durable record therefore carries the
    /// whole terminal Turn rather than its identity alone, so the ledger — not
    /// a live worker — is what `last_terminal` is reconstructed from.
    Terminal {
        turn_id: String,
        status: String,
        thread_id: String,
        effort: String,
        usage: Option<Value>,
        final_response: Option<FinalResponse>,
    },
    OutcomeUnknown {
        operation_id: Uuid,
    },
}

pub trait DurableTurnJournal {
    fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError>;
}

#[derive(Default)]
pub struct MemoryJournal {
    pub entries: Vec<JournalEntry>,
}
impl DurableTurnJournal for MemoryJournal {
    fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError> {
        self.entries.push(entry);
        Ok(())
    }
}

pub struct LedgerTurnJournal<'a, C: LedgerClock, F: FaultInjector> {
    ledger: &'a mut Ledger<C, F>,
    run_generation: u64,
}

impl<'a, C: LedgerClock, F: FaultInjector> LedgerTurnJournal<'a, C, F> {
    #[must_use]
    pub const fn new(ledger: &'a mut Ledger<C, F>, run_generation: u64) -> Self {
        Self {
            ledger,
            run_generation,
        }
    }
}

impl<C: LedgerClock + 'static, F: FaultInjector + 'static> DurableTurnJournal
    for LedgerTurnJournal<'_, C, F>
{
    fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError> {
        let (kind, payload) = journal_record(entry)?;
        append_journal_payload(self.ledger, kind, &payload, self.run_generation)
    }
}

/// One durable Ledger shared between the Turn writer and same-uid observers.
///
/// The lock is taken only for the length of one append, so an observer reading
/// the audit ledger is never blocked behind a Turn that is still running.
pub struct SharedLedgerJournal<C: LedgerClock = SystemLedgerClock, F: FaultInjector = NoFaults> {
    ledger: Arc<Mutex<Ledger<C, F>>>,
    run_generation: u64,
    event_authority: Option<JournalEventAuthority>,
}

struct JournalEventAuthority {
    writer: crate::writer::WriterStore,
    workspace_id: String,
    server_key: String,
    server_epoch: u64,
}

impl<C: LedgerClock, F: FaultInjector> SharedLedgerJournal<C, F> {
    #[must_use]
    pub const fn new(ledger: Arc<Mutex<Ledger<C, F>>>, run_generation: u64) -> Self {
        Self {
            ledger,
            run_generation,
            event_authority: None,
        }
    }

    #[must_use]
    pub fn with_event_authority(
        mut self,
        state_root: &Path,
        workspace_id: &str,
        uid: u32,
        server_key: &str,
        server_epoch: u64,
    ) -> Self {
        self.event_authority = Some(JournalEventAuthority {
            writer: crate::writer::WriterStore::new(state_root, workspace_id, uid),
            workspace_id: workspace_id.to_owned(),
            server_key: server_key.to_owned(),
            server_epoch,
        });
        self
    }

    #[must_use]
    pub fn ledger(&self) -> &Arc<Mutex<Ledger<C, F>>> {
        &self.ledger
    }
}

impl<C: LedgerClock + 'static, F: FaultInjector + 'static> DurableTurnJournal
    for SharedLedgerJournal<C, F>
{
    fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError> {
        let (kind, mut payload) = journal_record(entry)?;
        if let Some(authority) = &self.event_authority {
            return authority
                .writer
                .observe_locked(|writer| {
                    if kind == AuditKind::TurnStarted {
                        payload["acceptance_writer"] = serde_json::to_value(writer)
                            .map_err(|error| TurnError::Journal(error.to_string()))?;
                    }
                    let mut ledger = self.ledger.lock().map_err(|_| {
                        TurnError::Journal("shared ledger lock is poisoned".to_owned())
                    })?;
                    ledger.with_event_context(
                        crate::ledger::EventAppendContext {
                            workspace_id: authority.workspace_id.clone(),
                            server_key: authority.server_key.clone(),
                            server_epoch: authority.server_epoch,
                            writer_state_revision: writer.authority_revision,
                        },
                        |ledger| {
                            append_journal_payload(ledger, kind, &payload, self.run_generation)
                        },
                    )
                })
                .map_err(|_| {
                    TurnError::Journal("Writer authority observation failed".to_owned())
                })?;
        }
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| TurnError::Journal("shared ledger lock is poisoned".to_owned()))?;
        append_journal_payload(&mut ledger, kind, &payload, self.run_generation)
    }
}

fn append_journal_payload<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &mut Ledger<C, F>,
    kind: AuditKind,
    payload: &Value,
    run_generation: u64,
) -> Result<(), TurnError> {
    if kind == AuditKind::AppServerNotification {
        let raw =
            serde_json::to_vec(payload).map_err(|error| TurnError::Journal(error.to_string()))?;
        return ledger
            .append_app_server_message(
                kind,
                "item/fileChange/patchUpdated",
                &raw,
                run_generation,
                crate::ledger::AppendDurability::Required,
            )
            .map(|_| ())
            .map_err(|error| TurnError::Journal(error.to_string()));
    }
    if kind == AuditKind::TurnTerminal {
        ledger
            .append_final_response_event(payload, run_generation)
            .map_err(|error| TurnError::Journal(error.to_string()))?;
    }
    let outcome = if matches!(
        kind,
        AuditKind::CleanupResult | AuditKind::LifecycleTransition
    ) {
        ledger.append_conformance_payload(kind, payload, run_generation)
    } else {
        ledger.append_required_payload(kind, payload, run_generation)
    };
    outcome.map_err(|error| TurnError::Journal(error.to_string()))
}

/// Truncate one untrusted routing field on a character boundary.
fn bounded_diagnostic_text(value: &str) -> String {
    match value.char_indices().nth(MAX_FOREIGN_FIELD_CHARS) {
        Some((index, _)) => value[..index].to_owned(),
        None => value.to_owned(),
    }
}

fn journal_record(entry: JournalEntry) -> Result<(AuditKind, Value), TurnError> {
    let (kind, payload) = {
        match entry {
            JournalEntry::Intent(intent) => {
                (AuditKind::IdempotencyReserved, serde_json::to_value(intent))
            }
            JournalEntry::ProvisionalThread {
                operation_id,
                thread_id,
            } => (
                AuditKind::TurnIntent,
                Ok(
                    serde_json::json!({"operation_id":operation_id,"provisional_thread_id":thread_id}),
                ),
            ),
            JournalEntry::ThreadBound {
                operation_id,
                thread_id,
                thread_generation,
                effective_policy,
            } => (
                AuditKind::ThreadBound,
                Ok(
                    serde_json::json!({"operation_id":operation_id,"thread_id":thread_id,"thread_generation":thread_generation,"effective_policy":effective_policy}),
                ),
            ),
            JournalEntry::TurnAccepted {
                operation_id,
                thread_id,
                turn_id,
                effort,
                effective_policy,
                controller,
            } => (
                AuditKind::TurnStarted,
                Ok(
                    serde_json::json!({"operation_id":operation_id,"thread_id":thread_id,"turn_id":turn_id,"effort":effort,"effective_policy":effective_policy,"acceptance_controller":controller}),
                ),
            ),
            JournalEntry::InteractionOpened { request_id, method } => (
                AuditKind::InteractionOpened,
                Ok(serde_json::json!({"request_id":request_id.to_string(),"method":method})),
            ),
            JournalEntry::ApprovalRequested { interaction } => {
                (AuditKind::ApprovalRequested, Ok(interaction))
            }
            JournalEntry::InteractionResolved { request_id } => (
                AuditKind::InteractionResolved,
                Ok(serde_json::json!({"request_id":request_id.to_string()})),
            ),
            JournalEntry::ApprovalDecided {
                request_id,
                idempotency_key,
                response_sha256,
                resolution_receipt_id,
                resolved_at,
                resolution,
            } => (
                AuditKind::ApprovalDecided,
                Ok(serde_json::json!({
                    "request_id": request_id.to_string(),
                    "idempotency_key": idempotency_key,
                    "response_sha256": response_sha256,
                    "resolution_receipt_id": resolution_receipt_id,
                    "resolved_at": resolved_at,
                    "resolution": resolution,
                })),
            ),
            JournalEntry::FileChangeSnapshot {
                thread_id,
                turn_id,
                item_id,
                revision,
                snapshot_sha256,
            } => (
                AuditKind::AppServerNotification,
                Ok(serde_json::json!({
                    "method": "item/fileChange/patchUpdated",
                    "thread_id": thread_id,
                    "turn_id": turn_id,
                    "item_id": item_id,
                    "snapshot_revision": revision,
                    "snapshot_sha256": snapshot_sha256,
                })),
            ),
            JournalEntry::WriterAccessChanged {
                write,
                writer_generation,
                transaction_id,
                effective_policy,
            } => (
                if write {
                    AuditKind::WriterAcquired
                } else {
                    AuditKind::WriterReleased
                },
                Ok(serde_json::json!({
                    "writer_generation": writer_generation,
                    "transaction_id": transaction_id,
                    "effective_policy": effective_policy,
                })),
            ),
            JournalEntry::Reconciliation {
                thread_id,
                turn_id,
                observed_status,
                server_key,
                server_epoch,
            } => (
                AuditKind::Reconciliation,
                Ok(serde_json::json!({
                    "thread_id": thread_id,
                    "turn_id": turn_id,
                    "observed_status": observed_status,
                    "server_key": server_key,
                    "server_epoch": server_epoch,
                    "read_only": true,
                })),
            ),
            JournalEntry::CleanupResult { outcome } => (
                AuditKind::CleanupResult,
                Ok(serde_json::json!({"outcome": outcome})),
            ),
            JournalEntry::LifecycleTransition {
                previous,
                current,
                terminal_seal,
                interrupt_terminal_confirmed,
            } => {
                let mut payload = serde_json::json!({
                    "previous": previous.as_str(),
                    "current": current.as_str(),
                    "terminal_seal": terminal_seal,
                });
                if interrupt_terminal_confirmed {
                    payload["interrupt_terminal_confirmed"] = Value::Bool(true);
                }
                (AuditKind::LifecycleTransition, Ok(payload))
            }
            JournalEntry::Terminal {
                turn_id,
                status,
                thread_id,
                effort,
                usage,
                final_response,
            } => (
                AuditKind::TurnTerminal,
                terminal_payload(TerminalTurn {
                    thread_id,
                    turn_id,
                    status,
                    effort,
                    final_response,
                    usage,
                }),
            ),
            JournalEntry::OutcomeUnknown { operation_id } => (
                AuditKind::OutcomeUnknown,
                Ok(serde_json::json!({"operation_id":operation_id})),
            ),
        }
    };
    let payload = payload.map_err(|error| TurnError::Journal(error.to_string()))?;
    Ok((kind, payload))
}

/// How large a terminal record's payload may be before it is reduced to the
/// identity and outcome alone.
///
/// A Turn's answer can be up to `MAX_INLINE_FINAL_RESPONSE_BYTES` of text whose
/// JSON escaping multiplies it, and the app-server's usage object is its own,
/// not ours. The record proving the Turn ended must never be lost to the size
/// of what it describes, so half the payload bound is reserved for everything
/// else and an oversized terminal records what it can always represent.
const MAX_TERMINAL_PAYLOAD_BYTES: usize = crate::jcs::RAW_PAYLOAD_LIMIT / 2;

/// The `turn_terminal` payload, reduced to its bounded members when the whole
/// terminal Turn does not fit one record.
fn terminal_payload(terminal: TerminalTurn) -> Result<Value, serde_json::Error> {
    let whole = serde_json::to_value(&terminal)?;
    if serde_json::to_vec(&whole)?.len() <= MAX_TERMINAL_PAYLOAD_BYTES {
        return Ok(whole);
    }
    serde_json::to_value(TerminalTurn {
        final_response: None,
        usage: None,
        ..terminal
    })
}

pub trait AppServer {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, TurnError>;
    fn notify(&mut self, method: &str, params: Value) -> Result<(), TurnError>;
    fn next_message(&mut self) -> Result<Value, TurnError>;
    fn respond_result(&mut self, id: u64, result: Value) -> Result<(), TurnError>;
    fn respond_sensitive_result(&mut self, id: u64, result: &mut Value) -> Result<(), TurnError> {
        let outcome = self.respond_result(id, result.take());
        zeroize_protected_json(result);
        outcome
    }
    fn respond_error(&mut self, id: u64, code: i64, message: &str) -> Result<(), TurnError>;

    /// Issue a request whose reply this caller solicited and knows may exceed
    /// the unsolicited message bound.  The default implementation is the
    /// buffered one, which suits fakes whose replies are small; the real
    /// transport overrides it with the streaming path.
    fn request_streamed(
        &mut self,
        method: &str,
        params: Value,
        sink: &mut dyn SolicitedSink,
    ) -> Result<(), TurnError> {
        let result = self.request(method, params)?;
        let bytes = serde_json::to_vec(&result)
            .map_err(|_| TurnError::InvalidInput("solicited reply is unrepresentable"))?;
        sink.accept(&bytes)?;
        Ok(())
    }
}

impl<T: Wire> AppServer for JsonRpcConnection<T> {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, TurnError> {
        Ok(JsonRpcConnection::request(self, method, params)?)
    }
    fn notify(&mut self, method: &str, params: Value) -> Result<(), TurnError> {
        Ok(JsonRpcConnection::notify(self, method, params)?)
    }
    fn next_message(&mut self) -> Result<Value, TurnError> {
        Ok(JsonRpcConnection::next_message(self)?)
    }
    fn respond_result(&mut self, id: u64, result: Value) -> Result<(), TurnError> {
        Ok(JsonRpcConnection::respond_result(self, id, result)?)
    }
    fn respond_error(&mut self, id: u64, code: i64, message: &str) -> Result<(), TurnError> {
        Ok(JsonRpcConnection::respond_error(self, id, code, message)?)
    }
    fn request_streamed(
        &mut self,
        method: &str,
        params: Value,
        sink: &mut dyn SolicitedSink,
    ) -> Result<(), TurnError> {
        JsonRpcConnection::request_solicited(self, method, params, &mut Streamed(sink))?;
        Ok(())
    }
}

/// Adapts a trait object back into the generic sink the transport expects.
struct Streamed<'a>(&'a mut dyn SolicitedSink);

impl SolicitedSink for Streamed<'_> {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        self.0.accept(chunk)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadAttach {
    Start,
    Resume {
        thread_id: String,
    },
    Fork {
        source_thread_id: String,
        last_turn_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoordinatorState {
    Threadless,
    Idle,
    Paused,
    Running,
    WaitingInteraction,
    OutcomeUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedTurn {
    pub thread_id: String,
    pub turn_id: String,
    /// The reasoning effort this Turn was actually started with.
    ///
    /// SPEC-006 has a Turn report its own effort, not the Run's default: a
    /// one-turn `--effort` is a fact about this Turn and nothing else records
    /// it.
    pub effort: String,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryResult {
    Accepted(AcceptedTurn),
    WaitingInteraction {
        accepted: AcceptedTurn,
        requests: Vec<Interaction>,
    },
    Terminal(TerminalTurn),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interaction {
    pub request_id: u64,
    pub method: String,
    pub thread_id: String,
    pub turn_id: String,
    pub payload_sha256: String,
    pub byte_length: usize,
    #[serde(default, skip_serializing)]
    pub payload: Value,
    #[serde(default)]
    pub file_snapshot_revision: Option<u64>,
    #[serde(default)]
    pub file_snapshot_sha256: Option<String>,
}

#[derive(Clone, Debug)]
struct FileChangeSnapshot {
    revision: u64,
    changes: Vec<FileChangePayload>,
    snapshot_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FinalResponse {
    Inline {
        text: String,
    },
    Artifact {
        artifact_id: Uuid,
        byte_length: u64,
        sha256: String,
        created_at: String,
    },
    Unavailable {
        byte_length: u64,
        sha256: String,
        /// The published contract's own vocabulary: `too_large`,
        /// `quota_exceeded`, or `write_failed`.
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalTurn {
    pub thread_id: String,
    pub turn_id: String,
    pub status: String,
    /// The reasoning effort the Turn that just ended was started with.
    pub effort: String,
    pub final_response: Option<FinalResponse>,
    pub usage: Option<Value>,
}

/// A foreign-thread server request that this Run refused and ignored.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IgnoredForeignRequest {
    pub request_id: u64,
    pub method: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
}

/// The Run identity a foreign-thread observation is filed against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForeignLane<'a> {
    pub run_generation: u64,
    pub server_key: &'a str,
    pub server_epoch: u64,
}

/// Where a foreign-thread observation is durably recorded.
///
/// SPEC-004: "A foreign-thread object cannot mutate run state; it records
/// bounded profile diagnostic metadata."  docs/specs/README.md closes the v1 Run
/// audit-kind enum and states that a foreign-thread diagnostic "is never a Run
/// event and uses the separate profile diagnostic schema" — so the destination
/// is the Runtime Profile's journal, and the Run's coordinator is handed a
/// writer for it rather than deriving which profile it belongs to.
pub trait ForeignDiagnostics: Send {
    fn record(
        &self,
        ignored: &IgnoredForeignRequest,
        lane: ForeignLane<'_>,
    ) -> Result<(), TurnError>;
}

/// Streams one `thread/read(includeTurns:true)` reply, retaining only the Turn
/// being completed.
///
/// The reply may describe an entire Thread history, so it is never buffered.
/// Turn objects are recognised structurally — an object opened directly inside
/// an array member named `turns` — and each one is held only until its identity
/// is known.  Everything that is not the wanted Turn is dropped as it arrives.
#[derive(Debug)]
pub struct ThreadTurnSink {
    turn_id: String,
    stack: Vec<Frame>,
    in_string: bool,
    escaped: bool,
    last_significant: u8,
    reading_name: bool,
    name: Vec<u8>,
    pending_name: Option<Vec<u8>>,
    element: Option<Vec<u8>>,
    element_depth: usize,
    element_active: bool,
    reading_element_id: bool,
    element_id: Vec<u8>,
    items: Option<Vec<Value>>,
    seen: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Frame {
    array: bool,
    name: Option<Vec<u8>>,
}

impl ThreadTurnSink {
    #[must_use]
    pub fn new(turn_id: &str) -> Self {
        Self {
            turn_id: turn_id.to_owned(),
            stack: Vec::new(),
            in_string: false,
            escaped: false,
            last_significant: 0,
            reading_name: false,
            name: Vec::new(),
            pending_name: None,
            element: None,
            element_depth: 0,
            element_active: false,
            reading_element_id: false,
            element_id: Vec::new(),
            items: None,
            seen: 0,
        }
    }

    /// The wanted Turn's items, once the reply has been fully streamed.
    pub fn into_items(self) -> Result<Vec<Value>, TurnError> {
        if !self.stack.is_empty() || self.in_string {
            return Err(TurnError::CorrelationMismatch);
        }
        self.items.ok_or(TurnError::CorrelationMismatch)
    }

    #[must_use]
    pub const fn streamed_bytes(&self) -> u64 {
        self.seen
    }

    fn inside_turns_array(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|frame| frame.array && frame.name.as_deref() == Some(b"turns"))
    }

    /// The wanted Turn is the only one this reader ever ingests.  A Turn whose
    /// identity has already ruled it out stops being buffered the moment its
    /// `id` arrives, so a Thread history of any size costs one Turn of memory
    /// and one Turn of parsing.
    fn discard_unless_wanted(&mut self) {
        if self.element_id != self.turn_id.as_bytes() {
            self.element = None;
        }
    }

    fn finish_element(&mut self, closing: u8) -> Result<(), TurnError> {
        self.element_active = false;
        self.reading_element_id = false;
        let Some(mut bytes) = self.element.take() else {
            return Ok(());
        };
        bytes.push(closing);
        let text = std::str::from_utf8(&bytes).map_err(|_| TurnError::CorrelationMismatch)?;
        let parsed = parse(text).map_err(|_| TurnError::CorrelationMismatch)?;
        let canonical = canonicalize(&parsed).map_err(|_| TurnError::CorrelationMismatch)?;
        let turn: Value =
            serde_json::from_slice(&canonical).map_err(|_| TurnError::CorrelationMismatch)?;
        if turn.get("id").and_then(Value::as_str) != Some(self.turn_id.as_str()) {
            return Ok(());
        }
        if self.items.is_some() {
            return Err(TurnError::CorrelationMismatch);
        }
        let items = turn
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .ok_or(TurnError::CorrelationMismatch)?;
        if items.len() > MAX_TURN_ITEMS {
            return Err(TurnError::CorrelationMismatch);
        }
        self.items = Some(items);
        Ok(())
    }

    fn step(&mut self, byte: u8) -> Result<(), TurnError> {
        self.seen = self.seen.saturating_add(1);
        if let Some(element) = self.element.as_mut() {
            if element.len() >= MAX_SOLICITED_TURN_BYTES {
                return Err(TurnError::CorrelationMismatch);
            }
            element.push(byte);
        }
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.in_string = false;
                self.last_significant = b'"';
                if self.reading_name {
                    self.reading_name = false;
                    self.pending_name = Some(std::mem::take(&mut self.name));
                } else if self.reading_element_id {
                    self.reading_element_id = false;
                    self.discard_unless_wanted();
                }
            } else if self.reading_name {
                if self.name.len() >= 256 {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.name.push(byte);
            } else if self.reading_element_id {
                if self.element_id.len() >= 256 {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.element_id.push(byte);
            }
            return Ok(());
        }
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' => return Ok(()),
            b'"' => {
                if self.stack.last().is_some_and(|frame| {
                    !frame.array && matches!(self.last_significant, b'{' | b',')
                }) {
                    self.reading_name = true;
                    self.name.clear();
                } else if self.element_active
                    && self.last_significant == b':'
                    && self.stack.len() == self.element_depth + 1
                    && self.pending_name.as_deref() == Some(b"id")
                {
                    self.reading_element_id = true;
                    self.element_id.clear();
                }
                self.in_string = true;
            }
            b'{' | b'[' => {
                let array = byte == b'[';
                if byte == b'{' && !self.element_active && self.inside_turns_array() {
                    self.element = Some(vec![b'{']);
                    self.element_depth = self.stack.len();
                    self.element_active = true;
                    self.element_id.clear();
                }
                if self.stack.len() >= 256 {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.stack.push(Frame {
                    array,
                    name: self.pending_name.take(),
                });
            }
            b'}' | b']' => {
                let frame = self.stack.pop().ok_or(TurnError::CorrelationMismatch)?;
                if frame.array != (byte == b']') {
                    return Err(TurnError::CorrelationMismatch);
                }
                if self.element_active && self.stack.len() == self.element_depth {
                    if let Some(element) = self.element.as_mut() {
                        element.pop();
                    }
                    self.finish_element(byte)?;
                }
            }
            b',' => self.pending_name = None,
            _ => {}
        }
        self.last_significant = byte;
        Ok(())
    }
}

impl SolicitedSink for ThreadTurnSink {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        for byte in chunk {
            self.step(*byte)
                .map_err(|_| TransportError::CorrelationMismatch)?;
        }
        Ok(())
    }
}

/// One stored final-response artifact, named the way the artifact contract
/// names it: a UUIDv7 identity and the microsecond UTC instant it was written.
///
/// The store returns both because a `final_response` a Machine CLI consumer
/// reads is the published artifact metadata object, which neither a content
/// digest nor a filename can stand in for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredArtifact {
    pub artifact_id: Uuid,
    pub created_at: String,
}

pub trait ResponseArtifactStore {
    fn store(&mut self, bytes: &[u8]) -> Result<StoredArtifact, TurnError>;
}

#[derive(Default)]
pub struct MemoryArtifactStore {
    pub values: BTreeMap<Uuid, Vec<u8>>,
}
impl ResponseArtifactStore for MemoryArtifactStore {
    fn store(&mut self, bytes: &[u8]) -> Result<StoredArtifact, TurnError> {
        let artifact_id = Uuid::now_v7();
        self.values.insert(artifact_id, bytes.to_vec());
        Ok(StoredArtifact {
            artifact_id,
            created_at: SystemLedgerClock::default().timestamp(),
        })
    }
}

#[derive(Clone, Debug)]
struct IdempotencyRecord {
    digest: String,
    accepted: AcceptedTurn,
    terminal: Option<TerminalTurn>,
}

#[derive(Clone, Debug)]
struct InteractionResolution {
    idempotency_key: String,
    response_sha256: Option<String>,
    resolution_receipt_id: Option<Uuid>,
    stale: bool,
}

pub struct TurnCoordinator<S, J, A> {
    server: S,
    journal: J,
    artifacts: A,
    state: CoordinatorState,
    attach: ThreadAttach,
    thread_id: Option<String>,
    thread_generation: u64,
    policy_epoch: u64,
    acceptance_controller: Option<crate::domain::ControllerIdentity>,
    writer_generation: Option<u64>,
    active_turn_id: Option<String>,
    /// The effort the active Turn was started with, so its terminal reports
    /// what it actually ran at rather than the Run's default.
    active_turn_effort: Option<String>,
    active_operation_id: Option<Uuid>,
    recovery_turn_id: Option<String>,
    terminal_turn_ids: BTreeSet<String>,
    pending: BTreeMap<u64, Interaction>,
    resolved_interactions: BTreeMap<u64, InteractionResolution>,
    file_change_snapshots: BTreeMap<(String, String, String), FileChangeSnapshot>,
    completed_item_count: usize,
    ignored_foreign: Vec<IgnoredForeignRequest>,
    /// Foreign-thread requests this Run dropped without a durable diagnostic:
    /// past the recording bound, or because the profile journal refused the
    /// write.  Counted rather than silently lost, and never this Run's fault.
    undiagnosed_foreign: u64,
    streamed_history_bytes: u64,
    idempotency: BTreeMap<String, IdempotencyRecord>,
    unresolved_idempotency: BTreeMap<String, String>,
    fixed_model: String,
    default_effort: String,
    supported_efforts: BTreeSet<String>,
    cwd: PathBuf,
    developer_instructions: String,
    sandbox: String,
    approval_policy: String,
    safety_policy: SessionSafetyPolicy,
    run_generation: u64,
    server_key: String,
    server_epoch: u64,
    run_id: Uuid,
    controller_id: Uuid,
    control_mode: String,
    record_accepted_user_input: bool,
    foreign_diagnostics: Box<dyn ForeignDiagnostics>,
    primary_tool: Option<PrimaryToolConfig>,
    pending_primary_tools: BTreeMap<String, PendingPrimaryToolCall>,
}

pub struct CoordinatorConfig {
    pub attach: ThreadAttach,
    /// The durable authority for foreign-thread observations.
    pub foreign_diagnostics: Box<dyn ForeignDiagnostics>,
    pub fixed_model: String,
    pub default_effort: String,
    pub supported_efforts: BTreeSet<String>,
    pub cwd: PathBuf,
    pub developer_instructions: String,
    pub sandbox: String,
    pub approval_policy: String,
    pub safety_policy: SessionSafetyPolicy,
    pub run_generation: u64,
    pub server_key: String,
    pub server_epoch: u64,
    pub run_id: Uuid,
    pub controller_id: Uuid,
    pub control_mode: String,
    pub record_accepted_user_input: bool,
    pub primary_tool: Option<PrimaryToolConfig>,
}

impl<S: AppServer, J: DurableTurnJournal, A: ResponseArtifactStore> TurnCoordinator<S, J, A> {
    pub fn initialize(
        mut server: S,
        journal: J,
        artifacts: A,
        config: CoordinatorConfig,
        expected_codex_home: &str,
    ) -> Result<Self, TurnError> {
        if config.fixed_model.is_empty()
            || config.default_effort.is_empty()
            || config.run_generation == 0
            || config.server_epoch == 0
            || config.server_key.len() != 64
            || config.run_id.get_version_num() != 7
            || config.controller_id.get_version_num() != 7
            || !matches!(
                config.control_mode.as_str(),
                "direct_interactive" | "managed_agent"
            )
            || !config.cwd.is_absolute()
        {
            return Err(TurnError::InvalidInput(
                "coordinator configuration is incomplete",
            ));
        }
        if !config.supported_efforts.contains(&config.default_effort) {
            return Err(TurnError::EffortUnsupported {
                requested: config.default_effort.clone(),
                supported: config.supported_efforts.iter().cloned().collect(),
            });
        }
        let initialized = server.request("initialize", serde_json::json!({
            "clientInfo": {"name":"dolgorae","title":"Dolgorae","version":env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi":config.primary_tool.is_some(),"optOutNotificationMethods":[]}
        }))?;
        if initialized.get("codexHome").and_then(Value::as_str) != Some(expected_codex_home) {
            return Err(TurnError::CorrelationMismatch);
        }
        server.notify("initialized", serde_json::json!({}))?;
        let thread_id = None;
        let state = CoordinatorState::Threadless;
        Ok(Self {
            server,
            journal,
            artifacts,
            state,
            attach: config.attach,
            thread_id,
            thread_generation: 0,
            policy_epoch: 0,
            acceptance_controller: None,
            writer_generation: None,
            active_turn_id: None,
            active_turn_effort: None,
            active_operation_id: None,
            recovery_turn_id: None,
            terminal_turn_ids: BTreeSet::new(),
            pending: BTreeMap::new(),
            resolved_interactions: BTreeMap::new(),
            file_change_snapshots: BTreeMap::new(),
            completed_item_count: 0,
            ignored_foreign: Vec::new(),
            undiagnosed_foreign: 0,
            streamed_history_bytes: 0,
            idempotency: BTreeMap::new(),
            unresolved_idempotency: BTreeMap::new(),
            fixed_model: config.fixed_model,
            default_effort: config.default_effort,
            supported_efforts: config.supported_efforts,
            cwd: config.cwd,
            developer_instructions: config.developer_instructions,
            sandbox: config.sandbox,
            approval_policy: config.approval_policy,
            safety_policy: config.safety_policy,
            run_generation: config.run_generation,
            server_key: config.server_key,
            server_epoch: config.server_epoch,
            run_id: config.run_id,
            controller_id: config.controller_id,
            control_mode: config.control_mode,
            record_accepted_user_input: config.record_accepted_user_input,
            foreign_diagnostics: config.foreign_diagnostics,
            primary_tool: config.primary_tool,
            pending_primary_tools: BTreeMap::new(),
        })
    }

    #[must_use]
    pub const fn state(&self) -> &CoordinatorState {
        &self.state
    }
    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }
    /// The one model this Run may use; a Turn that names another is refused.
    #[must_use]
    pub fn fixed_model(&self) -> &str {
        &self.fixed_model
    }
    #[must_use]
    pub fn active_turn_id(&self) -> Option<&str> {
        self.active_turn_id.as_deref()
    }
    pub(crate) fn default_effort(&self) -> &str {
        &self.default_effort
    }

    /// The reasoning effort the active Turn was started with.
    #[must_use]
    pub fn active_turn_effort(&self) -> Option<&str> {
        self.active_turn_effort.as_deref()
    }
    #[must_use]
    pub fn pending_interactions(&self) -> Vec<&Interaction> {
        self.pending.values().collect()
    }

    pub(crate) fn set_acceptance_controller(
        &mut self,
        controller: crate::domain::ControllerIdentity,
    ) {
        self.acceptance_controller = Some(controller);
    }

    pub(crate) fn acceptance_controller(&self) -> Option<&crate::domain::ControllerIdentity> {
        self.acceptance_controller.as_ref()
    }

    fn next_unverified_policy(
        &self,
        writer_generation: Option<u64>,
    ) -> Result<crate::domain::EffectivePolicy, TurnError> {
        Ok(crate::domain::EffectivePolicy {
            access: crate::domain::Access::Unknown,
            verification: crate::domain::PolicyVerification::Unverified,
            policy_epoch: crate::domain::PolicyEpoch(
                self.policy_epoch
                    .checked_add(1)
                    .ok_or(TurnError::OutcomeUnknown)?,
            ),
            thread_generation: (self.thread_generation != 0).then_some(self.thread_generation),
            server_epoch: Some(self.server_epoch),
            writer_generation,
        })
    }

    pub fn set_writer_access(
        &mut self,
        write: bool,
        writer_generation: u64,
        transaction_id: Uuid,
    ) -> Result<(), TurnError> {
        if writer_generation == 0 || transaction_id.get_version_num() != 7 {
            return Err(TurnError::InvalidInput(
                "writer transition identity is invalid",
            ));
        }
        if matches!(
            self.state,
            CoordinatorState::Running
                | CoordinatorState::WaitingInteraction
                | CoordinatorState::OutcomeUnknown
        ) {
            return Err(TurnError::TurnBusy);
        }
        if write && self.safety_policy == SessionSafetyPolicy::ReviewerReadOnly {
            return Err(TurnError::InvalidInput(
                "Reviewer read-only policy cannot acquire writer access",
            ));
        }
        let effective_policy = self.next_unverified_policy(write.then_some(writer_generation))?;
        let policy_epoch = effective_policy.policy_epoch.0;
        self.journal
            .append_and_sync(JournalEntry::WriterAccessChanged {
                write,
                writer_generation,
                transaction_id,
                effective_policy,
            })?;
        self.policy_epoch = policy_epoch;
        self.writer_generation = write.then_some(writer_generation);
        self.sandbox = if write {
            "workspace-write"
        } else {
            "read-only"
        }
        .to_owned();
        self.approval_policy = if write { "on-request" } else { "never" }.to_owned();
        Ok(())
    }

    /// Restore only the lifecycle facts a restarted worker may safely act on.
    /// An in-flight durable state is quarantined as outcome-unknown in memory;
    /// no accepted input is ever replayed. Reconciliation is the only method
    /// that may consult the pinned thread history and move it onward.
    pub fn restore_durable_state(
        &mut self,
        lifecycle: RunLifecycle,
        thread_id: Option<String>,
        active_turn_id: Option<String>,
        latest_turn_id: Option<String>,
    ) {
        self.thread_id = thread_id;
        self.recovery_turn_id = active_turn_id.or(latest_turn_id);
        self.state = match lifecycle {
            RunLifecycle::Paused => CoordinatorState::Paused,
            RunLifecycle::Running
            | RunLifecycle::WaitingInteraction
            | RunLifecycle::ReconciliationRequired
            | RunLifecycle::OutcomeUnknown => CoordinatorState::OutcomeUnknown,
            _ => {
                if self.thread_id.is_some() {
                    CoordinatorState::Idle
                } else {
                    CoordinatorState::Threadless
                }
            }
        };
    }

    /// Rebuild accepted Turn receipts from the durable operation chain.
    /// Reservations without proven acceptance remain fenced even if
    /// recovery subsequently makes the Run idle again.
    pub fn restore_turn_receipts(
        &mut self,
        records: Vec<(AuditKind, Value)>,
    ) -> Result<(), TurnError> {
        for (_, payload) in &records {
            if let Some(policy) = payload.get("effective_policy") {
                let policy: crate::domain::EffectivePolicy = serde_json::from_value(policy.clone())
                    .map_err(|_| {
                        TurnError::Journal("durable effective policy is invalid".to_owned())
                    })?;
                if policy.policy_epoch.0 > self.policy_epoch {
                    self.policy_epoch = policy.policy_epoch.0;
                    self.writer_generation = policy.writer_generation;
                }
            }
        }
        let restored = restore_turn_receipt_maps(records)?;
        self.idempotency = restored.accepted;
        self.unresolved_idempotency = restored.unresolved;
        Ok(())
    }

    /// Restore the durable response receipts needed to make a retried
    /// interaction response idempotent after a worker restart. The upstream
    /// response is never replayed; only an exact winning key can recover its
    /// already-recorded result.
    pub fn restore_interaction_resolutions(
        &mut self,
        decisions: Vec<Value>,
    ) -> Result<(), TurnError> {
        for decision in decisions {
            let request_id = decision
                .get("request_id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| {
                    TurnError::Journal("durable interaction request id is invalid".to_owned())
                })?;
            let idempotency_key = decision
                .get("idempotency_key")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    TurnError::Journal("durable interaction idempotency key is invalid".to_owned())
                })?
                .to_owned();
            let response_sha256 = match decision.get("response_sha256") {
                None | Some(Value::Null) => None,
                Some(value) => Some(value.as_str().map(str::to_owned).ok_or_else(|| {
                    TurnError::Journal("durable interaction response digest is invalid".to_owned())
                })?),
            };
            let resolution_receipt_id = match decision.get("resolution_receipt_id") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .and_then(|value| Uuid::parse_str(value).ok())
                        .ok_or_else(|| {
                            TurnError::Journal("durable interaction receipt is invalid".to_owned())
                        })?,
                ),
            };
            let stale = decision
                .pointer("/resolution/outcome")
                .and_then(Value::as_str)
                == Some("stale");
            // Request ids are app-server scoped and may be reused by a later
            // physical generation. Ledger order is authoritative, just as it
            // is for the durable observer projection, so the latest decision
            // replaces an older receipt with the same upstream id.
            self.resolved_interactions.insert(
                request_id,
                InteractionResolution {
                    idempotency_key,
                    response_sha256,
                    resolution_receipt_id,
                    stale,
                },
            );
        }
        Ok(())
    }

    /// Read the pinned thread without resuming it or starting a Turn. A
    /// terminal observation moves the run to paused; absence remains
    /// outcome-unknown. Transport and shape failures append nothing, so a
    /// transient read cannot manufacture a lifecycle transition.
    pub fn reconcile_history(&mut self) -> Result<bool, TurnError> {
        if self.state != CoordinatorState::OutcomeUnknown {
            return Err(TurnError::TurnBusy);
        }
        let thread_id = self.thread_id.clone().ok_or(TurnError::OutcomeUnknown)?;
        let turn_id = self
            .recovery_turn_id
            .clone()
            .ok_or(TurnError::OutcomeUnknown)?;
        let result = self.server.request(
            "thread/read",
            serde_json::json!({"threadId": thread_id, "includeTurns": true}),
        )?;
        let turns = result
            .pointer("/thread/turns")
            .or_else(|| result.get("turns"))
            .and_then(Value::as_array)
            .ok_or(TurnError::CorrelationMismatch)?;
        let status = turns
            .iter()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id.as_str()))
            .and_then(|turn| turn.get("status").and_then(Value::as_str))
            .map(str::to_owned);
        if status
            .as_deref()
            .is_some_and(|value| !matches!(value, "completed" | "interrupted" | "failed"))
        {
            return Err(TurnError::CorrelationMismatch);
        }
        self.journal.append_and_sync(JournalEntry::Reconciliation {
            thread_id,
            turn_id,
            observed_status: status.clone(),
            server_key: self.server_key.clone(),
            server_epoch: self.server_epoch,
        })?;
        if status.is_some() {
            self.journal
                .append_and_sync(JournalEntry::LifecycleTransition {
                    previous: RunLifecycle::ReconciliationRequired,
                    current: RunLifecycle::Paused,
                    terminal_seal: false,
                    interrupt_terminal_confirmed: false,
                })?;
            self.state = CoordinatorState::Paused;
            self.recovery_turn_id = None;
            Ok(true)
        } else {
            self.journal.append_and_sync(JournalEntry::OutcomeUnknown {
                operation_id: Uuid::now_v7(),
            })?;
            Ok(false)
        }
    }

    pub fn pause(&mut self, interrupt_terminal_confirmed: bool) -> Result<(), TurnError> {
        let previous = match self.state {
            CoordinatorState::Threadless | CoordinatorState::Idle => RunLifecycle::Idle,
            CoordinatorState::Running => RunLifecycle::Running,
            CoordinatorState::WaitingInteraction => RunLifecycle::WaitingInteraction,
            CoordinatorState::Paused => return Ok(()),
            CoordinatorState::OutcomeUnknown => RunLifecycle::OutcomeUnknown,
        };
        if matches!(
            self.state,
            CoordinatorState::Running | CoordinatorState::WaitingInteraction
        ) && !interrupt_terminal_confirmed
        {
            return Err(TurnError::TurnBusy);
        }
        self.journal
            .append_and_sync(JournalEntry::LifecycleTransition {
                previous,
                current: RunLifecycle::Paused,
                terminal_seal: false,
                interrupt_terminal_confirmed,
            })?;
        self.state = CoordinatorState::Paused;
        self.pending.clear();
        self.active_turn_id = None;
        self.active_turn_effort = None;
        self.active_operation_id = None;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), TurnError> {
        if self.state != CoordinatorState::Paused {
            return Err(TurnError::TurnBusy);
        }
        self.journal
            .append_and_sync(JournalEntry::LifecycleTransition {
                previous: RunLifecycle::Paused,
                current: RunLifecycle::Idle,
                terminal_seal: false,
                interrupt_terminal_confirmed: false,
            })?;
        self.state = CoordinatorState::Idle;
        Ok(())
    }

    pub fn seal_closed(&mut self, interrupt_terminal_confirmed: bool) -> Result<(), TurnError> {
        let previous = match self.state {
            CoordinatorState::Threadless | CoordinatorState::Idle => RunLifecycle::Idle,
            CoordinatorState::Paused => RunLifecycle::Paused,
            CoordinatorState::OutcomeUnknown => RunLifecycle::OutcomeUnknown,
            CoordinatorState::Running => RunLifecycle::Running,
            CoordinatorState::WaitingInteraction => RunLifecycle::WaitingInteraction,
        };
        self.journal.append_and_sync(JournalEntry::CleanupResult {
            outcome: "owned_generation_cleanup_required_before_command_returns".to_owned(),
        })?;
        self.journal
            .append_and_sync(JournalEntry::LifecycleTransition {
                previous,
                current: RunLifecycle::Closed,
                terminal_seal: true,
                interrupt_terminal_confirmed,
            })
    }

    /// Bounded diagnostics for server requests that named a Thread this Run does
    /// not own.  They are refused and ignored, never treated as this Run losing
    /// track of its own outcome.
    #[must_use]
    pub fn ignored_foreign_requests(&self) -> &[IgnoredForeignRequest] {
        &self.ignored_foreign
    }
    /// How many foreign-thread requests were dropped without a durable
    /// diagnostic, because the bound was reached or the profile journal
    /// refused the write.  Neither is this Run's outcome.
    #[must_use]
    pub const fn undiagnosed_foreign_requests(&self) -> u64 {
        self.undiagnosed_foreign
    }
    #[must_use]
    pub const fn streamed_history_bytes(&self) -> u64 {
        self.streamed_history_bytes
    }
    #[must_use]
    pub const fn journal(&self) -> &J {
        &self.journal
    }
    #[must_use]
    pub const fn artifacts(&self) -> &A {
        &self.artifacts
    }

    /// Resolve an accepted retry without performing new work. Authority must
    /// be checked by the caller before using this result to bypass admission.
    fn replay_accepted(&self, request: &TurnRequest) -> Result<Option<AcceptedTurn>, TurnError> {
        if request.idempotency_key.is_empty()
            || request.idempotency_key.len() > 1024
            || request.message.is_empty()
        {
            return Err(TurnError::InvalidInput(
                "message and idempotency key must be nonempty and bounded",
            ));
        }
        if request.model != self.fixed_model {
            return Err(TurnError::ModelMismatch {
                expected: self.fixed_model.clone(),
                actual: request.model.clone(),
            });
        }
        let effort = request
            .effort
            .clone()
            .unwrap_or_else(|| self.default_effort.clone());
        if !self.supported_efforts.contains(&effort) {
            return Err(TurnError::EffortUnsupported {
                requested: effort.clone(),
                supported: self.supported_efforts.iter().cloned().collect(),
            });
        }
        for image in &request.images {
            image.verify()?;
        }
        let digest = request_digest(request, &effort)?;
        if let Some(recorded) = self.unresolved_idempotency.get(&request.idempotency_key) {
            return Err(if recorded == &digest {
                TurnError::OutcomeUnknown
            } else {
                TurnError::IdempotencyConflict {
                    recorded: recorded.clone(),
                    observed: digest,
                }
            });
        }
        if let Some(record) = self.idempotency.get(&request.idempotency_key) {
            if record.digest != digest {
                return Err(TurnError::IdempotencyConflict {
                    recorded: record.digest.clone(),
                    observed: digest.clone(),
                });
            }
            let mut accepted = record.accepted.clone();
            accepted.replayed = true;
            return Ok(Some(accepted));
        }
        Ok(None)
    }

    pub(crate) fn replay_delivery(
        &self,
        request: &TurnRequest,
    ) -> Result<Option<DeliveryResult>, TurnError> {
        let Some(accepted) = self.replay_accepted(request)? else {
            if self.state == CoordinatorState::OutcomeUnknown {
                return Err(TurnError::OutcomeUnknown);
            }
            return Ok(None);
        };
        if request.delivery == DeliveryMode::Send
            && let Some(terminal) = self
                .idempotency
                .get(&request.idempotency_key)
                .and_then(|record| record.terminal.clone())
        {
            return Ok(Some(DeliveryResult::Terminal(terminal)));
        }
        Ok(Some(DeliveryResult::Accepted(accepted)))
    }

    pub fn start_turn(&mut self, request: TurnRequest) -> Result<AcceptedTurn, TurnError> {
        if self.state == CoordinatorState::OutcomeUnknown {
            return Err(TurnError::OutcomeUnknown);
        }
        if let Some(accepted) = self.replay_accepted(&request)? {
            return Ok(accepted);
        }
        let effort = request
            .effort
            .clone()
            .unwrap_or_else(|| self.default_effort.clone());
        let digest = request_digest(&request, &effort)?;
        if matches!(
            self.state,
            CoordinatorState::Running
                | CoordinatorState::WaitingInteraction
                | CoordinatorState::Paused
                | CoordinatorState::OutcomeUnknown
        ) {
            return Err(TurnError::TurnBusy);
        }
        let accepted_input = self
            .record_accepted_user_input
            .then(|| self.capture_accepted_input(&request))
            .transpose()?;
        let operation_id = Uuid::now_v7();
        let intent = OperationIntent {
            operation_id,
            idempotency_key: request.idempotency_key.clone(),
            request_sha256: digest.clone(),
            run_generation: self.run_generation,
            server_key: self.server_key.clone(),
            server_epoch: self.server_epoch,
            provisional_thread: self.thread_id.is_none(),
            accepted_input,
        };
        self.journal.append_and_sync(JournalEntry::Intent(intent))?;
        self.unresolved_idempotency
            .insert(request.idempotency_key.clone(), digest.clone());
        let thread_id = match self.ensure_thread(operation_id) {
            Ok(thread) => thread,
            Err(error) => {
                if matches!(error, TurnError::AppServerRejected { .. }) {
                    self.unresolved_idempotency.remove(&request.idempotency_key);
                }
                return Err(self.lost_after_write(operation_id, error));
            }
        };
        let input = turn_input(&request)?;
        let result = match self.server.request("turn/start", serde_json::json!({
            "threadId": thread_id, "input": input, "model": self.fixed_model, "effort": effort,
            "sandboxPolicy": turn_sandbox(&self.sandbox, &self.cwd), "approvalPolicy": self.approval_policy
        })) {
            Ok(result) => result,
            Err(error) => {
                if matches!(error, TurnError::AppServerRejected { .. }) { self.unresolved_idempotency.remove(&request.idempotency_key); }
                return Err(self.lost_after_write(operation_id, error));
            },
        };
        let Some(turn_id) =
            nested_identity(&result, "turn", "id").or_else(|| identity(&result, "turnId"))
        else {
            self.mark_unknown(operation_id)?;
            return Err(TurnError::CorrelationMismatch);
        };
        // `thread_id` is installed as soon as `thread/start` yields the
        // provisional identity, but the permanent binding begins only when a
        // `turn/start` answer supplies an accepted Turn.  A definitive first
        // Turn refusal deliberately leaves the provisional Thread available
        // for a retry, so whether binding is still pending must come from the
        // durable generation rather than from whether this invocation called
        // `thread/start` itself.
        if self.thread_generation == 0 {
            self.thread_generation = self
                .thread_generation
                .checked_add(1)
                .ok_or(TurnError::OutcomeUnknown)?;
            let effective_policy = self.next_unverified_policy(self.writer_generation)?;
            let policy_epoch = effective_policy.policy_epoch.0;
            self.journal.append_and_sync(JournalEntry::ThreadBound {
                operation_id,
                thread_id: thread_id.clone(),
                thread_generation: self.thread_generation,
                effective_policy,
            })?;
            self.policy_epoch = policy_epoch;
        }
        self.journal.append_and_sync(JournalEntry::TurnAccepted {
            operation_id,
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            effort: effort.clone(),
            effective_policy: crate::domain::EffectivePolicy {
                access: crate::domain::Access::Unknown,
                verification: crate::domain::PolicyVerification::Unverified,
                policy_epoch: crate::domain::PolicyEpoch(self.policy_epoch),
                thread_generation: Some(self.thread_generation),
                server_epoch: Some(self.server_epoch),
                writer_generation: self.writer_generation,
            },
            controller: self.acceptance_controller.clone(),
        })?;
        self.active_turn_id = Some(turn_id.clone());
        self.active_turn_effort = Some(effort.clone());
        self.active_operation_id = Some(operation_id);
        self.state = CoordinatorState::Running;
        let accepted = AcceptedTurn {
            thread_id,
            turn_id,
            effort,
            replayed: false,
        };
        self.unresolved_idempotency.remove(&request.idempotency_key);
        self.idempotency.insert(
            request.idempotency_key,
            IdempotencyRecord {
                digest,
                accepted: accepted.clone(),
                terminal: None,
            },
        );
        Ok(accepted)
    }

    fn capture_accepted_input(
        &mut self,
        request: &TurnRequest,
    ) -> Result<AcceptedUserInput, TurnError> {
        const MAX_INLINE_INPUT_BYTES: usize = 1024 * 1024;
        const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
        if request.message.len() > MAX_INPUT_BYTES {
            return Err(TurnError::InvalidInput(
                "message exceeds the 8 MiB request bound",
            ));
        }
        if request.images.len() > 64 {
            return Err(TurnError::InvalidInput("at most 64 images are accepted"));
        }
        let bytes = request.message.as_bytes();
        let stored = self.artifacts.store(bytes)?;
        let storage = AcceptedInputStorage {
            artifact_id: stored.artifact_id,
            created_at: stored.created_at,
            byte_length: u64::try_from(bytes.len())
                .map_err(|_| TurnError::InvalidInput("message is too large"))?,
            sha256: sha256_hex(bytes),
        };
        let content = if bytes.len() <= MAX_INLINE_INPUT_BYTES {
            AcceptedInputContent::Inline { storage }
        } else {
            AcceptedInputContent::Artifact { storage }
        };
        let images = request
            .images
            .iter()
            .enumerate()
            .map(|(ordinal, image)| {
                Ok(AcceptedImageMetadata {
                    ordinal: u32::try_from(ordinal)
                        .map_err(|_| TurnError::InvalidInput("too many images"))?,
                    detail: image.detail,
                    media_type: image_media_type(&image.canonical_path)?,
                    byte_length: image.byte_length,
                    sha256: image.sha256.clone(),
                })
            })
            .collect::<Result<Vec<_>, TurnError>>()?;
        Ok(AcceptedUserInput { content, images })
    }

    /// Start a Turn and return as soon as the app-server has accepted it, or
    /// return the terminal a replayed idempotency key already earned.
    ///
    /// This is the half of `deliver` that never waits.  A worker that drains
    /// the app-server on its own thread accepts here and reports the outcome
    /// from the drain, so no caller has to hold the Run still while a Turn runs.
    pub fn accept(&mut self, request: TurnRequest) -> Result<DeliveryResult, TurnError> {
        if let Some(replayed) = self.replay_delivery(&request)? {
            return Ok(replayed);
        }
        let accepted = self.start_turn(request)?;
        Ok(DeliveryResult::Accepted(accepted))
    }

    pub fn deliver(&mut self, request: TurnRequest) -> Result<DeliveryResult, TurnError> {
        let mode = request.delivery;
        let replayed = self.idempotency.contains_key(&request.idempotency_key);
        let accepted = match self.accept(request)? {
            DeliveryResult::Accepted(accepted) => accepted,
            settled => return Ok(settled),
        };
        if replayed || mode == DeliveryMode::Submit {
            return Ok(DeliveryResult::Accepted(accepted));
        }
        loop {
            if let Some(terminal) = self.next_event()? {
                return Ok(DeliveryResult::Terminal(terminal));
            }
            if self.state == CoordinatorState::WaitingInteraction {
                return Ok(DeliveryResult::WaitingInteraction {
                    accepted,
                    requests: self.pending.values().cloned().collect(),
                });
            }
        }
    }

    fn ensure_thread(&mut self, operation_id: Uuid) -> Result<String, TurnError> {
        if let Some(thread_id) = &self.thread_id {
            return Ok(thread_id.clone());
        }
        let common = serde_json::json!({
            "cwd": self.cwd, "model": self.fixed_model, "sandbox": self.sandbox,
            "approvalPolicy": self.approval_policy, "developerInstructions": self.developer_instructions
        });
        let (method, mut params) = match &self.attach {
            ThreadAttach::Start => ("thread/start", common),
            ThreadAttach::Resume { thread_id } => {
                let mut value = common;
                value["threadId"] = Value::String(thread_id.clone());
                ("thread/resume", value)
            }
            ThreadAttach::Fork {
                source_thread_id,
                last_turn_id,
            } => {
                let mut value = common;
                value["threadId"] = Value::String(source_thread_id.clone());
                value["lastTurnId"] = Value::String(last_turn_id.clone());
                ("thread/fork", value)
            }
        };
        if !params.is_object() {
            return Err(TurnError::InvalidInput("thread parameters are invalid"));
        }
        if method == "thread/start"
            && let Some(primary_tool) = &self.primary_tool
        {
            params["dynamicTools"] = Value::Array(vec![primary_tool.contract.tool_spec()]);
        }
        let result = self.server.request(method, params.take())?;
        let thread_id = nested_identity(&result, "thread", "id")
            .or_else(|| identity(&result, "threadId"))
            .ok_or(TurnError::CorrelationMismatch)?;
        self.journal
            .append_and_sync(JournalEntry::ProvisionalThread {
                operation_id,
                thread_id: thread_id.clone(),
            })?;
        self.thread_id = Some(thread_id.clone());
        Ok(thread_id)
    }

    /// Create and durably bind a thread before the first task Turn. Brokered
    /// Specialist provisioning uses the spawn operation as the binding
    /// identity so a ready member never exposes a threadless publication gap.
    pub(crate) fn initialize_thread(&mut self, operation_id: Uuid) -> Result<(), TurnError> {
        if self.thread_id.is_some() {
            return Ok(());
        }
        if self.state != CoordinatorState::Threadless {
            return Err(TurnError::TurnBusy);
        }
        let thread_id = match self.ensure_thread(operation_id) {
            Ok(thread_id) => thread_id,
            Err(error) => return Err(self.lost_after_write(operation_id, error)),
        };
        self.thread_generation = self
            .thread_generation
            .checked_add(1)
            .ok_or(TurnError::OutcomeUnknown)?;
        let effective_policy = self.next_unverified_policy(self.writer_generation)?;
        self.policy_epoch = effective_policy.policy_epoch.0;
        self.journal.append_and_sync(JournalEntry::ThreadBound {
            operation_id,
            thread_id,
            thread_generation: self.thread_generation,
            effective_policy,
        })?;
        self.state = CoordinatorState::Idle;
        Ok(())
    }

    fn mark_unknown(&mut self, operation_id: Uuid) -> Result<(), TurnError> {
        self.journal
            .append_and_sync(JournalEntry::OutcomeUnknown { operation_id })?;
        self.active_turn_id = None;
        self.active_turn_effort = None;
        self.active_operation_id = None;
        self.state = CoordinatorState::OutcomeUnknown;
        Ok(())
    }

    /// Record that an operation which had already written to the app-server
    /// lost its answer, and restate the fault as the uncertainty it is.
    ///
    /// docs/specs/README.md: "`TRANSPORT_FAILURE` is retryable only when the operation
    /// made no external write ... Any uncertain acceptance emits `false`."
    /// Past an external write the caller cannot safely reissue the identical
    /// invocation, and the Run has just quarantined itself durably, so the
    /// reported fault is the same `outcome_unknown` the ledger now holds
    /// rather than a retryable transport hiccup.
    fn lost_after_write(&mut self, operation_id: Uuid, error: TurnError) -> TurnError {
        // A refusal the app-server correlated to this request is not a lost
        // write.  The app-server received it, decided it, and answered it, so
        // the Run knows exactly what happened: nothing.  Quarantining here
        // would spend a whole Run generation on an app-server saying "no".
        if matches!(error, TurnError::AppServerRejected { .. }) {
            return error;
        }
        if let Err(journal) = self.mark_unknown(operation_id) {
            return journal;
        }
        match error {
            TurnError::Transport { .. } => TurnError::OutcomeUnknown,
            other => other,
        }
    }

    /// Give up an accepted Turn whose outcome this Run can no longer observe,
    /// and restate the fault that ended it as the uncertainty it is.
    ///
    /// A transport loss under a Turn that was already accepted is not the
    /// retryable `TRANSPORT_FAILURE` the contract admits: that code promises
    /// "the operation made no external write", and this Turn is running on the
    /// app-server right now.  What every waiting caller is owed is the
    /// `outcome_unknown` the ledger has just recorded, nonretryable, so nobody
    /// reissues a Turn that may already be underway.
    pub fn lost_in_flight(&mut self, error: TurnError) -> TurnError {
        let accepted = self.active_turn_id.is_some();
        if let Err(journal) = self.abandon() {
            return journal;
        }
        match error {
            TurnError::Transport { .. } if accepted => TurnError::OutcomeUnknown,
            other => other,
        }
    }

    pub fn next_event(&mut self) -> Result<Option<TerminalTurn>, TurnError> {
        let message = match self.server.next_message() {
            Ok(message) => message,
            Err(error) => return Err(self.lost_in_flight(error)),
        };
        self.ingest(message)
    }

    /// Fold one already-received app-server message into this Run's state.
    ///
    /// Splitting this out from `next_event` is what lets the worker read on one
    /// thread and own Run state on another: the reader never touches the Run,
    /// and the owner never blocks on the socket.
    pub fn ingest(&mut self, message: Value) -> Result<Option<TerminalTurn>, TurnError> {
        let result = self.handle_message(message);
        if result.is_err() {
            self.abandon()?;
        }
        result
    }

    /// Record that this Run can no longer observe the outcome of the Turn it
    /// has in flight, which is what a lost transport means.
    pub fn abandon(&mut self) -> Result<(), TurnError> {
        if let Some(operation_id) = self.active_operation_id {
            self.mark_unknown(operation_id)?;
        }
        Ok(())
    }

    fn handle_message(&mut self, message: Value) -> Result<Option<TerminalTurn>, TurnError> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        let params = message
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(request_id) = message.get("id").and_then(Value::as_u64) {
            self.open_interaction(request_id, method, &params)?;
            return Ok(None);
        }
        match method {
            "item/started" => {
                if params.pointer("/item/type").and_then(Value::as_str) == Some("fileChange") {
                    let item = params.get("item").ok_or(TurnError::CorrelationMismatch)?;
                    self.capture_file_change_snapshot(
                        &params,
                        item.get("id").and_then(Value::as_str),
                        item.get("changes"),
                        false,
                    )?;
                }
                Ok(None)
            }
            "item/fileChange/patchUpdated" => {
                self.capture_file_change_snapshot(
                    &params,
                    params.get("itemId").and_then(Value::as_str),
                    params.get("changes"),
                    true,
                )?;
                Ok(None)
            }
            "item/completed" => {
                self.capture_completed_item(&params)?;
                Ok(None)
            }
            "turn/completed" => {
                if params.get("threadId").and_then(Value::as_str) != self.thread_id.as_deref() {
                    return Ok(None);
                }
                self.complete_turn(&params).map(Some)
            }
            _ => {
                if let Some(thread) = params.get("threadId").and_then(Value::as_str)
                    && self.thread_id.as_deref() != Some(thread)
                {
                    return Ok(None);
                }
                if let Some(turn) = event_turn_id(&params)
                    && self.active_turn_id.as_deref() != Some(turn)
                {
                    return Err(TurnError::CorrelationMismatch);
                }
                Ok(None)
            }
        }
    }

    fn capture_file_change_snapshot(
        &mut self,
        params: &Value,
        item_id: Option<&str>,
        changes: Option<&Value>,
        update: bool,
    ) -> Result<(), TurnError> {
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        if self.thread_id.as_deref() != Some(thread_id) {
            return Ok(());
        }
        let turn_id = event_turn_id(params).ok_or(TurnError::CorrelationMismatch)?;
        if self.active_turn_id.as_deref() != Some(turn_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        let item_id = item_id
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or(TurnError::CorrelationMismatch)?;
        let normalized = normalize_file_changes(
            changes
                .and_then(Value::as_array)
                .ok_or(TurnError::CorrelationMismatch)?,
            &self.cwd,
        )?;
        let bytes = canonical_json_bytes(
            &serde_json::to_value(&normalized).map_err(|_| TurnError::CorrelationMismatch)?,
        )?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(TurnError::CorrelationMismatch);
        }
        let key = (thread_id.to_owned(), turn_id.to_owned(), item_id.to_owned());
        let revision = if update {
            self.file_change_snapshots
                .get(&key)
                .ok_or(TurnError::CorrelationMismatch)?
                .revision
                .checked_add(1)
                .ok_or(TurnError::CorrelationMismatch)?
        } else if self.file_change_snapshots.contains_key(&key) {
            return Err(TurnError::CorrelationMismatch);
        } else {
            0
        };
        let snapshot_sha256 = sha256_hex(&bytes);
        self.journal
            .append_and_sync(JournalEntry::FileChangeSnapshot {
                thread_id: thread_id.to_owned(),
                turn_id: turn_id.to_owned(),
                item_id: item_id.to_owned(),
                revision,
                snapshot_sha256: snapshot_sha256.clone(),
            })?;
        self.file_change_snapshots.insert(
            key,
            FileChangeSnapshot {
                revision,
                changes: normalized,
                snapshot_sha256,
            },
        );
        if update {
            let stale = self
                .pending
                .iter()
                .filter_map(|(request_id, interaction)| {
                    (interaction.method == "item/fileChange/requestApproval"
                        && interaction.thread_id == thread_id
                        && interaction.turn_id == turn_id
                        && interaction.payload.get("itemId").and_then(Value::as_str)
                            == Some(item_id))
                    .then_some(*request_id)
                })
                .collect::<Vec<_>>();
            for request_id in stale {
                self.journal
                    .append_and_sync(JournalEntry::ApprovalDecided {
                        request_id,
                        idempotency_key: String::new(),
                        response_sha256: None,
                        resolution_receipt_id: None,
                        resolved_at: SystemLedgerClock::default().timestamp(),
                        resolution: serde_json::json!({
                            "outcome": "stale",
                            "reason": "file_change_snapshot_changed",
                        }),
                    })?;
                self.journal
                    .append_and_sync(JournalEntry::InteractionResolved { request_id })?;
                self.pending.remove(&request_id);
                self.resolved_interactions.insert(
                    request_id,
                    InteractionResolution {
                        idempotency_key: String::new(),
                        response_sha256: None,
                        resolution_receipt_id: None,
                        stale: true,
                    },
                );
            }
            if self.pending.is_empty() && self.state == CoordinatorState::WaitingInteraction {
                self.state = CoordinatorState::Running;
            }
        }
        Ok(())
    }

    fn open_interaction(
        &mut self,
        request_id: u64,
        method: &str,
        params: &Value,
    ) -> Result<(), TurnError> {
        let thread_id = params.get("threadId").and_then(Value::as_str);
        let turn_id = event_turn_id(params);
        // The Thread check comes before the method check on purpose: answering
        // an unsupported method that named another Thread would still be a
        // reply this connection has no authority to send into that Thread.
        if thread_id.is_none() || self.thread_id.as_deref() != thread_id {
            // Another Thread on the shared app-server asked this connection to
            // decide something.  That is not evidence about this Run's own Turn,
            // so record it and keep going rather than surrendering the outcome.
            self.ignore_foreign_request(request_id, method, thread_id, turn_id);
            return Ok(());
        }
        if method == "item/tool/call" {
            return self.open_primary_tool(request_id, params);
        }
        let supported = matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/tool/requestUserInput"
        );
        let recognized_unsupported = matches!(
            method,
            "item/permissions/requestApproval" | "mcpServer/elicitation/request"
        );
        if recognized_unsupported {
            let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) else {
                return Err(TurnError::CorrelationMismatch);
            };
            let public_request_id = Uuid::now_v7();
            let observed_at = SystemLedgerClock::default().timestamp();
            self.journal
                .append_and_sync(JournalEntry::ApprovalRequested {
                    interaction: serde_json::json!({
                        "interaction": {
                            "schema_version": 1,
                            "request_id": public_request_id,
                            "run_id": self.run_id,
                            "controller_id": self.controller_id,
                            "control_mode": self.control_mode,
                            "thread_id": thread_id,
                            "turn_id": turn_id,
                            "item_id": Value::Null,
                            "run_generation": self.run_generation,
                            "server_epoch": self.server_epoch,
                            "kind": "unsupported_request",
                            "status": "resolved",
                            "payload": {"method": method, "reason": "recognized_unsupported"},
                            "available_decisions": [],
                            "response_schema": "dolgorae.interaction.unsupported/v1",
                            "opened_at": observed_at.clone(),
                            "resolved_at": observed_at,
                            "resolution": {"outcome": "method_not_found", "reply_code": -32601},
                        },
                        "upstream_request_id": request_id,
                    }),
                })?;
            self.journal
                .append_and_sync(JournalEntry::InteractionOpened {
                    request_id,
                    method: method.to_owned(),
                })?;
            self.journal
                .append_and_sync(JournalEntry::InteractionResolved { request_id })?;
            self.server
                .respond_error(request_id, -32601, "method not supported")?;
            return Ok(());
        }
        if matches!(
            method,
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
        ) && self.approval_policy == "never"
        {
            self.journal
                .append_and_sync(JournalEntry::InteractionOpened {
                    request_id,
                    method: method.to_owned(),
                })?;
            self.journal
                .append_and_sync(JournalEntry::InteractionResolved { request_id })?;
            self.server
                .respond_error(request_id, -32601, "method not supported")?;
            return Ok(());
        }
        if !supported {
            self.server
                .respond_error(request_id, -32601, "method not supported")?;
            return Ok(());
        }
        if self.safety_policy == SessionSafetyPolicy::ReviewerReadOnly {
            self.server.respond_error(
                request_id,
                -32000,
                "Reviewer read-only policy denies interactive authority",
            )?;
            return Ok(());
        }
        let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) else {
            return Err(TurnError::CorrelationMismatch);
        };
        if self.active_turn_id.as_deref() != Some(turn_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        let raw = serde_json::to_vec(params).map_err(|_| TurnError::CorrelationMismatch)?;
        if raw.len() > MAX_INTERACTION_PAYLOAD_BYTES || self.pending.contains_key(&request_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        let file_snapshot = if method == "item/fileChange/requestApproval" {
            let item_id = params
                .get("itemId")
                .and_then(Value::as_str)
                .ok_or(TurnError::CorrelationMismatch)?;
            self.file_change_snapshots
                .get(&(thread_id.to_owned(), turn_id.to_owned(), item_id.to_owned()))
                .map(|snapshot| (snapshot.revision, snapshot.snapshot_sha256.clone()))
                .ok_or(TurnError::CorrelationMismatch)?
                .into()
        } else {
            None
        };
        let interaction = Interaction {
            request_id,
            method: method.to_owned(),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            payload_sha256: sha256_hex(&raw),
            byte_length: raw.len(),
            payload: params.clone(),
            file_snapshot_revision: file_snapshot.as_ref().map(|value| value.0),
            file_snapshot_sha256: file_snapshot.map(|value| value.1),
        };
        let public_request_id = Uuid::now_v7();
        let normalized = self.normalized_interaction(&interaction, public_request_id)?;
        self.journal
            .append_and_sync(JournalEntry::ApprovalRequested {
                interaction: serde_json::json!({
                    "interaction": normalized,
                    "upstream_request_id": request_id,
                }),
            })?;
        self.journal
            .append_and_sync(JournalEntry::InteractionOpened {
                request_id,
                method: method.to_owned(),
            })?;
        self.pending.insert(request_id, interaction);
        self.state = CoordinatorState::WaitingInteraction;
        Ok(())
    }

    fn open_primary_tool(&mut self, request_id: u64, params: &Value) -> Result<(), TurnError> {
        let Some((session_id, contract, dispatcher)) = self.primary_tool.as_ref().map(|config| {
            (
                config.session_id,
                config.contract.clone(),
                Arc::clone(&config.dispatcher),
            )
        }) else {
            self.server
                .respond_error(request_id, -32601, "method not supported")?;
            return Ok(());
        };
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        if self.active_turn_id.as_deref() != Some(turn_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        let call_id = params
            .get("callId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or(TurnError::CorrelationMismatch)?;
        if params.get("tool").and_then(Value::as_str) != Some(contract.name()) {
            self.server
                .respond_error(request_id, -32601, "tool not registered")?;
            return Ok(());
        }
        let payload = params
            .get("arguments")
            .cloned()
            .ok_or(TurnError::CorrelationMismatch)?;
        let raw = serde_json::to_vec(&payload).map_err(|_| TurnError::CorrelationMismatch)?;
        if raw.len() > MAX_INTERACTION_PAYLOAD_BYTES {
            return Err(TurnError::CorrelationMismatch);
        }
        if let Err(error) = contract.reject_model_identity(&payload) {
            return self.respond_primary_tool(request_id, Err(error));
        }
        let payload_sha256 = sha256_hex(
            &canonicalize(
                &parse(
                    &serde_json::to_string(&payload).map_err(|_| TurnError::CorrelationMismatch)?,
                )
                .map_err(|_| TurnError::CorrelationMismatch)?,
            )
            .map_err(|_| TurnError::CorrelationMismatch)?,
        );
        if let Some(pending) = self.pending_primary_tools.get_mut(call_id) {
            if pending.payload_sha256 != payload_sha256 {
                return self.respond_primary_tool(
                    request_id,
                    Err(MachineError::new(
                        "IDEMPOTENCY_CONFLICT",
                        "tool call identity was reused with different input",
                        false,
                        serde_json::json!({"source_tool_call_id":call_id}),
                    )),
                );
            }
            pending.request_ids.push(request_id);
            return Ok(());
        }
        if self.pending_primary_tools.len() >= MAX_PENDING_PRIMARY_TOOLS {
            return self.respond_primary_tool(
                request_id,
                Err(MachineError::new(
                    "RUN_BUSY",
                    "too many Primary tool calls are already in flight",
                    true,
                    serde_json::json!({}),
                )),
            );
        }
        let idempotency_key = sha256_hex(
            format!(
                "dolgorae-primary-tool-v1\0{}\0{}\0{thread_id}\0{turn_id}\0{call_id}",
                session_id, self.run_id
            )
            .as_bytes(),
        );
        let call = PrimaryToolCall {
            request_id,
            run_generation: self.run_generation,
            context: PrimaryCallContext {
                session_id,
                source_run_id: self.run_id,
                source_turn_id: turn_id.to_owned(),
                source_tool_call_id: call_id.to_owned(),
                idempotency_key,
            },
            payload,
        };
        self.pending_primary_tools.insert(
            call_id.to_owned(),
            PendingPrimaryToolCall {
                payload_sha256,
                request_ids: vec![request_id],
            },
        );
        if let Err(error) = dispatcher.dispatch(call) {
            self.pending_primary_tools.remove(call_id);
            return self.respond_primary_tool(request_id, Err(error));
        }
        Ok(())
    }

    pub fn complete_primary_tool(
        &mut self,
        completion: PrimaryToolCompletion,
    ) -> Result<(), TurnError> {
        if completion.run_generation != self.run_generation {
            return Ok(());
        }
        let Some(pending) = self
            .pending_primary_tools
            .remove(&completion.source_tool_call_id)
        else {
            return Ok(());
        };
        if !pending.request_ids.contains(&completion.request_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        for request_id in pending.request_ids {
            self.respond_primary_tool(request_id, completion.result.clone())?;
        }
        Ok(())
    }

    fn respond_primary_tool(
        &mut self,
        request_id: u64,
        result: Result<Value, MachineError>,
    ) -> Result<(), TurnError> {
        let (success, body) = match result {
            Ok(value) => (true, value),
            Err(error) => {
                let primary_tool = self
                    .primary_tool
                    .as_ref()
                    .ok_or(TurnError::CorrelationMismatch)?;
                (false, primary_tool.contract.error_result(&error))
            }
        };
        let text = serde_json::to_string(&body).map_err(|_| TurnError::CorrelationMismatch)?;
        self.server.respond_result(
            request_id,
            serde_json::json!({
                "success":success,
                "contentItems":[{"type":"inputText","text":text}],
            }),
        )
    }

    fn normalized_interaction(
        &mut self,
        interaction: &Interaction,
        public_request_id: Uuid,
    ) -> Result<Value, TurnError> {
        let (kind, payload, available_decisions, response_schema) =
            match interaction.method.as_str() {
                "item/commandExecution/requestApproval" => (
                    "command_execution_approval",
                    serde_json::to_value(CommandApprovalPayload {
                        title: interaction
                            .payload
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("Command approval")
                            .to_owned(),
                        message: interaction
                            .payload
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Command execution requires approval")
                            .to_owned(),
                        command: serde_json::from_value(
                            interaction
                                .payload
                                .get("command")
                                .cloned()
                                .ok_or(TurnError::CorrelationMismatch)?,
                        )
                        .map_err(|_| TurnError::CorrelationMismatch)?,
                        cwd: interaction
                            .payload
                            .get("cwd")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(|_| TurnError::CorrelationMismatch)?
                            .unwrap_or_else(|| LosslessPath::from_path(&self.cwd)),
                        reason: serde_json::from_value(
                            interaction
                                .payload
                                .get("reason")
                                .cloned()
                                .unwrap_or(Value::Null),
                        )
                        .map_err(|_| TurnError::CorrelationMismatch)?,
                    })
                    .map_err(|_| TurnError::CorrelationMismatch)?,
                    serde_json::json!(["accept_once", "decline", "cancel"]),
                    "dolgorae.interaction.command-approval/v1",
                ),
                "item/fileChange/requestApproval" => {
                    let item_id = interaction
                        .payload
                        .get("itemId")
                        .and_then(Value::as_str)
                        .ok_or(TurnError::CorrelationMismatch)?;
                    let key = (
                        interaction.thread_id.clone(),
                        interaction.turn_id.clone(),
                        item_id.to_owned(),
                    );
                    let snapshot = self
                        .file_change_snapshots
                        .get(&key)
                        .cloned()
                        .ok_or(TurnError::CorrelationMismatch)?;
                    let inline_bytes = snapshot
                        .changes
                        .iter()
                        .map(|change| change.diff.len())
                        .sum::<usize>();
                    let (changes, artifact) = if inline_bytes <= 64 * 1024 {
                        (Some(snapshot.changes), None)
                    } else {
                        let combined = snapshot
                            .changes
                            .iter()
                            .map(|change| change.diff.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        if combined.len() > 8 * 1024 * 1024 {
                            return Err(TurnError::CorrelationMismatch);
                        }
                        let stored = self.artifacts.store(combined.as_bytes())?;
                        (
                            None,
                            Some(ChangeArtifactPayload {
                                artifact_id: stored.artifact_id,
                                sha256: sha256_hex(combined.as_bytes()),
                                media_type: "text/x-diff".to_owned(),
                                byte_length: combined.len() as u64,
                                truncated: false,
                            }),
                        )
                    };
                    (
                        "file_change_approval",
                        serde_json::to_value(FileApprovalPayload {
                            title: "File change approval".to_owned(),
                            message: "File changes require approval".to_owned(),
                            reason: serde_json::from_value(
                                interaction
                                    .payload
                                    .get("reason")
                                    .cloned()
                                    .unwrap_or(Value::Null),
                            )
                            .map_err(|_| TurnError::CorrelationMismatch)?,
                            snapshot_sha256: snapshot.snapshot_sha256,
                            snapshot_revision: snapshot.revision,
                            truncated: false,
                            changes,
                            change_artifact: artifact,
                        })
                        .map_err(|_| TurnError::CorrelationMismatch)?,
                        serde_json::json!(["accept_once", "decline", "cancel"]),
                        "dolgorae.interaction.file-change-approval/v1",
                    )
                }
                "item/tool/requestUserInput" => (
                    "user_input",
                    normalize_user_input_payload(&interaction.payload, interaction.request_id)?,
                    serde_json::json!([]),
                    "dolgorae.interaction.user-input/v1",
                ),
                _ => return Err(TurnError::CorrelationMismatch),
            };
        Ok(serde_json::json!({
            "schema_version": 1,
            "request_id": public_request_id,
            "run_id": self.run_id,
            "controller_id": self.controller_id,
            "control_mode": self.control_mode,
            "thread_id": interaction.thread_id,
            "turn_id": interaction.turn_id,
            "item_id": interaction.payload.get("itemId").cloned().unwrap_or(Value::Null),
            "run_generation": self.run_generation,
            "server_epoch": self.server_epoch,
            "kind": kind,
            "status": "pending",
            "payload": payload,
            "available_decisions": available_decisions,
            "response_schema": response_schema,
            "opened_at": SystemLedgerClock::default().timestamp(),
            "resolved_at": Value::Null,
            "resolution": Value::Null,
        }))
    }

    /// Record a request that named a Thread this Run does not own, and answer
    /// nothing.
    ///
    /// Two properties are load-bearing (SPEC-004: "A foreign-thread object
    /// cannot mutate run state; it records bounded profile diagnostic
    /// metadata"):
    ///
    /// * The observation is durable before it is remembered in memory. A
    ///   worker that crashes still leaves the evidence that its connection was
    ///   handed another Thread's decision. docs/specs/README.md closes the v1 Run
    ///   audit-kind enum and states that a foreign-thread diagnostic "is never
    ///   a Run event and uses the separate profile diagnostic schema", so the
    ///   record goes to the Runtime Profile's diagnostic journal and never to
    ///   this Run's ledger.
    /// * Nothing is written back. A JSON-RPC error reply is an effect on the
    ///   *other* Thread's pending request, which this connection has no
    ///   authority over. The foreign caller's own connection owns that outcome.
    ///
    /// Only bounded routing metadata is recorded: request id, method, and the
    /// Thread/Turn the request named. The request payload is never read, never
    /// hashed, and never stored.
    /// * Neither the bound nor a failed diagnostic write is ever this Run's
    ///   outcome. Both are properties of the *other* Thread's traffic and of
    ///   the profile's diagnostic journal; surrendering this Run's Turn to
    ///   either would be exactly the mutation SPEC-004 forbids, and would let
    ///   a foreign Thread quarantine a Run it has no authority over. Past the
    ///   bound nothing more is recorded and the drain keeps going; a
    ///   diagnostics journal that cannot be written is profile degradation,
    ///   counted here so it is observable, and the request is still dropped
    ///   unanswered.
    fn ignore_foreign_request(
        &mut self,
        request_id: u64,
        method: &str,
        thread_id: Option<&str>,
        turn_id: Option<&str>,
    ) {
        if self.ignored_foreign.len() >= MAX_IGNORED_FOREIGN_REQUESTS {
            // Sustained foreign traffic is noise this connection stops
            // recording rather than an outcome it surrenders.  Nothing is
            // written past the bound, so a foreign Thread cannot drive the
            // journal's size either.
            self.undiagnosed_foreign = self.undiagnosed_foreign.saturating_add(1);
            return;
        }
        let observation = IgnoredForeignRequest {
            request_id,
            method: bounded_diagnostic_text(method),
            thread_id: thread_id.map(bounded_diagnostic_text),
            turn_id: turn_id.map(bounded_diagnostic_text),
        };
        if self
            .foreign_diagnostics
            .record(
                &observation,
                ForeignLane {
                    run_generation: self.run_generation,
                    server_key: &self.server_key,
                    server_epoch: self.server_epoch,
                },
            )
            .is_err()
        {
            self.undiagnosed_foreign = self.undiagnosed_foreign.saturating_add(1);
            return;
        }
        self.ignored_foreign.push(observation);
    }

    /// Existing interaction decisions are read before admitting a new mutation.
    pub(crate) fn replay_response(
        &mut self,
        request_id: u64,
        idempotency_key: &str,
        response: &Value,
    ) -> Option<Result<Option<Uuid>, TurnError>> {
        self.resolved_interactions
            .contains_key(&request_id)
            .then(|| self.respond(request_id, idempotency_key.to_owned(), response.clone()))
    }

    pub fn respond(
        &mut self,
        request_id: u64,
        idempotency_key: String,
        response: Value,
    ) -> Result<Option<Uuid>, TurnError> {
        let response = ProtectedResponse(response);
        if idempotency_key.is_empty() || idempotency_key.len() > 256 {
            return Err(TurnError::InteractionResponseInvalid { request_id });
        }
        let raw = Zeroizing::new(
            serde_json::to_vec(&response.0)
                .map_err(|_| TurnError::InteractionResponseInvalid { request_id })?,
        );
        if raw.len() > MAX_INTERACTION_PAYLOAD_BYTES {
            return Err(TurnError::InteractionResponseInvalid { request_id });
        }
        if let Some(resolved) = self.resolved_interactions.get(&request_id) {
            if resolved.stale {
                return Err(TurnError::InteractionStale { request_id });
            }
            if resolved.idempotency_key != idempotency_key {
                return Err(TurnError::InteractionAlreadyResolved { request_id });
            }
            if let Some(expected) = &resolved.response_sha256 {
                let actual = interaction_response_digest(&response.0, request_id)?;
                if &actual != expected {
                    return Err(TurnError::InteractionAlreadyResolved { request_id });
                }
            }
            return resolved
                .resolution_receipt_id
                .map(Some)
                .ok_or(TurnError::InteractionAlreadyResolved { request_id });
        }
        if self.state == CoordinatorState::OutcomeUnknown {
            return Err(TurnError::OutcomeUnknown);
        }
        let interaction = self
            .pending
            .get(&request_id)
            .cloned()
            .ok_or(TurnError::InteractionNotFound { request_id })?;
        let (mut wire_response, contains_secret) =
            validate_interaction_response(&interaction, &response.0)?;
        let response_sha256 = (!contains_secret)
            .then(|| interaction_response_digest(&response.0, request_id))
            .transpose()?;
        let resolution_receipt_id = Some(Uuid::now_v7());
        let resolution = if interaction.method == "item/tool/requestUserInput" {
            let answer_count = response
                .0
                .get("answers")
                .and_then(Value::as_object)
                .map_or(0, serde_json::Map::len);
            serde_json::json!({
                "outcome": "answered",
                "contained_secret": contains_secret,
                "answer_digest": response_sha256,
                "answer_count": answer_count,
                "resolution_receipt_id": resolution_receipt_id,
            })
        } else {
            serde_json::json!({
                "outcome": "approval",
                "decision": response.0.get("decision").cloned().unwrap_or(Value::Null),
            })
        };
        self.journal
            .append_and_sync(JournalEntry::ApprovalDecided {
                request_id,
                idempotency_key: idempotency_key.clone(),
                response_sha256: response_sha256.clone(),
                resolution_receipt_id,
                resolved_at: SystemLedgerClock::default().timestamp(),
                resolution,
            })?;
        self.resolved_interactions.insert(
            request_id,
            InteractionResolution {
                idempotency_key,
                response_sha256,
                resolution_receipt_id,
                stale: false,
            },
        );
        self.journal
            .append_and_sync(JournalEntry::InteractionResolved { request_id })?;
        self.pending.remove(&request_id);
        let delivered = if contains_secret {
            self.server
                .respond_sensitive_result(request_id, &mut wire_response)
        } else {
            self.server.respond_result(request_id, wire_response)
        };
        if let Err(error) = delivered {
            let Some(operation_id) = self.active_operation_id else {
                return Err(error);
            };
            return Err(self.lost_after_write(operation_id, error));
        }
        self.pending.remove(&request_id);
        self.state = if self.pending.is_empty() {
            CoordinatorState::Running
        } else {
            CoordinatorState::WaitingInteraction
        };
        Ok(resolution_receipt_id)
    }

    /// Ask the app-server to interrupt the live Turn.
    ///
    /// `turn/interrupt` is an external write like any other, so losing its
    /// answer leaves the Turn's outcome uncertain rather than merely unsent:
    /// the app-server may have interrupted it, may have let it run on, and
    /// this Run can no longer tell.  docs/specs/README.md forbids reporting that as a
    /// retryable transport hiccup, so the loss quarantines the Run durably and
    /// is restated as `outcome_unknown`.  A refusal the app-server correlated
    /// to the interrupt is definitive and does neither.
    pub fn interrupt(&mut self) -> Result<(), TurnError> {
        let thread_id = self.thread_id.clone().ok_or(TurnError::TurnBusy)?;
        let turn_id = self.active_turn_id.clone().ok_or(TurnError::TurnBusy)?;
        if let Err(error) = self.server.request(
            "turn/interrupt",
            serde_json::json!({"threadId":thread_id,"turnId":turn_id}),
        ) {
            let Some(operation_id) = self.active_operation_id else {
                return Err(error);
            };
            return Err(self.lost_after_write(operation_id, error));
        }
        Ok(())
    }

    fn capture_completed_item(&mut self, params: &Value) -> Result<(), TurnError> {
        if self.completed_item_count >= MAX_TURN_ITEMS {
            return Err(TurnError::CorrelationMismatch);
        }
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        if self.thread_id.as_deref() != Some(thread_id) {
            return Ok(());
        }
        // Foreign-thread traffic is never evidence about this Run.  Check
        // ownership before requiring any other field so even a malformed
        // notification for another Thread cannot quarantine our live Turn.
        let turn_id = event_turn_id(params).ok_or(TurnError::CorrelationMismatch)?;
        if self.active_turn_id.as_deref() != Some(turn_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        params.get("item").ok_or(TurnError::CorrelationMismatch)?;
        self.completed_item_count += 1;
        Ok(())
    }

    fn complete_turn(&mut self, params: &Value) -> Result<TerminalTurn, TurnError> {
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?
            .to_owned();
        let turn = params.get("turn").ok_or(TurnError::CorrelationMismatch)?;
        let turn_id = turn
            .get("id")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?
            .to_owned();
        let status = turn
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| matches!(*status, "completed" | "interrupted" | "failed"))
            .ok_or(TurnError::CorrelationMismatch)?
            .to_owned();
        if self.terminal_turn_ids.contains(&turn_id) {
            return Err(TurnError::DuplicateTerminal);
        }
        if self.thread_id.as_deref() != Some(&thread_id)
            || self.active_turn_id.as_deref() != Some(&turn_id)
        {
            self.state = CoordinatorState::OutcomeUnknown;
            return Err(TurnError::CorrelationMismatch);
        }
        let effort = self
            .active_turn_effort
            .clone()
            .unwrap_or_else(|| self.default_effort.clone());
        let usage = turn.get("usage").cloned();
        // The pinned protocol carries the turn's authoritative items with
        // `turn/completed`, so the response is already known here and travels
        // into the durable record.  A server that omitted them forces a
        // `thread/read`, and that network round trip must never stand between
        // an observed terminal and the record proving it: the append happens
        // at the same point either way, and the degraded path records the
        // terminal without a response rather than risking losing both.
        let inline_items = turn.get("items").and_then(Value::as_array).cloned();
        let inline_response = inline_items
            .as_ref()
            .map(|items| self.extract_final_response(&thread_id, &turn_id, items));
        for request_id in self.pending.keys().copied().collect::<Vec<_>>() {
            self.journal
                .append_and_sync(JournalEntry::ApprovalDecided {
                    request_id,
                    idempotency_key: String::new(),
                    response_sha256: None,
                    resolution_receipt_id: None,
                    resolved_at: SystemLedgerClock::default().timestamp(),
                    resolution: serde_json::json!({"outcome":"stale", "reason":"terminal_turn"}),
                })?;
            self.journal
                .append_and_sync(JournalEntry::InteractionResolved { request_id })?;
            self.resolved_interactions.insert(
                request_id,
                InteractionResolution {
                    idempotency_key: String::new(),
                    response_sha256: None,
                    resolution_receipt_id: None,
                    stale: true,
                },
            );
            self.pending.remove(&request_id);
        }
        self.journal.append_and_sync(JournalEntry::Terminal {
            turn_id: turn_id.clone(),
            status: status.clone(),
            thread_id: thread_id.clone(),
            effort: effort.clone(),
            usage: usage.clone(),
            final_response: match &inline_response {
                Some(Ok(response)) => response.clone(),
                _ => None,
            },
        })?;
        self.terminal_turn_ids.insert(turn_id.clone());
        self.active_turn_id = None;
        self.active_turn_effort = None;
        self.active_operation_id = None;
        self.pending.clear();
        self.state = CoordinatorState::Idle;
        let final_response = match inline_response {
            Some(response) => response?,
            None => {
                let items = self.streamed_items(&thread_id, &turn_id)?;
                self.extract_final_response(&thread_id, &turn_id, &items)?
            }
        };
        let terminal = TerminalTurn {
            thread_id,
            turn_id,
            status,
            effort,
            final_response,
            usage,
        };
        for record in self.idempotency.values_mut() {
            if record.accepted.turn_id == terminal.turn_id {
                record.terminal = Some(terminal.clone());
            }
        }
        Ok(terminal)
    }

    /// The Turn's authoritative items, read from the Thread's own history.
    ///
    /// Only reached when `turn/completed` omitted them: the notification's
    /// items are authoritative when present, and reading history again would
    /// cost a round trip for an answer already in hand.
    fn streamed_items(&mut self, thread_id: &str, turn_id: &str) -> Result<Vec<Value>, TurnError> {
        let mut sink = ThreadTurnSink::new(turn_id);
        self.server.request_streamed(
            "thread/read",
            serde_json::json!({"threadId":thread_id,"includeTurns":true}),
            &mut sink,
        )?;
        self.streamed_history_bytes = sink.streamed_bytes();
        sink.into_items()
    }

    fn extract_final_response(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        authoritative_items: &[Value],
    ) -> Result<Option<FinalResponse>, TurnError> {
        let mut fallback = None;
        let mut final_answer = None;
        for item in authoritative_items {
            if item.get("type").and_then(Value::as_str) != Some("agentMessage")
                || item
                    .get("threadId")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value != thread_id)
                || item
                    .get("turnId")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value != turn_id)
                || item
                    .get("parentThreadId")
                    .is_some_and(|value| !value.is_null())
                || item
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| status != "completed")
            {
                continue;
            }
            let Some(text) = item.get("text").and_then(Value::as_str) else {
                continue;
            };
            match item.get("phase") {
                Some(Value::String(phase)) if phase == "final_answer" => {
                    final_answer = Some(text.to_owned())
                }
                Some(Value::Null) | None => fallback = Some(text.to_owned()),
                _ => {}
            }
        }
        let Some(text) = final_answer.or(fallback) else {
            return Ok(None);
        };
        if text.len() <= MAX_INLINE_FINAL_RESPONSE_BYTES {
            return Ok(Some(FinalResponse::Inline { text }));
        }
        let bytes = text.as_bytes();
        let sha256 = sha256_hex(bytes);
        let byte_length = u64::try_from(bytes.len()).expect("response bound fits u64");
        if bytes.len() > MAX_FINAL_RESPONSE_ARTIFACT_BYTES {
            return Ok(Some(FinalResponse::Unavailable {
                byte_length,
                sha256,
                reason: "too_large".to_owned(),
            }));
        }
        match self.artifacts.store(bytes) {
            Ok(stored) => Ok(Some(FinalResponse::Artifact {
                artifact_id: stored.artifact_id,
                byte_length,
                sha256,
                created_at: stored.created_at,
            })),
            Err(_) => Ok(Some(FinalResponse::Unavailable {
                byte_length,
                sha256,
                reason: "write_failed".to_owned(),
            })),
        }
    }
}

fn same_file_snapshot(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.size() == after.size()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[must_use]
pub fn forkable_status(status: &str) -> bool {
    status == "completed"
}

/// The terminal one named Turn durably reached, read from the Run's ledger.
///
/// A live Run remembers only its most recent terminals, because a worker's
/// control state is bounded process memory.  The `turn_terminal` record is the
/// durable authority for the same fact and is kept for the life of the Run, so
/// a caller asking about a Turn that has fallen out of that memory is answered
/// from the ledger rather than told the Run never had it.
///
/// A ledger that cannot be read answers nothing rather than guessing: the
/// caller's own refusal is a better answer than an invented terminal.
#[must_use]
pub fn recorded_terminal<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &Ledger<C, F>,
    turn_id: &str,
) -> Option<TerminalTurn> {
    ledger
        .durable_records()
        .ok()?
        .iter()
        .rev()
        .filter(|record| record.kind() == AuditKind::TurnTerminal)
        .find_map(|record| {
            let bytes = canonicalize(record.payload()).ok()?;
            let terminal: TerminalTurn = serde_json::from_slice(&bytes).ok()?;
            (terminal.turn_id == turn_id).then_some(terminal)
        })
}

struct RestoredTurnReceipts {
    accepted: BTreeMap<String, IdempotencyRecord>,
    unresolved: BTreeMap<String, String>,
}

fn restore_turn_receipt_maps(
    records: Vec<(AuditKind, Value)>,
) -> Result<RestoredTurnReceipts, TurnError> {
    let mut idempotency = BTreeMap::new();
    let mut unresolved_idempotency = BTreeMap::new();
    let mut bound_threads = BTreeSet::new();
    let mut intents = BTreeMap::<String, OperationIntent>::new();
    let mut accepted = BTreeMap::<String, (String, String)>::new();
    for (kind, payload) in records {
        match kind {
            AuditKind::IdempotencyReserved if payload.get("operation_id").is_some() => {
                let intent: OperationIntent = serde_json::from_value(payload)
                    .map_err(|_| TurnError::Journal("invalid durable Turn intent".to_owned()))?;
                unresolved_idempotency.insert(
                    intent.idempotency_key.clone(),
                    intent.request_sha256.clone(),
                );
                intents.insert(intent.operation_id.to_string(), intent);
            }
            AuditKind::ThreadBound => {
                if let Some(thread_id) = payload.get("thread_id").and_then(Value::as_str) {
                    bound_threads.insert(thread_id.to_owned());
                }
            }
            AuditKind::TurnStarted => {
                let field = |name| {
                    payload.get(name).and_then(Value::as_str).ok_or_else(|| {
                        TurnError::Journal("invalid durable Turn acceptance".to_owned())
                    })
                };
                let Some(operation_id) = payload.get("operation_id").and_then(Value::as_str) else {
                    continue;
                };
                if let Some(intent) = intents.get(operation_id) {
                    let thread_id = field("thread_id")?;
                    if !bound_threads.contains(thread_id) {
                        continue;
                    }
                    if let Some(effort) = payload
                        .get("effort")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        unresolved_idempotency.remove(&intent.idempotency_key);
                        idempotency.insert(
                            intent.idempotency_key.clone(),
                            IdempotencyRecord {
                                digest: intent.request_sha256.clone(),
                                accepted: AcceptedTurn {
                                    thread_id: thread_id.to_owned(),
                                    turn_id: field("turn_id")?.to_owned(),
                                    effort: effort.to_owned(),
                                    replayed: true,
                                },
                                terminal: None,
                            },
                        );
                    }
                    accepted.insert(
                        field("turn_id")?.to_owned(),
                        (operation_id.to_owned(), field("thread_id")?.to_owned()),
                    );
                }
            }
            AuditKind::TurnTerminal => {
                let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some((operation_id, thread_id)) = accepted.get(turn_id) else {
                    continue;
                };
                let terminal: TerminalTurn = serde_json::from_value(payload)
                    .map_err(|_| TurnError::Journal("invalid durable Turn terminal".to_owned()))?;
                if terminal.thread_id != *thread_id {
                    return Err(TurnError::CorrelationMismatch);
                }
                let intent = &intents[operation_id];
                unresolved_idempotency.remove(&intent.idempotency_key);
                idempotency.insert(
                    intent.idempotency_key.clone(),
                    IdempotencyRecord {
                        digest: intent.request_sha256.clone(),
                        accepted: AcceptedTurn {
                            thread_id: terminal.thread_id.clone(),
                            turn_id: terminal.turn_id.clone(),
                            effort: terminal.effort.clone(),
                            replayed: true,
                        },
                        terminal: Some(terminal),
                    },
                );
            }
            _ => {}
        }
    }
    Ok(RestoredTurnReceipts {
        accepted: idempotency,
        unresolved: unresolved_idempotency,
    })
}

/// Recover an original Submit result without constructing or starting a worker.
/// Callers authorize before this read and pass the pinned model/default effort.
pub(crate) fn replay_durable_submit(
    records: Vec<(AuditKind, Value)>,
    request: &TurnRequest,
    default_effort: &str,
) -> Result<Option<AcceptedTurn>, TurnError> {
    let RestoredTurnReceipts {
        accepted,
        unresolved,
    } = restore_turn_receipt_maps(records)?;
    let effort = request.effort.as_deref().unwrap_or(default_effort);
    let digest = request_digest(request, effort)?;
    if let Some(recorded) = unresolved.get(&request.idempotency_key) {
        return Err(if recorded == &digest {
            TurnError::OutcomeUnknown
        } else {
            TurnError::IdempotencyConflict {
                recorded: recorded.clone(),
                observed: digest,
            }
        });
    }
    let Some(record) = accepted.get(&request.idempotency_key) else {
        return Ok(None);
    };
    if record.digest != digest {
        return Err(TurnError::IdempotencyConflict {
            recorded: record.digest.clone(),
            observed: digest,
        });
    }
    let mut accepted = record.accepted.clone();
    accepted.replayed = true;
    Ok(Some(accepted))
}

pub(crate) fn normalized_request_digest(
    request: &TurnRequest,
    default_effort: &str,
) -> Result<String, TurnError> {
    request_digest(request, request.effort.as_deref().unwrap_or(default_effort))
}

fn request_digest(request: &TurnRequest, effort: &str) -> Result<String, TurnError> {
    let value = serde_json::json!({
        "message": request.message,
        "write": request.write,
        "images": request.images,
        "model": request.model,
        "effort": effort,
    });
    let serialized = serde_json::to_string(&value)
        .map_err(|_| TurnError::InvalidInput("request is not serializable"))?;
    let parsed = parse(&serialized)
        .map_err(|_| TurnError::InvalidInput("request is not canonicalizable"))?;
    let canonical = canonicalize(&parsed)
        .map_err(|_| TurnError::InvalidInput("request is not canonicalizable"))?;
    Ok(sha256_hex(&canonical))
}

pub(crate) fn zeroize_protected_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_protected_json),
        Value::Object(values) => {
            for (_, value) in values.iter_mut() {
                zeroize_protected_json(value);
            }
            values.clear();
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    *value = Value::Null;
}

struct ProtectedResponse(Value);

impl Drop for ProtectedResponse {
    fn drop(&mut self) {
        zeroize_protected_json(&mut self.0);
    }
}

fn interaction_response_digest(response: &Value, request_id: u64) -> Result<String, TurnError> {
    let serialized = serde_json::to_string(response)
        .map_err(|_| TurnError::InteractionResponseInvalid { request_id })?;
    let parsed =
        parse(&serialized).map_err(|_| TurnError::InteractionResponseInvalid { request_id })?;
    let canonical =
        canonicalize(&parsed).map_err(|_| TurnError::InteractionResponseInvalid { request_id })?;
    Ok(sha256_hex(&canonical))
}

fn validate_interaction_response(
    interaction: &Interaction,
    response: &Value,
) -> Result<(Value, bool), TurnError> {
    let invalid = || TurnError::InteractionResponseInvalid {
        request_id: interaction.request_id,
    };
    let object = response.as_object().ok_or_else(invalid)?;
    match interaction.method.as_str() {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            if object.len() != 1 {
                return Err(invalid());
            }
            let decision = object
                .get("decision")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            let wire = match decision {
                "accept_once" => "accept",
                "decline" => "decline",
                "cancel" => "cancel",
                _ => return Err(invalid()),
            };
            Ok((serde_json::json!({"decision": wire}), false))
        }
        "item/tool/requestUserInput" => {
            if object.len() != 1 {
                return Err(invalid());
            }
            let answers = object
                .get("answers")
                .and_then(Value::as_object)
                .ok_or_else(invalid)?;
            if answers.is_empty() {
                return Err(invalid());
            }
            let questions = interaction
                .payload
                .get("questions")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            let mut contains_secret = false;
            let mut known = BTreeMap::new();
            for question in questions {
                let id = question
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                contains_secret |= question
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                known.insert(id, question);
            }
            if answers.len() != known.len()
                || answers.keys().any(|id| !known.contains_key(id.as_str()))
            {
                return Err(invalid());
            }
            for (question_id, answer) in answers {
                let values = answer
                    .get("answers")
                    .and_then(Value::as_array)
                    .filter(|values| !values.is_empty())
                    .ok_or_else(invalid)?;
                if answer.as_object().is_none_or(|object| object.len() != 1)
                    || values.iter().any(|value| {
                        value
                            .as_str()
                            .is_none_or(|text| text.is_empty() || text.len() > 4096)
                    })
                {
                    return Err(invalid());
                }
                let question = known.get(question_id.as_str()).ok_or_else(invalid)?;
                if let Some(options) = question.get("options").and_then(Value::as_array)
                    && !options.is_empty()
                    && !question
                        .get("isOther")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    let allowed = options
                        .iter()
                        .filter_map(|option| {
                            option.as_str().or_else(|| {
                                option
                                    .get("label")
                                    .or_else(|| option.get("value"))
                                    .or_else(|| option.get("name"))
                                    .and_then(Value::as_str)
                            })
                        })
                        .collect::<BTreeSet<_>>();
                    if allowed.len() != options.len()
                        || values.iter().any(|value| {
                            value.as_str().is_none_or(|value| !allowed.contains(value))
                        })
                    {
                        return Err(invalid());
                    }
                }
            }
            Ok((response.clone(), contains_secret))
        }
        _ => Err(invalid()),
    }
}

fn normalize_user_input_payload(params: &Value, request_id: u64) -> Result<Value, TurnError> {
    let invalid = || TurnError::InteractionResponseInvalid { request_id };
    let questions = params
        .get("questions")
        .and_then(Value::as_array)
        .filter(|questions| (1..=3).contains(&questions.len()))
        .ok_or_else(invalid)?;
    let normalized = questions
        .iter()
        .map(|question| {
            let id = question
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            if id.is_empty() || id.len() > 256 {
                return Err(invalid());
            }
            let options = question.get("options").map_or(Ok(Value::Null), |options| {
                let options = options.as_array().ok_or_else(invalid)?;
                if options.len() > 32 {
                    return Err(invalid());
                }
                Ok(Value::Array(options.clone()))
            })?;
            Ok(serde_json::json!({
                "id": id,
                "header": question.get("header").and_then(Value::as_str).unwrap_or(""),
                "question": question.get("question").and_then(Value::as_str).unwrap_or(""),
                "is_other": question.get("isOther").and_then(Value::as_bool).unwrap_or(false),
                "is_secret": question.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
                "options": options,
            }))
        })
        .collect::<Result<Vec<_>, TurnError>>()?;
    Ok(serde_json::json!({
        "is_blocking": params.get("isBlocking").and_then(Value::as_bool).unwrap_or(true),
        "questions": normalized,
    }))
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, TurnError> {
    let serialized = serde_json::to_string(value).map_err(|_| TurnError::CorrelationMismatch)?;
    let parsed = parse(&serialized).map_err(|_| TurnError::CorrelationMismatch)?;
    canonicalize(&parsed).map_err(|_| TurnError::CorrelationMismatch)
}

fn normalize_file_changes(
    changes: &[Value],
    workspace: &Path,
) -> Result<Vec<FileChangePayload>, TurnError> {
    if changes.is_empty() || changes.len() > 4_096 || !workspace.is_absolute() {
        return Err(TurnError::CorrelationMismatch);
    }
    let mut total_diff = 0usize;
    changes
        .iter()
        .map(|change| {
            let object = change.as_object().ok_or(TurnError::CorrelationMismatch)?;
            let path = checked_change_path(
                object
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or(TurnError::CorrelationMismatch)?,
            )?;
            let kind_object = object
                .get("kind")
                .and_then(Value::as_object)
                .ok_or(TurnError::CorrelationMismatch)?;
            let kind = kind_object
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| matches!(*kind, "add" | "update" | "delete"))
                .ok_or(TurnError::CorrelationMismatch)?;
            let diff = object
                .get("diff")
                .and_then(Value::as_str)
                .ok_or(TurnError::CorrelationMismatch)?;
            if diff.len() > 64 * 1024 {
                return Err(TurnError::CorrelationMismatch);
            }
            total_diff = total_diff
                .checked_add(diff.len())
                .ok_or(TurnError::CorrelationMismatch)?;
            if total_diff > 8 * 1024 * 1024 {
                return Err(TurnError::CorrelationMismatch);
            }
            let move_path = kind_object
                .get("move_path")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_str()
                        .ok_or(TurnError::CorrelationMismatch)
                        .and_then(checked_change_path)
                })
                .transpose()?;
            if kind != "update" && move_path.is_some() {
                return Err(TurnError::CorrelationMismatch);
            }
            Ok(FileChangePayload {
                path: LosslessPath::Utf8(path),
                kind: kind.to_owned(),
                diff: diff.to_owned(),
                move_path: move_path.map(LosslessPath::Utf8),
            })
        })
        .collect()
}

fn checked_change_path(path: &str) -> Result<String, TurnError> {
    use std::path::Component;
    if path.is_empty()
        || path.len() > 4_096
        || Path::new(path).is_absolute()
        || Path::new(path).components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(TurnError::CorrelationMismatch);
    }
    Ok(path.to_owned())
}

fn turn_input(request: &TurnRequest) -> Result<Vec<Value>, TurnError> {
    let mut input = vec![serde_json::json!({"type":"text","text":request.message})];
    for image in &request.images {
        let path = image.verify()?;
        let path = path.to_str().ok_or(TurnError::InvalidInput(
            "Codex local image paths must be UTF-8",
        ))?;
        input.push(
            serde_json::json!({"type":"localImage","path":path,"detail":image.detail.as_str()}),
        );
    }
    Ok(input)
}

fn image_media_type(path: &LosslessPath) -> Result<String, TurnError> {
    let path = path
        .to_path_buf()
        .map_err(|_| TurnError::InvalidInput("image path encoding is invalid"))?;
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("png") => Ok("image/png".to_owned()),
        Some("jpg" | "jpeg") => Ok("image/jpeg".to_owned()),
        Some("gif") => Ok("image/gif".to_owned()),
        Some("webp") => Ok("image/webp".to_owned()),
        _ => Err(TurnError::InvalidInput("image media type is unsupported")),
    }
}

fn turn_sandbox(sandbox: &str, cwd: &Path) -> Value {
    if sandbox == "read-only" {
        // SPEC-002 pins the reader's turn policy whole: a `readOnly` sandbox
        // that omits `networkAccess` leaves the one property that separates a
        // reader from an unconstrained turn to the server's default.
        serde_json::json!({"type":"readOnly","networkAccess":false})
    } else {
        serde_json::json!({"type":"workspaceWrite","writableRoots":[cwd],"networkAccess":false,"excludeSlashTmp":false,"excludeTmpdirEnvVar":false})
    }
}

fn identity(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(str::to_owned)
}
fn nested_identity(value: &Value, object: &str, field: &str) -> Option<String> {
    value.get(object).and_then(|value| identity(value, field))
}
fn event_turn_id(params: &Value) -> Option<&str> {
    params.get("turnId").and_then(Value::as_str).or_else(|| {
        params
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primary_tool::PRIMARY_TOOL_NAME;
    use std::collections::VecDeque;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    #[derive(Default)]
    struct RecordingPrimaryDispatcher {
        calls: Mutex<Vec<PrimaryToolCall>>,
    }

    impl PrimaryToolDispatcher for RecordingPrimaryDispatcher {
        fn dispatch(&self, call: PrimaryToolCall) -> Result<(), MachineError> {
            self.calls.lock().unwrap().push(call);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeServer {
        replies: VecDeque<Result<Value, TurnError>>,
        messages: VecDeque<Value>,
        calls: Vec<(String, Value)>,
        responses: Vec<(u64, Value)>,
        fail_response: bool,
    }
    impl AppServer for FakeServer {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TurnError> {
            self.calls.push((method.to_owned(), params));
            self.replies.pop_front().unwrap_or(Err(TurnError::transport(
                TransportStage::Read,
                "missing fake reply",
            )))
        }
        fn notify(&mut self, method: &str, params: Value) -> Result<(), TurnError> {
            self.calls.push((method.to_owned(), params));
            Ok(())
        }
        fn next_message(&mut self) -> Result<Value, TurnError> {
            self.messages
                .pop_front()
                .ok_or_else(|| TurnError::transport(TransportStage::Read, "missing fake message"))
        }
        fn respond_result(&mut self, id: u64, result: Value) -> Result<(), TurnError> {
            self.responses.push((id, result));
            if self.fail_response {
                return Err(TurnError::transport(
                    TransportStage::Write,
                    "uncertain response write",
                ));
            }
            Ok(())
        }
        fn respond_error(&mut self, id: u64, code: i64, message: &str) -> Result<(), TurnError> {
            self.responses
                .push((id, serde_json::json!({"code":code,"message":message})));
            Ok(())
        }
    }

    /// One observation as the durable authority received it.
    #[derive(Clone, Debug, Eq, PartialEq)]
    struct RecordedObservation {
        ignored: IgnoredForeignRequest,
        run_generation: u64,
        server_key: String,
        server_epoch: u64,
    }

    /// A durable authority stand-in that records exactly what the real one
    /// would write, and can be made to fail the way a broken journal does.
    #[derive(Clone, Default)]
    struct RecordingDiagnostics {
        records: Arc<Mutex<Vec<RecordedObservation>>>,
        broken: bool,
    }

    impl RecordingDiagnostics {
        fn records(&self) -> Vec<RecordedObservation> {
            self.records.lock().unwrap().clone()
        }
    }

    impl ForeignDiagnostics for RecordingDiagnostics {
        fn record(
            &self,
            ignored: &IgnoredForeignRequest,
            lane: ForeignLane<'_>,
        ) -> Result<(), TurnError> {
            if self.broken {
                return Err(TurnError::Journal(
                    "diagnostic journal is broken".to_owned(),
                ));
            }
            self.records.lock().unwrap().push(RecordedObservation {
                ignored: ignored.clone(),
                run_generation: lane.run_generation,
                server_key: lane.server_key.to_owned(),
                server_epoch: lane.server_epoch,
            });
            Ok(())
        }
    }

    fn coordinator(
        server: FakeServer,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        coordinator_with_attach(server, ThreadAttach::Start)
    }

    fn coordinator_with_attach(
        server: FakeServer,
        attach: ThreadAttach,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        coordinator_in(
            server,
            attach,
            Box::new(RecordingDiagnostics::default()),
            SessionSafetyPolicy::Standard,
        )
    }

    fn coordinator_in(
        server: FakeServer,
        attach: ThreadAttach,
        foreign_diagnostics: Box<dyn ForeignDiagnostics>,
        safety_policy: SessionSafetyPolicy,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        coordinator_in_with_primary(server, attach, foreign_diagnostics, safety_policy, None)
    }

    fn coordinator_in_with_primary(
        mut server: FakeServer,
        attach: ThreadAttach,
        foreign_diagnostics: Box<dyn ForeignDiagnostics>,
        safety_policy: SessionSafetyPolicy,
        primary_tool: Option<PrimaryToolConfig>,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        server
            .replies
            .push_front(Ok(serde_json::json!({"codexHome":"/tmp/codex-home"})));
        TurnCoordinator::initialize(
            server,
            MemoryJournal::default(),
            MemoryArtifactStore::default(),
            CoordinatorConfig {
                attach,
                foreign_diagnostics,
                fixed_model: "gpt-5".to_owned(),
                default_effort: "medium".to_owned(),
                supported_efforts: ["low".to_owned(), "medium".to_owned(), "high".to_owned()]
                    .into_iter()
                    .collect(),
                cwd: PathBuf::from("/tmp/workspace"),
                developer_instructions: "fixed".to_owned(),
                sandbox: "read-only".to_owned(),
                approval_policy: "untrusted".to_owned(),
                safety_policy,
                run_generation: 1,
                server_key: "a".repeat(64),
                server_epoch: 7,
                run_id: Uuid::now_v7(),
                controller_id: Uuid::now_v7(),
                control_mode: "direct_interactive".to_owned(),
                record_accepted_user_input: true,
                primary_tool,
            },
            "/tmp/codex-home",
        )
        .unwrap()
    }

    fn request(key: &str, delivery: DeliveryMode) -> TurnRequest {
        TurnRequest {
            write: false,
            idempotency_key: key.to_owned(),
            message: "hello".to_owned(),
            images: vec![],
            model: "gpt-5".to_owned(),
            effort: None,
            delivery,
        }
    }

    #[test]
    fn prepare_precedes_thread_and_turn_effects_and_delivery_is_not_identity() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        let mut coordinator = coordinator(server);
        let accepted = coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        assert_eq!(accepted.turn_id, "turn-1");
        assert_eq!(
            coordinator.server.calls[0]
                .1
                .pointer("/capabilities/experimentalApi"),
            Some(&Value::Bool(false))
        );
        assert!(
            coordinator
                .server
                .calls
                .iter()
                .find(|(method, _)| method == "thread/start")
                .unwrap()
                .1
                .get("dynamicTools")
                .is_none()
        );
        assert!(matches!(
            coordinator.journal().entries[0],
            JournalEntry::Intent(_)
        ));
        assert!(matches!(
            coordinator.journal().entries[1],
            JournalEntry::ProvisionalThread { .. }
        ));
        let replay = coordinator
            .start_turn(request("key", DeliveryMode::Send))
            .unwrap();
        assert!(replay.replayed);
    }

    #[test]
    fn specialist_turn_does_not_become_accepted_human_input() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        let mut coordinator = coordinator(server);
        coordinator.record_accepted_user_input = false;
        coordinator
            .start_turn(request("specialist", DeliveryMode::Submit))
            .unwrap();
        let JournalEntry::Intent(intent) = &coordinator.journal().entries[0] else {
            panic!("first durable record is not the Turn intent");
        };
        assert!(intent.accepted_input.is_none());
    }

    #[test]
    fn primary_tool_is_run_scoped_authenticated_deduplicated_and_generation_fenced() {
        let dispatcher = Arc::new(RecordingPrimaryDispatcher::default());
        let session_id = Uuid::now_v7();
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        let mut coordinator = coordinator_in_with_primary(
            server,
            ThreadAttach::Start,
            Box::new(RecordingDiagnostics::default()),
            SessionSafetyPolicy::Standard,
            Some(PrimaryToolConfig {
                session_id,
                contract: PrimaryToolContract::load().unwrap(),
                dispatcher: dispatcher.clone(),
            }),
        );
        coordinator
            .start_turn(request("primary", DeliveryMode::Submit))
            .unwrap();

        let initialize = &coordinator.server.calls[0].1;
        assert_eq!(
            initialize.pointer("/capabilities/experimentalApi"),
            Some(&Value::Bool(true))
        );
        let thread_start = coordinator
            .server
            .calls
            .iter()
            .find(|(method, _)| method == "thread/start")
            .unwrap();
        let tools = thread_start.1["dynamicTools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], PRIMARY_TOOL_NAME);
        assert_eq!(
            tools[0]["inputSchema"]["oneOf"].as_array().unwrap().len(),
            8
        );

        coordinator
            .ingest(
                serde_json::json!({"id":40,"method":"item/tool/call","params":{
                    "threadId":"substituted-thread",
                    "turnId":"turn-1",
                    "callId":"foreign-call",
                    "tool":PRIMARY_TOOL_NAME,
                    "arguments":{"operation":"list_specialists"}
                }}),
            )
            .unwrap();
        assert!(dispatcher.calls.lock().unwrap().is_empty());
        assert!(coordinator.server.responses.is_empty());

        let params = serde_json::json!({
            "threadId":"thread-1",
            "turnId":"turn-1",
            "callId":"call-1",
            "tool":PRIMARY_TOOL_NAME,
            "arguments":{"operation":"list_specialists"},
        });
        coordinator
            .ingest(serde_json::json!({"id":41,"method":"item/tool/call","params":params}))
            .unwrap();
        coordinator
            .ingest(
                serde_json::json!({"id":42,"method":"item/tool/call","params":{
                    "threadId":"thread-1",
                    "turnId":"turn-1",
                    "callId":"call-1",
                    "tool":PRIMARY_TOOL_NAME,
                    "arguments":{"operation":"list_specialists"}
                }}),
            )
            .unwrap();
        let calls = dispatcher.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].context.session_id, session_id);
        assert_eq!(calls[0].context.source_turn_id, "turn-1");
        assert_eq!(calls[0].context.source_tool_call_id, "call-1");
        assert_eq!(
            calls[0].context.idempotency_key,
            sha256_hex(
                format!(
                    "dolgorae-primary-tool-v1\0{session_id}\0{}\0thread-1\0turn-1\0call-1",
                    coordinator.run_id
                )
                .as_bytes()
            )
        );

        coordinator
            .ingest(
                serde_json::json!({"id":44,"method":"item/tool/call","params":{
                    "threadId":"thread-1",
                    "turnId":"turn-1",
                    "callId":"call-1",
                    "tool":PRIMARY_TOOL_NAME,
                    "arguments":{"operation":"collect_specialist_results","after_sequence":0,"limit":1}
                }}),
            )
            .unwrap();
        assert_eq!(dispatcher.calls.lock().unwrap().len(), 1);
        assert_eq!(coordinator.server.responses.last().unwrap().0, 44);
        coordinator
            .ingest(
                serde_json::json!({"id":45,"method":"item/tool/call","params":{
                    "threadId":"thread-1",
                    "turnId":"turn-1",
                    "callId":"call-2",
                    "tool":PRIMARY_TOOL_NAME,
                    "arguments":{"operation":"list_specialists"}
                }}),
            )
            .unwrap();
        assert_eq!(dispatcher.calls.lock().unwrap().len(), 2);

        coordinator
            .complete_primary_tool(PrimaryToolCompletion {
                request_id: 41,
                run_generation: 2,
                source_tool_call_id: "call-1".to_owned(),
                result: Ok(serde_json::json!({"ignored":true})),
            })
            .unwrap();
        assert_eq!(coordinator.server.responses.len(), 1);
        coordinator
            .complete_primary_tool(PrimaryToolCompletion {
                request_id: 41,
                run_generation: 1,
                source_tool_call_id: "call-1".to_owned(),
                result: Ok(serde_json::json!({"operation":"list_specialists_result"})),
            })
            .unwrap();
        assert_eq!(
            coordinator
                .server
                .responses
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![44, 41, 42]
        );
        assert!(
            coordinator
                .server
                .responses
                .iter()
                .filter(|(id, _)| *id != 44)
                .all(|(_, value)| {
                    value["success"] == true
                        && !value.to_string().contains("credential")
                        && !value.to_string().contains("socket")
                        && !value.to_string().contains("database")
                })
        );
        coordinator
            .complete_primary_tool(PrimaryToolCompletion {
                request_id: 45,
                run_generation: 1,
                source_tool_call_id: "call-2".to_owned(),
                result: Ok(serde_json::json!({"operation":"list_specialists_result"})),
            })
            .unwrap();

        coordinator
            .ingest(
                serde_json::json!({"id":43,"method":"item/tool/call","params":{
                    "threadId":"thread-1",
                    "turnId":"turn-1",
                    "callId":"forged",
                    "tool":PRIMARY_TOOL_NAME,
                    "arguments":{"operation":"list_specialists","source_primary_run_id":session_id}
                }}),
            )
            .unwrap();
        assert_eq!(dispatcher.calls.lock().unwrap().len(), 2);
        assert_eq!(coordinator.server.responses.last().unwrap().0, 43);
        assert_eq!(
            coordinator.server.responses.last().unwrap().1["success"],
            false
        );
        let error: Value = serde_json::from_str(
            coordinator.server.responses.last().unwrap().1["contentItems"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(error["operation"], "orchestration_error");
        assert_eq!(error["code"], "ORCHESTRATION_NOT_AVAILABLE");
        assert!(error.get("details").is_none());
    }

    #[test]
    fn writer_transition_is_durable_and_pins_the_nonexcluding_sandbox() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-w"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-w"}})));
        let mut coordinator = coordinator(server);
        let transaction_id = Uuid::now_v7();
        coordinator
            .set_writer_access(true, 7, transaction_id)
            .unwrap();
        assert!(matches!(
            coordinator.journal().entries.last(),
            Some(JournalEntry::WriterAccessChanged {
                write: true,
                writer_generation: 7,
                transaction_id: observed,
                ..
            }) if *observed == transaction_id
        ));
        coordinator
            .start_turn(request("writer", DeliveryMode::Submit))
            .unwrap();
        let (_, params) = coordinator
            .server
            .calls
            .iter()
            .find(|(method, _)| method == "turn/start")
            .expect("writer turn request");
        assert_eq!(
            params["sandboxPolicy"],
            serde_json::json!({
                "type": "workspaceWrite",
                "writableRoots": ["/tmp/workspace"],
                "networkAccess": false,
                "excludeSlashTmp": false,
                "excludeTmpdirEnvVar": false,
            })
        );
        assert_eq!(params["approvalPolicy"], "on-request");
    }

    #[test]
    fn durable_interaction_receipt_restores_idempotency_without_replaying_upstream() {
        let mut coordinator = coordinator(FakeServer::default());
        let receipt = Uuid::now_v7();
        coordinator
            .restore_interaction_resolutions(vec![serde_json::json!({
                "request_id": "42",
                "idempotency_key": "answer-once",
                "response_sha256": null,
                "resolution_receipt_id": receipt,
                "resolution": {"outcome": "answered", "contained_secret": true}
            })])
            .unwrap();

        assert_eq!(
            coordinator.respond(
                42,
                "answer-once".to_owned(),
                serde_json::json!({"answers":{"secret":["replacement-is-not-compared"]}}),
            ),
            Ok(Some(receipt))
        );
        assert!(matches!(
            coordinator.respond(
                42,
                "different-key".to_owned(),
                serde_json::json!({"answers":{"secret":["value"]}}),
            ),
            Err(TurnError::InteractionAlreadyResolved { request_id: 42 })
        ));
        assert!(coordinator.server.responses.is_empty());
    }

    #[test]
    fn nonsecret_resolution_receipt_survives_restart_without_redelivery() {
        let answer = serde_json::json!({"answers":{"q":{"answers":["yes"]}}});
        let mut original =
            running_turn_with(&RecordingDiagnostics::default(), vec![input_request()]);
        original.next_event().unwrap();
        let receipt = original
            .respond(9, "answer".to_owned(), answer.clone())
            .unwrap()
            .expect("every accepted resolution has a receipt");
        assert_eq!(original.server.responses.len(), 1);
        let decisions = original
            .journal
            .entries
            .clone()
            .into_iter()
            .map(journal_record)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter_map(|(kind, payload)| (kind == AuditKind::ApprovalDecided).then_some(payload))
            .collect();
        let mut restarted = coordinator(FakeServer::default());
        restarted
            .restore_interaction_resolutions(decisions)
            .unwrap();
        assert_eq!(
            restarted.respond(9, "answer".to_owned(), answer),
            Ok(Some(receipt))
        );
        assert!(restarted.server.responses.is_empty());
        assert!(matches!(
            restarted.respond(
                9,
                "answer".to_owned(),
                serde_json::json!({"answers":{"q":{"answers":["no"]}}})
            ),
            Err(TurnError::InteractionAlreadyResolved { request_id: 9 })
        ));
    }

    #[test]
    fn legacy_resolution_without_a_receipt_never_redelivers_or_invents_one() {
        let answer = serde_json::json!({"decision":"decline"});
        let mut restarted = coordinator(FakeServer::default());
        restarted
            .restore_interaction_resolutions(vec![serde_json::json!({
                "request_id":"9", "idempotency_key":"answer",
                "response_sha256":interaction_response_digest(&answer, 9).unwrap(),
                "resolution_receipt_id":null, "resolution":{"outcome":"approval"}
            })])
            .unwrap();
        assert_eq!(
            restarted.respond(9, "answer".to_owned(), answer),
            Err(TurnError::InteractionAlreadyResolved { request_id: 9 })
        );
        assert!(restarted.server.responses.is_empty());
    }

    #[test]
    fn latest_durable_interaction_receipt_wins_when_an_upstream_id_is_reused() {
        let mut coordinator = coordinator(FakeServer::default());
        let latest_receipt = Uuid::now_v7();
        coordinator
            .restore_interaction_resolutions(vec![
                serde_json::json!({
                    "request_id": "7",
                    "idempotency_key": "old-generation",
                    "response_sha256": null,
                    "resolution_receipt_id": Uuid::now_v7(),
                    "resolution": {"outcome": "answered"}
                }),
                serde_json::json!({
                    "request_id": "7",
                    "idempotency_key": "new-generation",
                    "response_sha256": null,
                    "resolution_receipt_id": latest_receipt,
                    "resolution": {"outcome": "answered"}
                }),
            ])
            .unwrap();

        assert_eq!(
            coordinator.respond(7, "new-generation".to_owned(), serde_json::json!({})),
            Ok(Some(latest_receipt))
        );
        assert!(coordinator.server.responses.is_empty());
    }

    #[test]
    fn durable_acceptance_replays_without_terminal_and_without_claiming_outcome() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        let mut original = coordinator(server);
        let accepted = original
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        let records = original
            .journal
            .entries
            .clone()
            .into_iter()
            .map(journal_record)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut restored = coordinator(FakeServer::default());
        restored.restore_turn_receipts(records.clone()).unwrap();
        restored.restore_durable_state(
            RunLifecycle::OutcomeUnknown,
            Some("thread-1".to_owned()),
            Some("turn-1".to_owned()),
            None,
        );
        let before = restored.journal.entries.len();
        assert!(
            matches!(restored.accept(request("key", DeliveryMode::Submit)).unwrap(),
            DeliveryResult::Accepted(replayed) if replayed.replayed && replayed.turn_id == accepted.turn_id && replayed.effort == accepted.effort)
        );
        assert_eq!(restored.state(), &CoordinatorState::OutcomeUnknown);
        assert_eq!(restored.journal.entries.len(), before);

        let legacy = records
            .into_iter()
            .map(|(kind, mut payload)| {
                if kind == AuditKind::TurnStarted {
                    payload.as_object_mut().unwrap().remove("effort");
                }
                (kind, payload)
            })
            .collect();
        let mut legacy_restored = coordinator(FakeServer::default());
        legacy_restored.restore_turn_receipts(legacy).unwrap();
        assert!(matches!(
            legacy_restored.accept(request("key", DeliveryMode::Submit)),
            Err(TurnError::OutcomeUnknown)
        ));
    }

    #[test]
    fn completed_turn_receipt_survives_worker_restart_without_resubmission() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}}));
        let mut original = coordinator(server);
        original
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        let terminal = original.next_event().unwrap().unwrap();
        let records = original
            .journal
            .entries
            .clone()
            .into_iter()
            .map(journal_record)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut restored = coordinator(FakeServer::default());
        restored.restore_turn_receipts(records).unwrap();
        let before = restored.journal.entries.len();
        assert_eq!(
            restored.accept(request("key", DeliveryMode::Send)).unwrap(),
            DeliveryResult::Terminal(terminal)
        );
        let mut changed = request("key", DeliveryMode::Submit);
        changed.message = "changed".to_owned();
        assert!(matches!(
            restored.accept(changed),
            Err(TurnError::IdempotencyConflict { .. })
        ));
        assert_eq!(restored.journal.entries.len(), before);
    }

    #[test]
    fn restored_unaccepted_reservation_never_reexecutes_after_run_becomes_idle() {
        let mut original = coordinator(FakeServer::default());
        assert!(
            original
                .start_turn(request("lost", DeliveryMode::Submit))
                .is_err()
        );
        let records = original
            .journal
            .entries
            .clone()
            .into_iter()
            .map(journal_record)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut restored = coordinator(FakeServer::default());
        restored.restore_turn_receipts(records).unwrap();
        let before = restored.journal.entries.len();
        assert!(matches!(
            restored.accept(request("lost", DeliveryMode::Submit)),
            Err(TurnError::OutcomeUnknown)
        ));
        assert_eq!(restored.journal.entries.len(), before);
    }

    #[test]
    fn revision_admission_replay_lookup_has_no_effect_and_preserves_identity() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        let mut coordinator = coordinator(server);
        assert!(
            coordinator
                .replay_delivery(&request("key", DeliveryMode::Submit))
                .unwrap()
                .is_none()
        );
        let accepted = coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        let before = coordinator.journal.entries.len();
        assert!(
            matches!(coordinator.replay_delivery(&request("key", DeliveryMode::Send)).unwrap(),
            Some(DeliveryResult::Accepted(replayed)) if replayed.replayed && replayed.turn_id == accepted.turn_id)
        );
        let mut write_drift = request("key", DeliveryMode::Submit);
        write_drift.write = true;
        assert!(matches!(
            coordinator.replay_delivery(&write_drift),
            Err(TurnError::IdempotencyConflict { .. })
        ));
        let mut changed = request("key", DeliveryMode::Submit);
        changed.message = "different identity".to_owned();
        assert!(matches!(
            coordinator.replay_delivery(&changed),
            Err(TurnError::IdempotencyConflict { .. })
        ));
        assert!(
            coordinator
                .replay_delivery(&request("new", DeliveryMode::Submit))
                .unwrap()
                .is_none()
        );
        assert_eq!(coordinator.journal.entries.len(), before);
        coordinator.state = CoordinatorState::OutcomeUnknown;
        assert!(matches!(
            coordinator.replay_delivery(&request("key", DeliveryMode::Submit)),
            Ok(Some(DeliveryResult::Accepted(_)))
        ));
        assert!(matches!(
            coordinator.replay_delivery(&request("new", DeliveryMode::Submit)),
            Err(TurnError::OutcomeUnknown)
        ));
        assert_eq!(coordinator.journal.entries.len(), before);
    }

    #[test]
    fn same_key_drift_conflicts_after_terminal_and_exact_replay_returns_original() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}}));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        coordinator.next_event().unwrap().unwrap();
        assert!(
            coordinator
                .start_turn(request("key", DeliveryMode::Send))
                .unwrap()
                .replayed
        );
        let mut changed = request("key", DeliveryMode::Send);
        changed.message = "changed".to_owned();
        assert!(matches!(
            coordinator.start_turn(changed).unwrap_err(),
            TurnError::IdempotencyConflict { recorded, observed } if recorded != observed
        ));
    }

    /// One Turn running, with `sink` as the Run's durable diagnostic
    /// authority.
    fn running_turn_with(
        sink: &RecordingDiagnostics,
        requests: Vec<Value>,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        for request in requests {
            server.messages.push_back(request);
        }
        let mut coordinator = coordinator_in(
            server,
            ThreadAttach::Start,
            Box::new(sink.clone()),
            SessionSafetyPolicy::Standard,
        );
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        coordinator
    }

    fn input_request() -> Value {
        serde_json::json!({"id":9,"method":"item/tool/requestUserInput","params":{
            "threadId":"thread-1","turnId":"turn-1","questions":[{
                "id":"q","header":"Input","question":"Choose","isOther":true,
                "isSecret":false,"options":[{"label":"yes","description":"Use yes"}]
            }]
        }})
    }

    #[test]
    fn quarantine_rejects_unresolved_answers_and_uncertain_delivery_never_replays() {
        let answer = serde_json::json!({"answers":{"q":{"answers":["yes"]}}});
        let mut pending =
            running_turn_with(&RecordingDiagnostics::default(), vec![input_request()]);
        pending.next_event().unwrap();
        pending
            .mark_unknown(pending.active_operation_id.unwrap())
            .unwrap();
        assert_eq!(
            pending.respond(9, "answer".to_owned(), answer.clone()),
            Err(TurnError::OutcomeUnknown)
        );
        assert!(pending.server.responses.is_empty());
        assert_eq!(pending.state(), &CoordinatorState::OutcomeUnknown);

        let mut uncertain =
            running_turn_with(&RecordingDiagnostics::default(), vec![input_request()]);
        uncertain.next_event().unwrap();
        uncertain.server.fail_response = true;
        assert_eq!(
            uncertain.respond(9, "answer".to_owned(), answer.clone()),
            Err(TurnError::OutcomeUnknown)
        );
        assert!(matches!(
            uncertain.respond(9, "answer".to_owned(), answer),
            Ok(Some(_))
        ));
        assert_eq!(uncertain.server.responses.len(), 1);
        assert_eq!(uncertain.state(), &CoordinatorState::OutcomeUnknown);
        assert!(uncertain.pending_interactions().is_empty());
    }

    #[test]
    fn terminal_turn_durably_stales_pending_interactions() {
        let mut coordinator = running_turn_with(
            &RecordingDiagnostics::default(),
            vec![
                input_request(),
                serde_json::json!({"method":"turn/completed","params":{
                    "threadId":"thread-1","turn":{"id":"turn-1","status":"interrupted","items":[]}
                }}),
            ],
        );
        coordinator.next_event().unwrap();
        coordinator.next_event().unwrap();
        assert!(coordinator.pending_interactions().is_empty());
        assert_eq!(
            coordinator.respond(9, "late".to_owned(), serde_json::json!({})),
            Err(TurnError::InteractionStale { request_id: 9 })
        );
        assert!(coordinator.journal.entries.iter().any(|entry| matches!(entry,
            JournalEntry::ApprovalDecided {request_id:9, resolution, ..} if resolution["outcome"]=="stale")));
        assert!(coordinator.server.responses.is_empty());
    }

    #[test]
    fn file_change_kinds_preserve_pinned_add_delete_update_and_move() {
        let changes = serde_json::json!([
            {"path":"a","kind":{"type":"add"},"diff":"+a"},
            {"path":"b","kind":{"type":"delete"},"diff":"-b"},
            {"path":"c","kind":{"type":"update","move_path":"d"},"diff":"-c\n+d"}
        ]);
        let normalized =
            normalize_file_changes(changes.as_array().unwrap(), Path::new("/tmp/workspace"))
                .unwrap();
        assert_eq!(normalized[0].kind, "add");
        assert_eq!(normalized[1].kind, "delete");
        assert_eq!(normalized[2].kind, "update");
        assert_eq!(
            normalized[2].move_path,
            Some(LosslessPath::Utf8("d".to_owned()))
        );
        assert!(normalized[0].move_path.is_none());
    }

    #[test]
    fn a_foreign_thread_request_is_recorded_durably_and_never_answered() {
        let sink = RecordingDiagnostics::default();
        let mut coordinator = running_turn_with(
            &sink,
            vec![
                serde_json::json!({"id":41,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread-other","turnId":"turn-other","command":["rm","-rf","/secret"]}}),
                serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","phase":"final_answer","text":"mine"}]}}}),
            ],
        );

        assert!(coordinator.next_event().unwrap().is_none());
        assert_eq!(coordinator.state(), &CoordinatorState::Running);
        let ignored = coordinator.ignored_foreign_requests();
        assert_eq!(ignored.len(), 1);
        assert_eq!(ignored[0].request_id, 41);
        assert_eq!(ignored[0].thread_id.as_deref(), Some("thread-other"));
        assert!(coordinator.pending_interactions().is_empty());

        // This Run's own outcome is unaffected by the foreign traffic.
        let terminal = coordinator.next_event().unwrap().unwrap();
        assert_eq!(terminal.status, "completed");
        assert_eq!(
            terminal.final_response,
            Some(FinalResponse::Inline {
                text: "mine".to_owned()
            })
        );

        // Nothing was written back into the other Thread's pending request.
        assert!(
            !coordinator.server.responses.iter().any(|(id, _)| *id == 41),
            "a foreign Thread's request must never be answered by this connection"
        );

        // The observation reached the durable authority, with the Run lane it
        // belongs to and no request payload.
        let records = sink.records();
        assert_eq!(records.len(), 1);
        let recorded = &records[0];
        let observation = &recorded.ignored;
        assert_eq!(observation.request_id, 41);
        assert_eq!(observation.method, "item/commandExecution/requestApproval");
        assert_eq!(observation.thread_id.as_deref(), Some("thread-other"));
        assert_eq!(observation.turn_id.as_deref(), Some("turn-other"));
        assert_eq!(recorded.run_generation, 1);
        assert_eq!(recorded.server_key, "a".repeat(64));
        assert_eq!(recorded.server_epoch, 7);
        // `IgnoredForeignRequest` has no payload member at all, so there is
        // nothing for a writer to leak; this pins that.
        let serialized = serde_json::to_string(&serde_json::json!({
            "request_id": observation.request_id,
            "method": observation.method,
            "thread_id": observation.thread_id,
            "turn_id": observation.turn_id,
        }))
        .unwrap();
        for leaked in ["\"command\"", "-rf", "/secret"] {
            assert!(
                !serialized.contains(leaked),
                "the foreign request payload leaked into the observation: {serialized}"
            );
        }
    }

    #[test]
    fn an_unsupported_method_naming_a_foreign_thread_is_not_answered_either() {
        let sink = RecordingDiagnostics::default();
        let mut coordinator = running_turn_with(
            &sink,
            vec![
                serde_json::json!({"id":7,"method":"item/somethingElse","params":{"threadId":"thread-other"}}),
                serde_json::json!({"id":8,"method":"item/somethingElse","params":{"threadId":"thread-1"}}),
            ],
        );

        assert!(coordinator.next_event().unwrap().is_none());
        assert!(coordinator.next_event().unwrap().is_none());
        let answered: Vec<u64> = coordinator
            .server
            .responses
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            answered,
            vec![8],
            "only an unsupported method on this Run's own Thread earns a reply"
        );
        assert_eq!(sink.records().len(), 1);
    }

    /// SPEC-004: "A foreign-thread object cannot mutate run state."
    ///
    /// Sustained foreign traffic stops being recorded at the bound, and that
    /// is all it does: the Run keeps its Turn, keeps draining, and never
    /// quarantines itself over another Thread's noise.
    #[test]
    fn sustained_foreign_traffic_stops_being_recorded_without_costing_this_run_its_turn() {
        let sink = RecordingDiagnostics::default();
        let foreign = (0..=MAX_IGNORED_FOREIGN_REQUESTS)
            .map(|index| {
                serde_json::json!({"id":index,"method":"item/tool/requestUserInput","params":{"threadId":"thread-other","turnId":"turn-other"}})
            })
            .collect();
        let mut coordinator = running_turn_with(&sink, foreign);
        for _ in 0..=MAX_IGNORED_FOREIGN_REQUESTS {
            assert!(coordinator.next_event().unwrap().is_none());
        }
        assert_eq!(
            coordinator.ignored_foreign_requests().len(),
            MAX_IGNORED_FOREIGN_REQUESTS
        );
        assert_eq!(coordinator.undiagnosed_foreign_requests(), 1);
        // The in-memory bound is the durable bound too: the request past it
        // writes nothing, so a foreign Thread cannot grow the journal without
        // limit.
        assert_eq!(sink.records().len(), MAX_IGNORED_FOREIGN_REQUESTS);
        // The Run's own Turn is untouched, which is the whole point of the
        // bound being a recording limit rather than an outcome.
        assert_eq!(*coordinator.state(), CoordinatorState::Running);
        assert_eq!(coordinator.active_turn_id(), Some("turn-1"));
        assert!(
            !coordinator
                .server
                .responses
                .iter()
                .any(|(id, _)| *id == u64::try_from(MAX_IGNORED_FOREIGN_REQUESTS).unwrap()),
            "a dropped foreign request is still never answered"
        );
    }

    #[test]
    fn foreign_routing_fields_are_truncated_before_they_reach_the_authority() {
        let sink = RecordingDiagnostics::default();
        let huge = "\u{00e9}".repeat(4096);
        let mut coordinator = running_turn_with(
            &sink,
            vec![serde_json::json!({
                "id": 3,
                "method": "item/tool/requestUserInput",
                "params": {"threadId": huge.clone(), "turnId": huge}
            })],
        );
        assert!(coordinator.next_event().unwrap().is_none());

        let records = sink.records();
        assert_eq!(records.len(), 1);
        let thread_id = records[0].ignored.thread_id.as_deref().unwrap();
        assert_eq!(thread_id.chars().count(), MAX_FOREIGN_FIELD_CHARS);
        assert_eq!(
            records[0]
                .ignored
                .turn_id
                .as_deref()
                .unwrap()
                .chars()
                .count(),
            MAX_FOREIGN_FIELD_CHARS
        );
    }

    /// A profile diagnostic journal that refuses the write is profile
    /// degradation, never this Run's outcome.
    ///
    /// Durable first, in memory second: nothing is remembered that was not
    /// written.  But the Run keeps its Turn — the alternative would let an
    /// unwritable profile root, driven by another Thread's traffic,
    /// quarantine a Run that has done nothing wrong.
    #[test]
    fn an_observation_that_cannot_be_recorded_is_never_only_remembered() {
        let sink = RecordingDiagnostics {
            broken: true,
            ..RecordingDiagnostics::default()
        };
        let mut coordinator = running_turn_with(
            &sink,
            vec![serde_json::json!({
                "id": 5,
                "method": "item/tool/requestUserInput",
                "params": {"threadId": "thread-other", "turnId": "turn-other"}
            })],
        );

        assert!(coordinator.next_event().unwrap().is_none());
        assert!(coordinator.ignored_foreign_requests().is_empty());
        assert_eq!(coordinator.undiagnosed_foreign_requests(), 1);
        assert_eq!(*coordinator.state(), CoordinatorState::Running);
        assert_eq!(coordinator.active_turn_id(), Some("turn-1"));
        assert!(
            !coordinator.server.responses.iter().any(|(id, _)| *id == 5),
            "a failed recording still never answers the foreign Thread"
        );
    }

    #[test]
    fn interactions_wait_and_resume_without_leaking_payload() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({"id":9,"method":"item/tool/requestUserInput","params":{"threadId":"thread-1","turnId":"turn-1","questions":[{"id":"q","isSecret":true}]}}));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        assert!(coordinator.next_event().unwrap().is_none());
        assert_eq!(coordinator.state(), &CoordinatorState::WaitingInteraction);
        assert_eq!(coordinator.pending_interactions()[0].request_id, 9);
        coordinator
            .respond(
                9,
                "response-1".to_owned(),
                serde_json::json!({"answers":{"q":{"answers":["secret"]}}}),
            )
            .unwrap();
        assert_eq!(coordinator.state(), &CoordinatorState::Running);
        assert!(
            coordinator
                .journal()
                .entries
                .iter()
                .all(|entry| !format!("{entry:?}").contains("String(\"secret\")"))
        );
    }

    #[test]
    fn user_input_answers_must_match_the_presented_options() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({
            "id": 9,
            "method": "item/tool/requestUserInput",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "questions": [{
                    "id": "choice",
                    "isOther": false,
                    "options": [{"label":"alpha"}, {"label":"beta"}]
                }]
            }
        }));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        assert!(coordinator.next_event().unwrap().is_none());

        assert_eq!(
            coordinator
                .respond(
                    9,
                    "invalid-option".to_owned(),
                    serde_json::json!({"answers":{"choice":{"answers":["gamma"]}}}),
                )
                .unwrap_err(),
            TurnError::InteractionResponseInvalid { request_id: 9 }
        );
        assert_eq!(coordinator.state(), &CoordinatorState::WaitingInteraction);
        assert!(coordinator.server.responses.is_empty());

        assert!(
            coordinator
                .respond(
                    9,
                    "valid-option".to_owned(),
                    serde_json::json!({"answers":{"choice":{"answers":["beta"]}}}),
                )
                .unwrap()
                .is_some()
        );
        assert_eq!(coordinator.state(), &CoordinatorState::Running);
    }

    #[test]
    fn file_approval_binds_the_latest_durable_snapshot_and_stales_on_update() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({
            "method": "item/started",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "item": {
                    "id": "item-1",
                    "type": "fileChange",
                    "changes": [{"path":"src/lib.rs","kind":{"type":"update","move_path":null},"diff":"@@ -1 +1 @@\n-old\n+new\n"}]
                }
            }
        }));
        server.messages.push_back(serde_json::json!({
            "id": 9,
            "method": "item/fileChange/requestApproval",
            "params": {"threadId":"thread-1","turnId":"turn-1","itemId":"item-1"}
        }));
        server.messages.push_back(serde_json::json!({
            "method": "item/fileChange/patchUpdated",
            "params": {
                "threadId":"thread-1",
                "turnId":"turn-1",
                "itemId":"item-1",
                "changes":[{"path":"src/lib.rs","kind":{"type":"update","move_path":null},"diff":"@@ -1 +1 @@\n-old\n+newer\n"}]
            }
        }));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        assert!(coordinator.next_event().unwrap().is_none());
        assert!(coordinator.next_event().unwrap().is_none());
        let opened = coordinator.pending_interactions()[0].clone();
        assert_eq!(opened.file_snapshot_revision, Some(0));
        assert_eq!(
            opened.file_snapshot_sha256.as_deref().map(str::len),
            Some(64)
        );
        assert!(coordinator.next_event().unwrap().is_none());
        assert!(coordinator.pending_interactions().is_empty());
        assert_eq!(
            coordinator
                .respond(
                    9,
                    "response".to_owned(),
                    serde_json::json!({"decision":"accept_once"})
                )
                .unwrap_err(),
            TurnError::InteractionStale { request_id: 9 }
        );
        assert!(
            coordinator
                .journal()
                .entries
                .iter()
                .any(|entry| matches!(entry, JournalEntry::FileChangeSnapshot { revision: 1, .. }))
        );
    }

    #[test]
    fn reviewer_policy_denies_interactions_without_waiting_or_retaining_payload() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({
            "id": 9,
            "method": "item/fileChange/requestApproval",
            "params": {"threadId":"thread-1","turnId":"turn-1","patch":"sentinel-secret-value"}
        }));
        let mut coordinator = coordinator_in(
            server,
            ThreadAttach::Start,
            Box::new(RecordingDiagnostics::default()),
            SessionSafetyPolicy::ReviewerReadOnly,
        );
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();

        assert!(coordinator.next_event().unwrap().is_none());
        assert_eq!(coordinator.state(), &CoordinatorState::Running);
        assert!(coordinator.pending_interactions().is_empty());
        assert_eq!(coordinator.server.responses.len(), 1);
        assert_eq!(coordinator.server.responses[0].0, 9);
        assert!(!format!("{:?}", coordinator.journal().entries).contains("sentinel-secret-value"));
    }

    #[test]
    fn final_response_uses_authoritative_phase_and_ignores_foreign_items() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        for (thread, phase, text) in [
            ("thread-1", Value::Null, "fallback"),
            ("child", Value::String("final_answer".to_owned()), "child"),
            (
                "thread-1",
                Value::String("commentary".to_owned()),
                "commentary",
            ),
            (
                "thread-1",
                Value::String("final_answer".to_owned()),
                "final",
            ),
        ] {
            server.messages.push_back(serde_json::json!({"method":"item/completed","params":{"threadId":thread,"turnId":"turn-1","item":{"type":"agentMessage","status":"completed","phase":phase,"text":text}}}));
        }
        server.replies.push_back(Ok(serde_json::json!({"thread":{"turns":[{"id":"turn-1","items":[
            {"type":"agentMessage","status":"completed","phase":null,"text":"fallback"},
            {"type":"agentMessage","status":"completed","phase":"commentary","text":"commentary"},
            {"type":"agentMessage","status":"completed","phase":"final_answer","text":"final"}
        ]}]}})));
        server.messages.push_back(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","usage":{"inputTokens":3}}}}));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        for _ in 0..4 {
            assert!(coordinator.next_event().unwrap().is_none());
        }
        let terminal = coordinator.next_event().unwrap().unwrap();
        assert_eq!(
            terminal.final_response,
            Some(FinalResponse::Inline {
                text: "final".to_owned()
            })
        );
        assert_eq!(terminal.usage.unwrap()["inputTokens"], 3);
    }

    #[test]
    fn a_foreign_completed_item_without_a_turn_id_cannot_abandon_this_run() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread-other",
                "item": {"type": "agentMessage", "status": "completed"}
            }
        }));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();

        assert!(coordinator.next_event().unwrap().is_none());
        assert_eq!(coordinator.state(), &CoordinatorState::Running);
        assert_eq!(coordinator.active_turn_id(), Some("turn-1"));
        assert!(
            !coordinator
                .journal()
                .entries
                .iter()
                .any(|entry| matches!(entry, JournalEntry::OutcomeUnknown { .. })),
            "foreign malformed traffic quarantined this Run"
        );
    }

    #[test]
    fn large_final_response_is_preserved_as_a_digest_checked_artifact() {
        let text = "x".repeat(MAX_INLINE_FINAL_RESPONSE_BYTES + 1);
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","status":"completed","phase":"final_answer","text":text}]}}}));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        let terminal = coordinator.next_event().unwrap().unwrap();
        let FinalResponse::Artifact {
            artifact_id,
            byte_length,
            sha256,
            created_at,
        } = terminal.final_response.unwrap()
        else {
            panic!("large response must use an artifact");
        };
        let stored = &coordinator.artifacts().values[&artifact_id];
        assert_eq!(byte_length, stored.len() as u64);
        assert_eq!(sha256, sha256_hex(stored));
        assert_eq!(
            artifact_id.get_version_num(),
            7,
            "the artifact contract names artifacts with a UUIDv7"
        );
        assert!(
            created_at.ends_with('Z') && created_at.len() == 27,
            "artifact metadata needs a microsecond UTC instant: {created_at}"
        );
    }

    #[test]
    fn malformed_active_event_quarantines_but_duplicate_known_terminal_does_not() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({"params":{}}));
        let mut malformed_coordinator = coordinator(server);
        malformed_coordinator
            .start_turn(request("malformed", DeliveryMode::Submit))
            .unwrap();
        assert_eq!(
            malformed_coordinator.next_event().unwrap_err(),
            TurnError::CorrelationMismatch
        );
        assert_eq!(
            malformed_coordinator.state(),
            &CoordinatorState::OutcomeUnknown
        );

        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        let completed = serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}});
        server.messages.push_back(completed.clone());
        server.messages.push_back(completed);
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("duplicate", DeliveryMode::Submit))
            .unwrap();
        coordinator.next_event().unwrap().unwrap();
        assert_eq!(
            coordinator.next_event().unwrap_err(),
            TurnError::DuplicateTerminal
        );
        assert_eq!(coordinator.state(), &CoordinatorState::Idle);
    }

    #[test]
    fn image_snapshot_detects_content_changes_before_turn_effects() {
        let root = PathBuf::from("/tmp").join(format!("dg-turn-image-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("image.png");
        std::fs::write(&path, b"before").unwrap();
        let snapshot = ImageSnapshot::capture(&path, ImageDetail::High).unwrap();
        std::fs::write(&path, b"after").unwrap();
        assert_eq!(snapshot.verify().unwrap_err(), TurnError::OutcomeUnknown);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn correlation_effort_and_fork_status_fail_closed() {
        let mut coordinator = coordinator(FakeServer::default());
        let mut unsupported = request("key", DeliveryMode::Submit);
        unsupported.effort = Some("extreme".to_owned());
        assert!(matches!(
            coordinator.start_turn(unsupported).unwrap_err(),
            TurnError::EffortUnsupported { ref requested, .. } if requested == "extreme"
        ));
        assert!(forkable_status("completed"));
        assert!(!forkable_status("interrupted"));
        assert!(!forkable_status("failed"));
    }

    #[test]
    fn resume_and_fork_install_a_new_thread_generation_before_turn_start() {
        for (attach, expected_method, resulting_thread) in [
            (
                ThreadAttach::Resume {
                    thread_id: "thread-old".to_owned(),
                },
                "thread/resume",
                "thread-old",
            ),
            (
                ThreadAttach::Fork {
                    source_thread_id: "thread-source".to_owned(),
                    last_turn_id: "turn-terminal".to_owned(),
                },
                "thread/fork",
                "thread-fork",
            ),
        ] {
            let mut server = FakeServer::default();
            server
                .replies
                .push_back(Ok(serde_json::json!({"thread":{"id":resulting_thread}})));
            server
                .replies
                .push_back(Ok(serde_json::json!({"turn":{"id":"turn-new"}})));
            let mut coordinator = coordinator_with_attach(server, attach);
            coordinator
                .start_turn(request("key", DeliveryMode::Submit))
                .unwrap();
            assert_eq!(coordinator.server.calls[2].0, expected_method);
            assert!(matches!(
                coordinator.journal().entries[2],
                JournalEntry::ThreadBound {
                    thread_generation: 1,
                    ..
                }
            ));
        }
    }

    #[test]
    fn lost_turn_response_records_outcome_unknown_and_forbids_replay() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server.replies.push_back(Err(TurnError::transport(
            TransportStage::Read,
            "response lost",
        )));
        let mut coordinator = coordinator(server);
        // docs/specs/README.md: "`TRANSPORT_FAILURE` is retryable only when the operation
        // made no external write ... Any uncertain acceptance emits `false`."
        // `turn/start` was already on the wire, so the fault reported is the
        // same `outcome_unknown` the ledger has just recorded, not a retryable
        // transport hiccup a caller would reissue.
        assert_eq!(
            coordinator
                .start_turn(request("key", DeliveryMode::Submit))
                .unwrap_err(),
            TurnError::OutcomeUnknown
        );
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        assert!(matches!(
            coordinator.journal().entries.last(),
            Some(JournalEntry::OutcomeUnknown { .. })
        ));
        assert_eq!(
            coordinator
                .start_turn(request("key", DeliveryMode::Submit))
                .unwrap_err(),
            TurnError::OutcomeUnknown
        );
    }

    #[test]
    fn read_only_reconciliation_restores_terminal_unknown_to_paused_without_replay() {
        let mut server = FakeServer::default();
        server.replies.push_back(Ok(serde_json::json!({
            "thread": {
                "id": "thread-1",
                "turns": [{"id": "turn-1", "status": "completed"}]
            }
        })));
        let mut coordinator = coordinator(server);
        coordinator.restore_durable_state(
            RunLifecycle::OutcomeUnknown,
            Some("thread-1".to_owned()),
            None,
            Some("turn-1".to_owned()),
        );
        assert!(coordinator.reconcile_history().unwrap());
        assert_eq!(coordinator.state(), &CoordinatorState::Paused);
        assert_eq!(coordinator.server.calls[2].0, "thread/read");
        assert!(coordinator
            .journal()
            .entries
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Reconciliation { observed_status: Some(status), .. } if status == "completed")));
        assert!(
            !coordinator
                .server
                .calls
                .iter()
                .any(|(method, _)| method == "turn/start" || method == "thread/resume")
        );
    }

    #[test]
    fn reconciliation_without_the_unknown_turn_stays_outcome_unknown() {
        let mut server = FakeServer::default();
        server.replies.push_back(Ok(serde_json::json!({
            "thread": {"id": "thread-1", "turns": []}
        })));
        let mut coordinator = coordinator(server);
        coordinator.restore_durable_state(
            RunLifecycle::OutcomeUnknown,
            Some("thread-1".to_owned()),
            None,
            Some("turn-1".to_owned()),
        );
        assert!(!coordinator.reconcile_history().unwrap());
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        assert!(matches!(
            coordinator.journal().entries.last(),
            Some(JournalEntry::OutcomeUnknown { .. })
        ));
    }

    #[test]
    fn send_waits_for_terminal_and_submit_returns_after_acceptance() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}}));
        let mut send_coordinator = coordinator(server);
        assert!(matches!(
            send_coordinator
                .deliver(request("send", DeliveryMode::Send))
                .unwrap(),
            DeliveryResult::Terminal(_)
        ));

        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-2"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-2"}})));
        let mut submit_coordinator = coordinator(server);
        assert!(matches!(
            submit_coordinator
                .deliver(request("submit", DeliveryMode::Submit))
                .unwrap(),
            DeliveryResult::Accepted(_)
        ));
    }

    #[test]
    fn streamed_history_keeps_only_the_wanted_turn() {
        let mut sink = ThreadTurnSink::new("turn-9");
        let mut body = String::from("{\"thread\":{\"id\":\"t\",\"turns\":[");
        for index in 0..3 {
            body.push_str(&format!(
                "{{\"id\":\"decoy-{index}\",\"status\":\"completed\",\"items\":[{{\"type\":\"agentMessage\",\"text\":\"{}\"}}]}},",
                "d".repeat(4096)
            ));
        }
        body.push_str(
            "{\"id\":\"turn-9\",\"status\":\"completed\",\"items\":[{\"type\":\"agentMessage\",\"phase\":\"final_answer\",\"text\":\"kept\"}]}]}}",
        );
        for chunk in body.as_bytes().chunks(7) {
            sink.accept(chunk).unwrap();
        }
        let streamed = sink.streamed_bytes();
        let items = sink.into_items().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "kept");
        assert!(streamed > 12_000, "the decoys were streamed, not skipped");
    }

    #[test]
    fn shared_journal_stamps_acceptance_and_response_at_their_durable_boundaries() {
        let root = PathBuf::from("/tmp").join(format!("dg-stamped-journal-{}", Uuid::now_v7()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let state_root = root.as_path();
        for path in [
            state_root.join("runtime"),
            state_root.join("runtime/locks"),
            state_root.join("run"),
            state_root.join("run/recovery"),
        ] {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(state_root.join("run/audit.jsonl"))
            .unwrap();
        let workspace_id = "a".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        crate::writer::WriterStore::initialize_layout(state_root, &workspace_id, uid).unwrap();
        let run_id = Uuid::now_v7();
        let ledger = Arc::new(Mutex::new(
            Ledger::open(state_root.join("run"), run_id).unwrap(),
        ));
        let mut journal = SharedLedgerJournal::new(Arc::clone(&ledger), 1).with_event_authority(
            state_root,
            &workspace_id,
            uid,
            &"b".repeat(64),
            7,
        );
        let operation_id = Uuid::now_v7();
        journal
            .append_and_sync(JournalEntry::ThreadBound {
                operation_id,
                thread_id: "thread-1".to_owned(),
                thread_generation: 1,
                effective_policy: crate::domain::EffectivePolicy {
                    access: crate::domain::Access::Unknown,
                    verification: crate::domain::PolicyVerification::Unverified,
                    policy_epoch: crate::domain::PolicyEpoch(1),
                    thread_generation: Some(1),
                    server_epoch: Some(7),
                    writer_generation: None,
                },
            })
            .unwrap();
        journal
            .append_and_sync(JournalEntry::TurnAccepted {
                operation_id,
                thread_id: "thread-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                effort: "medium".to_owned(),
                effective_policy: crate::domain::EffectivePolicy {
                    access: crate::domain::Access::Unknown,
                    verification: crate::domain::PolicyVerification::Unverified,
                    policy_epoch: crate::domain::PolicyEpoch(1),
                    thread_generation: Some(1),
                    server_epoch: Some(7),
                    writer_generation: None,
                },
                controller: Some(crate::domain::ControllerIdentity {
                    controller_id: Uuid::now_v7(),
                    kind: crate::domain::ControllerKind::HumanCli,
                    instance_id: "test".to_owned(),
                    subject_id: None,
                    generation: 1,
                }),
            })
            .unwrap();
        journal
            .append_and_sync(JournalEntry::Terminal {
                thread_id: "thread-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                status: "completed".to_owned(),
                effort: "medium".to_owned(),
                usage: None,
                final_response: Some(FinalResponse::Inline {
                    text: "finished".to_owned(),
                }),
            })
            .unwrap();
        let locked = ledger.lock().unwrap();
        let records = locked.durable_records().unwrap();
        let projected: Vec<_> = records
            .iter()
            .filter_map(|record| {
                record
                    .client_projection()
                    .map(|projection| (record.sequence(), projection))
            })
            .collect();
        assert_eq!(projected.len(), 3);
        for (sequence, projection) in &projected {
            assert_eq!(projection.stamp.run_state_revision, *sequence);
            assert_eq!(projection.stamp.writer_state_revision, 0);
            assert_eq!(projection.record.server_epoch, 7);
        }
        assert!(projected[0].0 < projected[1].0 && projected[1].0 < projected[2].0);
        assert_eq!(records.last().unwrap().kind(), AuditKind::TurnTerminal);
        drop(locked);
        drop(journal);
        drop(ledger);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ledger_journal_fsyncs_intent_before_external_effect_state() {
        let run_id = Uuid::now_v7();
        let root = PathBuf::from("/tmp").join(format!("dg-turn-ledger-{}", Uuid::now_v7()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        {
            let mut ledger = Ledger::open(&root, run_id).unwrap();
            let mut journal = LedgerTurnJournal::new(&mut ledger, 1);
            journal
                .append_and_sync(JournalEntry::Intent(OperationIntent {
                    operation_id: Uuid::now_v7(),
                    idempotency_key: "key".to_owned(),
                    request_sha256: "b".repeat(64),
                    run_generation: 1,
                    server_key: "a".repeat(64),
                    server_epoch: 1,
                    provisional_thread: true,
                    accepted_input: None,
                }))
                .unwrap();
        }
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        LedgerTurnJournal::new(&mut ledger, 1)
            .append_and_sync(JournalEntry::FileChangeSnapshot {
                thread_id: "thread".to_owned(),
                turn_id: "turn".to_owned(),
                item_id: "item".to_owned(),
                revision: 0,
                snapshot_sha256: "c".repeat(64),
            })
            .unwrap();
        drop(ledger);
        let ledger = Ledger::open(&root, run_id).unwrap();
        assert_eq!(ledger.durable_records().unwrap().len(), 2);
        assert_eq!(
            ledger.durable_records().unwrap()[1].kind(),
            AuditKind::AppServerNotification
        );
        assert_eq!(
            ledger.durable_records().unwrap()[0].kind(),
            AuditKind::IdempotencyReserved
        );
        drop(ledger);
        std::fs::remove_dir_all(root).unwrap();
    }
}
